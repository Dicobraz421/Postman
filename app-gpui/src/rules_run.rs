//! Правила в приложении (T2-50): правила «в приложении» по новым письмам «Входящих» (после каждого обновления
//! писем; отметка «что уже проверено» — в кэше), «Применять к существующим» и правила из письма — пункты меню
//! «Всегда перемещать от этого отправителя ▸» и «Заблокировать отправителя» (без окон: правило сразу уходит на
//! сервер, письма отправителя — следом). Правки писем — тем же путём, что кнопки над письмами (`bulk::plan` →
//! `apply_patches`); срабатывания — в журнал (в кэше, последние [`LOG_MAX`]), его показывает «Настройки → Правила».

use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::{Level as Toast, ToastAction};
use postman_cache::EmailRow;
use postman_core::bulk::{self, Act, Need};
use postman_core::rulerun;
use postman_core::rules::{Action, Cond, Field, Rule};
use postman_core::settings::Request;
use postman_core::sync::Command;

use crate::Mailer;
use crate::rules::{LogEntry, log_parse, log_text};

/// Ключи в кэше: до какого времени письма проверены, журнал срабатываний.
const SINCE: &str = "rules-since";
const LOG: &str = "rules-log";
/// Строк журнала — не больше.
pub(crate) const LOG_MAX: usize = 200;

#[derive(Default)]
pub(crate) struct RulesRun {
    running: bool,
    again: bool,
    since: Option<i64>,
    /// Журнал срабатываний (новые первыми); `None` — ещё не прочитан из кэша.
    pub(crate) log: Option<Vec<LogEntry>>,
    /// Правила из письма, которые ждут списка правил с сервера: правило и применить ли к письмам.
    pub(crate) waiting: Vec<Rule>,
    /// Запись правил — из меню письма: свой тост уже показан, «Правила сохранены» не нужен.
    pub(crate) quiet: bool,
}

/// Правка писем по действию правила; пересылка — только сервером.
fn act_of(a: &Action, rows: &[EmailRow]) -> Option<Act> {
    Some(match a {
        Action::Move { mailbox_id, .. } => Act::Move { to: mailbox_id.clone() },
        Action::MarkRead => Act::Seen(true),
        Action::Flag => Act::Flag(true),
        // В приложении «удалить» — в «Удалённые»: без корзины удаляет только сервер.
        Action::Discard => Act::Trash,
        Action::Category(k) => {
            let _ = rows;
            Act::Keywords { add: vec![k.clone()], remove: Vec::new() }
        }
        Action::Redirect(_) => return None,
    })
}

/// Правило «всегда от этого отправителя»: «От содержит адрес» → действие.
pub(crate) fn from_sender(name: String, address: &str, action: Action) -> Rule {
    Rule { name, when: vec![vec![Cond::contains(Field::From, address.to_owned())]], ..Rule::simple(true, Field::From, address, action) }
}

impl Mailer {
    /// Журнал срабатываний: из кэша (в фоне) при первой надобности; панели правил — копию.
    pub(crate) fn rules_log(&mut self, cx: &mut Context<Self>) {
        if let Some(log) = self.rules_run.log.clone() {
            return self.give_rules_log(log, cx);
        }
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { cache.state(LOG).ok().flatten().map(|t| log_parse(&t)).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let log = task.await;
            let _ = this.update(cx, |m, cx| {
                let mut all = log;
                // Пока читали, могли сработать новые.
                if let Some(fresh) = m.rules_run.log.take() {
                    all.splice(0..0, fresh);
                }
                m.rules_run.log = Some(all.clone());
                m.give_rules_log(all, cx);
            });
        })
        .detach();
    }

    fn give_rules_log(&mut self, log: Vec<LogEntry>, cx: &mut Context<Self>) {
        if let Some(p) = &self.settings.rules_panel {
            p.update(cx, |p, cx| p.set_log(log, cx));
        }
    }

    /// Новые записи журнала — сверху; журнал — в кэш (в фоне).
    fn log_hits(&mut self, entries: Vec<LogEntry>, cx: &mut Context<Self>) {
        if entries.is_empty() {
            return;
        }
        let mut log = self.rules_run.log.take().unwrap_or_default();
        log.splice(0..0, entries);
        log.truncate(LOG_MAX);
        let text = log_text(&log);
        self.rules_run.log = Some(log.clone());
        self.give_rules_log(log, cx);
        let cache = self.cache.clone();
        cx.background_spawn(async move {
            if let Err(e) = cache.set_state(LOG, &text) {
                tracing::error!("rules: rules log not written: {e}");
            }
        })
        .detach();
    }

    /// Правила «в приложении» по письмам, пришедшим с прошлой проверки (живой ящик; одна проверка за раз).
    pub(crate) fn rules_pass(&mut self, cx: &mut Context<Self>) {
        if !self.backend.is_live() || self.screens.is_some() || self.bench.is_some() {
            return;
        }
        let Some(rules) = self.settings.rules.as_ref().map(|s| s.rules.clone()) else { return };
        if !rules.iter().any(|r| r.enabled && r.local) {
            return;
        }
        if self.rules_run.running {
            self.rules_run.again = true;
            return;
        }
        self.rules_run.running = true;
        let cache = self.cache.clone();
        let known = self.rules_run.since;
        let list = rules.clone();
        let task = cx.background_spawn(async move {
            let now = jiff::Timestamp::now().as_second();
            let stored = cache.state(SINCE).ok().flatten().and_then(|s| s.parse::<i64>().ok());
            let Some(since) = known.or(stored) else {
                // Первый раз: то, что уже лежит, правила не трогают.
                let _ = cache.set_state(SINCE, &now.to_string());
                return Ok::<_, postman_cache::Error>((Vec::new(), now));
            };
            let hits = rulerun::fresh_hits(&cache, &list, since)?;
            let next = hits.iter().map(|h| h.received_at).max().unwrap_or(since).max(since);
            if next != since {
                cache.set_state(SINCE, &next.to_string())?;
            }
            Ok((hits, next))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |m, cx| {
                m.rules_run.running = false;
                match result {
                    Ok((hits, since)) => {
                        m.rules_run.since = Some(since);
                        let (mailboxes, categories) = m.rules_names(cx);
                        let mut entries = Vec::new();
                        for (i, r) in rules.iter().enumerate() {
                            let ids: Vec<String> = hits.iter().filter(|h| h.rule == i).map(|h| h.id.clone()).collect();
                            if ids.is_empty() {
                                continue;
                            }
                            let name = crate::rules::title(r, &mailboxes, &categories);
                            entries.extend(hits.iter().filter(|h| h.rule == i).map(|h| LogEntry {
                                at: jiff::Timestamp::now().as_second(),
                                rule: name.clone(),
                                subject: h.subject.clone(),
                            }));
                            m.apply_rule_to(r.clone(), ids, true, cx);
                        }
                        m.log_hits(entries, cx);
                    }
                    Err(e) => tracing::error!("rules: rules not applied: {e}"),
                }
                if std::mem::take(&mut m.rules_run.again) {
                    m.rules_pass(cx);
                }
            });
        })
        .detach();
    }

    /// Папки и категории своего ящика — для имён правил.
    pub(crate) fn rules_names(&self, cx: &App) -> (Vec<(String, String)>, Vec<crate::rules::CategoryChoice>) {
        let mailboxes = self.folders.read(cx).of(None).map(|m| (m.id.clone(), m.name.clone())).collect();
        let categories = self
            .list
            .read(cx)
            .labels
            .for_account(None)
            .cloned()
            .unwrap_or_else(crate::labels::default_table)
            .visible()
            .map(|c| (c.keyword.clone(), c.name.clone(), c.shown_color()))
            .collect();
        (mailboxes, categories)
    }

    /// «Применять к существующим»: подходящие письма кэша (в фоне) — действиям правила, тост «применено: N».
    pub(crate) fn apply_rule(&mut self, rule: Rule, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let check = rule.clone();
        let task = cx.background_spawn(async move { rulerun::matching(&cache, &check, rulerun::EXISTING) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |m, cx| match found {
                Ok((rows, _)) => {
                    let (mailboxes, categories) = m.rules_names(cx);
                    let name = crate::rules::title(&rule, &mailboxes, &categories);
                    let now = jiff::Timestamp::now().as_second();
                    let entries = rows.iter().map(|r| LogEntry { at: now, rule: name.clone(), subject: r.subject.clone() }).collect();
                    let ids: Vec<String> = rows.into_iter().map(|r| r.id).collect();
                    let n = ids.len();
                    m.apply_rule_to(rule, ids, true, cx);
                    m.log_hits(entries, cx);
                    m.toast(Toast::Success, tr!("rules-applied", rule = name, n = crate::bulk::plural_n(n)), cx);
                }
                Err(e) => tracing::error!("rules: rule not applied: {e}"),
            });
        })
        .detach();
    }

    /// Действия правила письмам `ids`: каждое — планом `bulk` (права общих ящиков, уже стоящие слова), правки — в
    /// кэш и серверу. `quiet` — без тоста «Отменить» на каждое действие.
    pub(crate) fn apply_rule_to(&mut self, rule: Rule, ids: Vec<String>, quiet: bool, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let cache = self.cache.clone();
        let mailboxes = self.folders.read(cx).mailboxes.clone();
        let rights = self.list.read(cx).rights.clone();
        let task = cx.background_spawn(async move {
            let mut rows = cache.rows_by_ids(&ids).unwrap_or_default();
            let mut patches = Vec::new();
            for a in &rule.actions {
                let Some(act) = act_of(a, &rows) else { continue };
                let plan = bulk::plan(&rows, &act, &mailboxes, |r, need| {
                    crate::list::may(&rights, r, |x| match need {
                        Need::Seen => x.seen,
                        Need::Keywords => x.keywords,
                        Need::Remove => x.remove,
                    })
                });
                // Следующее действие — над письмом с уже применённым (папка, слова): план видит его новым.
                for p in &plan.patches {
                    if let Some(row) = rows.iter_mut().find(|r| r.id == p.id) {
                        p.apply(row);
                    }
                }
                patches.extend(plan.patches);
            }
            patches
        });
        cx.spawn(async move |this, cx| {
            let patches = task.await;
            let _ = this.update(cx, |m, cx| {
                if !quiet && !patches.is_empty() {
                    m.toast(Toast::Success, tr!("rules-applied", rule = rule.name.clone(), n = crate::bulk::plural_n(patches.len())), cx);
                }
                m.apply_patches(patches, cx);
            });
        })
        .detach();
    }

    /// Новое правило из письма: в конец списка на сервере; список ещё не пришёл — запросить и дописать по ответу.
    pub(crate) fn add_rule(&mut self, rule: Rule, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return self.needs_server(cx);
        }
        self.rules_run.quiet = true;
        match self.settings.rules.clone() {
            Some(state) => {
                let mut rules = state.rules;
                rules.push(rule);
                if !self.backend.send(Command::Settings(Request::SaveRules { rules, replace: false })) {
                    self.sync_stopped(cx);
                }
            }
            None => {
                self.rules_run.waiting.push(rule);
                if !self.backend.send(Command::Settings(Request::Rules)) {
                    self.sync_stopped(cx);
                }
            }
        }
    }

    /// Пришёл список правил: дописать ждущие правила из письма.
    pub(crate) fn flush_waiting_rules(&mut self, cx: &mut Context<Self>) {
        if self.rules_run.waiting.is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut self.rules_run.waiting);
        let Some(state) = self.settings.rules.clone() else { return };
        let mut rules = state.rules;
        rules.extend(waiting);
        if !self.backend.send(Command::Settings(Request::SaveRules { rules, replace: false })) {
            self.sync_stopped(cx);
        }
    }

    /// «Всегда перемещать от этого отправителя ▸ <папка>»: правило на сервер и письма отправителя — в папку.
    pub(crate) fn always_move(&mut self, row: EmailRow, folder: (String, String), cx: &mut Context<Self>) {
        let address = row.from_addr.trim().to_owned();
        if address.is_empty() {
            return;
        }
        let (id, name) = folder;
        let rule = from_sender(
            tr!("rules-from-move-name", sender = address.clone(), folder = name.clone()),
            &address,
            Action::Move { mailbox_id: id, name: name.clone() },
        );
        self.add_rule(rule.clone(), cx);
        if self.backend.is_live() {
            self.apply_rule(rule, cx);
        }
        self.rule_toast(tr!("rules-from-move-toast", sender = address, folder = name), cx);
    }

    /// «Заблокировать отправителя»: его письма — в «Нежелательные» (нет — в «Удалённые») правилом на сервере.
    pub(crate) fn block_sender(&mut self, row: EmailRow, cx: &mut Context<Self>) {
        let address = row.from_addr.trim().to_owned();
        if address.is_empty() {
            return;
        }
        let folders = self.folders.read(cx);
        let target =
            folders.of(None).find(|m| m.role.as_deref() == Some("junk")).or_else(|| folders.of(None).find(|m| m.role.as_deref() == Some("trash")));
        let Some(target) = target.map(|m| (m.id.clone(), m.name.clone())) else {
            return self.toast(Toast::Info, tr!("rules-block-no-folder"), cx);
        };
        let (id, name) = target;
        let rule = from_sender(tr!("rules-block-name", sender = address.clone()), &address, Action::Move { mailbox_id: id, name: name.clone() });
        self.add_rule(rule.clone(), cx);
        if self.backend.is_live() {
            self.apply_rule(rule, cx);
        }
        self.rule_toast(tr!("rules-block-toast", sender = address, folder = name), cx);
    }

    /// Тост «Правило создано…» с «Правила» — открыть раздел.
    fn rule_toast(&mut self, text: String, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return;
        }
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("rules-open"), move |window, cx| {
            let _ = mailer.update(cx, |m, cx| m.open_rules(window, cx));
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Success, text, action, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{act_of, from_sender};
    use postman_core::bulk::Act;
    use postman_core::rules::{Action, Field};

    #[test]
    fn rule_actions_become_letter_edits() {
        assert_eq!(act_of(&Action::MarkRead, &[]), Some(Act::Seen(true)));
        assert_eq!(act_of(&Action::Discard, &[]), Some(Act::Trash), "в приложении — в «Удалённые»");
        assert_eq!(act_of(&Action::Redirect("a@b.c".into()), &[]), None, "пересылка — только сервером");
        assert_eq!(act_of(&Action::Move { mailbox_id: "f".into(), name: "F".into() }, &[]), Some(Act::Move { to: "f".into() }));
        let r = from_sender("x".into(), "boss@corp", Action::Flag);
        assert_eq!((r.name.as_str(), r.when[0][0].field, r.when[0][0].value.as_str()), ("x", Field::From, "boss@corp"));
    }
}
