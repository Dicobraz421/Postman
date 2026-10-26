//! Настройки ящика по JMAP: автоответ «Нет на месте» и заполненность ящика (S4-05), правила почты (S4-07).
//! Договор (`Request`, `Reply`) — `postman_core::settings`; скрипт правил — `postman_core::rules`.
//!
//! Stalwart держит активным один скрипт Sieve, а автоответ — это его скрипт `vacation` (`docs/STALWART.md`).
//! Поэтому при включённых правилах активен наш скрипт, а автоответ подключён к нему (`include`); `isEnabled`
//! сервера тогда `false`, и клиент считает автоответ включённым по нашему скрипту (`settings::effective`).
//!
//! Автоответ Postman (S5-29): тексты для своих и внешних, «только контактам», «не отвечать повторно» — свой скрипт
//! `vacation` поверх того, что пишет `VacationResponse/set` (`postman_core::autoreply`), и событие «Вне офиса» в
//! календаре.

use postman_core::autoreply::{self, AutoReply, Stored};
use postman_core::model::VacationResponse;
use postman_core::rules::{self, Rule, SCRIPT_NAME, Script, VACATION_SCRIPT};
use postman_core::settings::{Area, AutoReplySave, Foreign, RulesState, Usage, effective};
pub use postman_core::settings::{Reply, Request};
use postman_jmap::{Activate, CALENDARS, Client, QUOTA, Quota, SIEVE, SaveError, SieveScript, VACATION_RESPONSE};

use crate::convert::{FromJmap, ToJmap};

/// Скрипты Sieve на сервере, разобранные.
#[derive(Debug, Clone, Default)]
struct Sieve {
    /// Наш скрипт (разобран).
    ours: Option<(SieveScript, Script)>,
    /// Скрипт с нашим именем, изменённый вне Postman.
    ours_edited: Option<SieveScript>,
    vacation_active: bool,
    /// Скрипт `vacation` и его текст (свой — с заголовком `autoreply::HEADER`).
    vacation: Option<(SieveScript, String)>,
    foreign: Vec<Foreign>,
}

impl Sieve {
    fn ours_active(&self) -> bool {
        self.ours.as_ref().is_some_and(|(s, _)| s.is_active)
    }

    /// Автоответ работает через наш активный скрипт.
    fn vacation_in_rules(&self) -> bool {
        self.ours.as_ref().is_some_and(|(s, script)| s.is_active && script.vacation)
    }

    /// Настройки автоответа Postman в скрипте `vacation`.
    fn stored(&self) -> Option<Stored> {
        self.vacation.as_ref().and_then(|(_, text)| autoreply::parse(text))
    }

    fn state(&self) -> RulesState {
        RulesState {
            rules: self.ours.as_ref().map(|(_, s)| s.rules.clone()).unwrap_or_default(),
            active: self.ours_active(),
            foreign: self.foreign.clone(),
        }
    }
}

/// Квота почты в байтах: из квот с пределом, которые касаются писем, — самая заполненная.
pub fn mail_usage(quotas: &[Quota]) -> Option<Usage> {
    quotas
        .iter()
        .filter(|q| q.resource_type == "octets" && q.hard_limit > 0)
        .filter(|q| q.types.is_empty() || q.types.iter().any(|t| t == "Mail"))
        .map(|q| Usage { used: q.used, limit: q.hard_limit })
        .max_by(|a, b| (u128::from(a.used) * u128::from(b.limit)).cmp(&(u128::from(b.used) * u128::from(a.limit))))
}

pub async fn handle(client: &Client, request: Request, notify: &impl Fn(Reply)) {
    match request {
        Request::Load => {
            let sieve = read_sieve(client).await;
            if let Err(e) = &sieve {
                notify(Reply::Failed { area: Area::Rules, error: e.to_string() });
            }
            let sieve = sieve.unwrap_or_default();
            notify(vacation(client, &sieve).await);
            if client.has(SIEVE) {
                notify(Reply::AutoReply(sieve.stored().map(|s| s.reply)));
            }
            notify(if client.has(SIEVE) { Reply::Rules(sieve.state()) } else { Reply::Unsupported(Area::Rules) });
            quota(client, notify).await;
        }
        Request::SetVacation(v) => match set_vacation(client, v).await {
            Ok((v, sieve)) => {
                notify(Reply::VacationSaved(v));
                if client.has(SIEVE) {
                    notify(Reply::Rules(sieve.state()));
                }
            }
            Err(e) => notify(Reply::Failed { area: Area::Vacation, error: e.to_string() }),
        },
        Request::SetAutoReply(save) => match set_autoreply(client, *save).await {
            Ok((v, sieve, reply)) => {
                notify(Reply::VacationSaved(v));
                notify(Reply::AutoReply(reply));
                if client.has(SIEVE) {
                    notify(Reply::Rules(sieve.state()));
                }
            }
            Err(e) => notify(Reply::Failed { area: Area::Vacation, error: e.to_string() }),
        },
        Request::Quota => quota(client, notify).await,
        Request::Rules => notify(match read_sieve(client).await {
            Ok(s) if client.has(SIEVE) => Reply::Rules(s.state()),
            Ok(_) => Reply::Unsupported(Area::Rules),
            Err(e) => Reply::Failed { area: Area::Rules, error: e.to_string() },
        }),
        Request::SaveRules { rules, replace } => {
            let reply = save_rules(client, rules, replace).await;
            let saved = matches!(reply, Reply::RulesSaved(_));
            notify(reply);
            // Автоответ мог переехать в скрипт правил или обратно: экран — заново.
            if saved && let Ok(sieve) = read_sieve(client).await {
                notify(vacation(client, &sieve).await);
            }
        }
    }
}

/// Автоответ с учётом правил.
async fn vacation(client: &Client, sieve: &Sieve) -> Reply {
    if !client.has(VACATION_RESPONSE) {
        return Reply::Unsupported(Area::Vacation);
    }
    match get_vacation(client).await {
        Ok(v) => Reply::Vacation(with_stored(effective(v, sieve.vacation_in_rules()), sieve.stored().as_ref())),
        Err(e) => Reply::Failed { area: Area::Vacation, error: e.to_string() },
    }
}

/// Скрипты сервера: наш разобран, чужие — текстом; `vacation` — только активность. Без `sieve` — пусто.
async fn read_sieve(client: &Client) -> postman_jmap::Result<Sieve> {
    let mut sieve = Sieve::default();
    if !client.has(SIEVE) {
        return Ok(sieve);
    }
    for script in client.sieve_scripts().await? {
        let name = script.name.clone().unwrap_or_default();
        if name == VACATION_SCRIPT {
            sieve.vacation_active = script.is_active;
            let text = client.sieve_text(&script).await?;
            sieve.vacation = Some((script, text));
            continue;
        }
        let text = client.sieve_text(&script).await?;
        match (name == SCRIPT_NAME).then(|| rules::parse(&text)).flatten() {
            Some(parsed) => sieve.ours = Some((script, parsed)),
            None => {
                if name == SCRIPT_NAME {
                    sieve.ours_edited = Some(script.clone());
                }
                sieve.foreign.push(Foreign { name, text, active: script.is_active });
            }
        }
    }
    Ok(sieve)
}

/// Записать автоответ. При включённых правилах сервер не должен выключить наш скрипт: автоответ пишется
/// выключенным, а включается строкой `include` в нашем скрипте.
async fn set_vacation(client: &Client, v: VacationResponse) -> postman_jmap::Result<(VacationResponse, Sieve)> {
    let sieve = read_sieve(client).await?;
    match &sieve.ours {
        Some((ours, script)) if ours.is_active && script.has_enabled() => {
            let on = v.is_enabled;
            put_vacation(client, &VacationResponse { is_enabled: false, ..v }).await?;
            let script = Script { rules: script.rules.clone(), vacation: on };
            client.save_sieve(Some(&ours.id), SCRIPT_NAME, &rules::generate(&script), Activate::This).await.map_err(save_error)?;
        }
        _ => put_vacation(client, &v).await?,
    }
    let sieve = read_sieve(client).await?;
    let v = effective(get_vacation(client).await?, sieve.vacation_in_rules());
    Ok((v, sieve))
}

/// Период из своего скрипта, если сервер после записи скрипта его не вернул ([проверить на стенде]).
fn with_stored(v: VacationResponse, stored: Option<&Stored>) -> VacationResponse {
    let Some(s) = stored else { return v };
    VacationResponse { from_date: v.from_date.or_else(|| s.from.clone()), to_date: v.to_date.or_else(|| s.to.clone()), ..v }
}

/// Автоответ Postman: `VacationResponse/set` (как раньше), «Вне офиса» в календаре, затем свой скрипт `vacation`
/// с теми же включением и периодом.
async fn set_autoreply(client: &Client, save: AutoReplySave) -> postman_jmap::Result<(VacationResponse, Sieve, Option<AutoReply>)> {
    let primary = save.reply.primary();
    let text = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    let v = VacationResponse {
        subject: text(&primary.subject),
        text_body: Some(primary.body.trim_end().to_owned()).filter(|s| !s.is_empty()),
        html_body: None,
        ..save.vacation.clone()
    };
    let (v, sieve) = set_vacation(client, v).await?;
    if !client.has(SIEVE) {
        return Ok((v, sieve, None));
    }
    let previous = sieve.stored();
    let ooo = out_of_office(client, &save, &v, previous.as_ref().and_then(|p| p.ooo.as_deref())).await;
    let stored = Stored { reply: save.reply.clone(), from: v.from_date.clone(), to: v.to_date.clone(), ooo };
    let script = autoreply::script(&stored, &save.audience);
    let id = sieve.vacation.as_ref().map(|(s, _)| s.id.clone());
    client.save_sieve(id.as_deref(), VACATION_SCRIPT, &script, Activate::Keep).await.map_err(save_error)?;
    let sieve = read_sieve(client).await?;
    let v = with_stored(effective(get_vacation(client).await?, sieve.vacation_in_rules()), sieve.stored().as_ref());
    let reply = sieve.stored().map(|s| s.reply);
    Ok((v, sieve, reply))
}

/// «Вне офиса»: прежнее событие Postman — прочь, новое — на период включённого автоответа. Ошибки календаря
/// автоответ не останавливают (в журнал). Итог — uid нового события.
async fn out_of_office(client: &Client, save: &AutoReplySave, v: &VacationResponse, old: Option<&str>) -> Option<String> {
    if !client.has(CALENDARS) {
        return None;
    }
    if let Some(uid) = old {
        match client.event_by_uid(uid).await {
            Ok(Some(e)) => {
                if let Some(id) = e["id"].as_str()
                    && let Err(e) = client.destroy_event(id, false).await
                {
                    tracing::warn!("out of office: old event is not removed: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("out of office: {e}"),
        }
    }
    let (Some(from), Some(to)) = (v.from_date.as_deref(), v.to_date.as_deref()) else { return None };
    if !save.reply.out_of_office || !v.is_enabled {
        return None;
    }
    let tz = jiff::tz::TimeZone::get(&save.zone).unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let uid = postman_core::tasks::new_uid(jiff::Timestamp::now().as_nanosecond().unsigned_abs());
    let calendar = match client.calendars().await {
        Ok(mut cals) => {
            cals.sort_by_key(|c| (!c.is_visible, c.sort_order));
            cals.into_iter().next()?
        }
        Err(e) => {
            tracing::warn!("out of office: {e}");
            return None;
        }
    };
    let event = autoreply::out_of_office(&save.ooo_title, from, to, &tz, &uid, &calendar.id)?;
    match client.create_event(event, false).await {
        Ok(_) => Some(uid),
        Err(e) => {
            tracing::warn!("out of office: event is not created: {e}");
            None
        }
    }
}

fn save_error(e: SaveError) -> postman_jmap::Error {
    match e {
        SaveError::Invalid(m) | SaveError::Other(m) => postman_jmap::Error::Protocol(m),
    }
}

async fn save_rules(client: &Client, rules: Vec<Rule>, replace: bool) -> Reply {
    let failed = |e: String| Reply::Failed { area: Area::Rules, error: e };
    if !client.has(SIEVE) {
        return Reply::Unsupported(Area::Rules);
    }
    let sieve = match read_sieve(client).await {
        Ok(s) => s,
        Err(e) => return failed(e.to_string()),
    };
    if !replace && let Some(conflict) = conflict(&sieve) {
        return conflict;
    }
    let target = sieve.ours.as_ref().map(|(s, _)| s).or(sieve.ours_edited.as_ref());
    let was_active = target.is_some_and(|s| s.is_active);
    let vacation_on = sieve.vacation_active || sieve.vacation_in_rules();
    let enabled = rules.iter().any(|r| r.enabled);
    // Автоответ — в скрипт правил, если тот будет активным.
    let script = Script { vacation: vacation_on && enabled, rules };
    let result = if script.rules.is_empty() {
        // Правил нет — скрипт не нужен (активный сервер не удаляет: сначала выключить).
        match target {
            Some(t) => {
                let off = if was_active { client.activate_sieve(None).await } else { Ok(()) };
                match off {
                    Ok(()) => client.destroy_sieve(&t.id).await,
                    Err(e) => Err(e),
                }
                .map_err(|e| SaveError::Other(e.to_string()))
            }
            None => Ok(()),
        }
    } else {
        let activate = match (enabled, was_active) {
            (true, _) => Activate::This,
            (false, true) => Activate::None,
            (false, false) => Activate::Keep,
        };
        let id = target.map(|t| t.id.as_str());
        client.save_sieve(id, SCRIPT_NAME, &rules::generate(&script), activate).await.map(|_| ())
    };
    match result {
        Ok(()) => {}
        Err(SaveError::Invalid(m)) => return Reply::RulesInvalid(m),
        Err(SaveError::Other(m)) => return failed(m),
    }
    // Правила выключены, а автоответ был в их скрипте — снова скрипт `vacation`.
    if !enabled
        && was_active
        && vacation_on
        && let Err(e) = enable_vacation(client).await
    {
        return failed(e.to_string());
    }
    match read_sieve(client).await {
        Ok(s) => Reply::RulesSaved(s.state()),
        Err(e) => failed(e.to_string()),
    }
}

/// Чужой скрипт мешает записи без подтверждения: изменённый наш — сначала (его текст заменится).
fn conflict(sieve: &Sieve) -> Option<Reply> {
    if sieve.ours_edited.is_some() {
        return Some(Reply::RulesConflict { name: SCRIPT_NAME.into(), overwrite: true });
    }
    sieve.foreign.iter().find(|f| f.active).map(|f| Reply::RulesConflict { name: f.name.clone(), overwrite: false })
}

/// Автоответ сервера в нейтральной модели (путь JMAP).
async fn get_vacation(client: &Client) -> postman_jmap::Result<VacationResponse> {
    client.vacation().await.map(VacationResponse::from_jmap)
}

async fn put_vacation(client: &Client, v: &VacationResponse) -> postman_jmap::Result<()> {
    client.set_vacation(&v.to_jmap()).await
}

async fn enable_vacation(client: &Client) -> postman_jmap::Result<()> {
    let v = get_vacation(client).await?;
    put_vacation(client, &VacationResponse { is_enabled: true, ..v }).await
}

async fn quota(client: &Client, notify: &impl Fn(Reply)) {
    if !client.has(QUOTA) {
        return notify(Reply::Unsupported(Area::Quota));
    }
    notify(match client.quotas().await {
        Ok(q) => Reply::Quota(mail_usage(&q)),
        Err(e) => Reply::Failed { area: Area::Quota, error: e.to_string() },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use postman_core::rules::{Action, Field};

    fn q(resource: &str, used: u64, limit: u64, types: &[&str]) -> Quota {
        Quota {
            id: format!("{resource}{used}"),
            resource_type: resource.into(),
            used,
            hard_limit: limit,
            warn_limit: None,
            scope: "account".into(),
            name: String::new(),
            types: types.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn the_fullest_mail_quota_in_bytes() {
        assert_eq!(mail_usage(&[]), None);
        // Число писем и квота без предела — не то.
        assert_eq!(mail_usage(&[q("count", 10, 100, &["Mail"]), q("octets", 5, 0, &[])]), None);
        // Квота календаря — не про почту; без типов — про всё.
        assert_eq!(mail_usage(&[q("octets", 9, 10, &["Calendar"]), q("octets", 1, 10, &[])]), Some(Usage { used: 1, limit: 10 }));
        let u = mail_usage(&[q("octets", 300, 1000, &["Mail"]), q("octets", 950, 1000, &["Mail", "Calendar"])]).unwrap();
        assert_eq!(u, Usage { used: 950, limit: 1000 });
        assert_eq!(u.percent(), 95);
        assert_eq!(Usage { used: 3, limit: 0 }.percent(), 0);
        assert_eq!(Usage { used: u64::MAX, limit: 1 }.percent(), u64::MAX);
    }

    fn script(id: &str, name: &str, active: bool) -> SieveScript {
        SieveScript { id: id.into(), name: Some(name.into()), blob_id: format!("blob-{id}"), is_active: active }
    }

    fn rule(enabled: bool) -> Rule {
        Rule { enabled, field: Field::Subject, value: "x".into(), action: Action::Flag }
    }

    #[test]
    fn vacation_inside_active_rules_counts_as_enabled() {
        let off = VacationResponse { subject: Some("Нет на месте".into()), ..Default::default() };
        let ours = |active, vacation| Sieve {
            ours: Some((script("c", SCRIPT_NAME, active), Script { rules: vec![rule(true)], vacation })),
            ..Default::default()
        };
        assert!(effective(off.clone(), ours(true, true).vacation_in_rules()).is_enabled);
        // Скрипт выключен или без `include` — автоответа нет.
        assert!(!effective(off.clone(), ours(false, true).vacation_in_rules()).is_enabled);
        assert!(!effective(off.clone(), ours(true, false).vacation_in_rules()).is_enabled);
        assert!(effective(VacationResponse { is_enabled: true, ..off }, false).is_enabled);
        assert_eq!(ours(true, true).state(), RulesState { rules: vec![rule(true)], active: true, foreign: vec![] });
    }

    #[test]
    fn foreign_scripts_need_replace() {
        assert!(conflict(&Sieve::default()).is_none());
        let inactive = Foreign { name: "old".into(), text: "keep;".into(), active: false };
        let mut s = Sieve { foreign: vec![inactive.clone()], ..Default::default() };
        // Выключенный чужой не мешает.
        assert!(conflict(&s).is_none());
        s.foreign.push(Foreign { name: "admin".into(), text: "keep;".into(), active: true });
        assert!(matches!(conflict(&s), Some(Reply::RulesConflict { name, overwrite: false }) if name == "admin"));
        assert_eq!(s.state().active_foreign(), Some("admin"));
        s.ours_edited = Some(script("c", SCRIPT_NAME, false));
        assert!(matches!(conflict(&s), Some(Reply::RulesConflict { name, overwrite: true }) if name == SCRIPT_NAME));
    }
}
