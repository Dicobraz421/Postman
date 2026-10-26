//! Общие ящики (S5-28, DESIGN «Общие ящики», макет `SharedMailboxes`): «Взять в работу» (Ctrl+J) — письмо общего
//! ящика закрепляется за мной словами `pm.taken…` на сервере (`postman_core::marks`), коллеги видят «Взято в работу:
//! … · время» и «Забрать себе»; моё — «Вернуть в очередь». В строке письма — нейтральная метка «Назначено: …», у
//! ветки ящика — значок «общий» и права в подсказке, в ответе — предупреждение, если «От» сменили на свой адрес.

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use pets_ui::{Level as Toast, ToastAction};
use postman_cache::{EmailRow, Patch, Rights, account_of};
use postman_core::marks::{self, Taken};

use crate::chrome::{glyph, icon};
use crate::compose::Book;
use crate::mailer::Mailer;
use crate::roles::Look;

/// Кто я и как зовут коллег — `Global`: строке списка, шапке письма и окну ответа. Имена — из адресной книги экрана
/// почты (переписка, контакты, справочник сотрудников); адрес — `Event::Me` (в демо — владелец демо-ящика).
pub(crate) struct People {
    pub(crate) book: Book,
    pub(crate) me: Option<String>,
}

impl Global for People {}

/// Мой адрес, если известен.
pub(crate) fn me(cx: &App) -> Option<String> {
    cx.try_global::<People>().and_then(|p| p.me.clone())
}

pub(crate) fn set_me(me: Option<String>, cx: &mut App) {
    if cx.has_global::<People>() {
        cx.update_global::<People, _>(|p, _| p.me = me);
    }
}

/// Имя по адресу: из адресной книги, иначе сам адрес.
pub(crate) fn name(email: &str, cx: &App) -> String {
    cx.try_global::<People>()
        .and_then(|p| p.book.borrow().get(email).map(|e| e.name.clone()))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| email.to_owned())
}

/// Письмо общего ящика (ключ кэша с префиксом аккаунта).
pub(crate) fn is_shared(row: &EmailRow) -> bool {
    account_of(&row.id).is_some()
}

/// Кто держит письмо общего ящика; своё письмо — никто.
pub(crate) fn taken(row: &EmailRow) -> Option<Taken> {
    if is_shared(row) { marks::taken(&row.keywords) } else { None }
}

/// Метка «Назначено: …» в строке письма (DESIGN: нейтральная): «вы» или имя коллеги.
pub(crate) fn assigned(row: &EmailRow, cx: &App) -> Option<String> {
    let t = taken(row)?;
    let who = if me(cx).is_some_and(|m| t.by(&m)) { tr!("shared-assigned-me") } else { name(&t.who, cx) };
    Some(tr!("shared-assigned", who = who))
}

/// Время «взял»: сегодня — «10:42», раньше — «21.10 16:30» (как в списке).
fn when(t: &Taken) -> String {
    crate::format_date(t.at.as_second())
}

/// Права на ящик в подсказке ветки: отправка, изменение или только чтение (по папкам ящика).
pub(crate) fn rights_hint<'a>(read_only: bool, folders: impl Iterator<Item = &'a Rights>) -> String {
    let rights: Vec<&Rights> = folders.collect();
    let key = if rights.iter().any(|r| r.submit) {
        "shared-rights-send"
    } else if read_only {
        "shared-rights-read"
    } else {
        "shared-rights-change"
    };
    tr!("shared-branch-hint", rights = tr!(key))
}

/// Что сделает «Взять в работу» с письмом (кнопка и Ctrl+J): взять, забрать у коллеги или вернуть своё.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Act {
    Take,
    Steal,
    Release,
}

impl Act {
    pub(crate) fn of(row: &EmailRow, me: Option<&str>) -> Act {
        match (taken(row), me) {
            (Some(t), Some(me)) if t.by(me) => Act::Release,
            (Some(_), _) => Act::Steal,
            (None, _) => Act::Take,
        }
    }

    /// Подпись кнопки.
    pub(crate) fn label(self) -> String {
        tr!(match self {
            Act::Take => "shared-take",
            Act::Steal => "shared-steal",
            Act::Release => "shared-release",
        })
    }
}

/// Правка письма: взять (или забрать) — мои слова, вернуть — снять все. Адрес не годится в ключевое слово — `None`.
pub(crate) fn patch(row: &EmailRow, act: Act, me: &str, now: jiff::Timestamp) -> Option<Patch> {
    let keywords = match act {
        Act::Release => marks::releasing(&row.keywords).into_iter().map(|k| (k, false)).collect(),
        Act::Take | Act::Steal => {
            let (add, remove) = marks::taking(&row.keywords, me, now)?;
            remove.into_iter().map(|k| (k, false)).chain(add.into_iter().map(|k| (k, true))).collect()
        }
    };
    Some(Patch { id: row.id.clone(), keywords, mailboxes: Vec::new() })
}

/// Отмена правки: каждое слово — как было у письма до неё.
pub(crate) fn undo(row: &EmailRow, p: &Patch) -> Patch {
    let had = |k: &str| row.keywords.iter().any(|x| x.eq_ignore_ascii_case(k));
    Patch { id: p.id.clone(), keywords: p.keywords.iter().map(|(k, _)| (k.clone(), had(k))).collect(), mailboxes: Vec::new() }
}

/// Полоса в шапке письма общего ящика (макет): никто не взял — «Взять в работу»; взял коллега — его инициалы, «Взято в
/// работу: имя · время» и «Забрать себе»; взял я — «Вы взяли в работу · время» и «Вернуть в очередь».
pub(crate) fn strip(row: &EmailRow, l: &Look, cx: &App) -> Option<Div> {
    if !is_shared(row) {
        return None;
    }
    let me = me(cx);
    let act = Act::of(row, me.as_deref());
    let t = taken(row);
    let lead = match (&t, act) {
        (Some(t), Act::Steal) => {
            let who = name(&t.who, cx);
            let neutral = l.cat(postman_core::palette::NEUTRAL);
            let avatar = div()
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(neutral.bg)
                .text_color(neutral.text)
                .text_size(px(10.))
                .child(crate::list::initials(&who));
            let text = div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(tr!("shared-taken-by"))
                .child(div().ml_1().font_weight(FontWeight::SEMIBOLD).text_color(l.text()).child(who))
                .child(format!(" · {}", when(t)))
                .flex()
                .items_baseline();
            div().flex().flex_1().min_w_0().items_center().gap(px(10.)).child(avatar).child(text)
        }
        (Some(t), _) => div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(10.))
            .child(icon(glyph::CONTACTS, 16.).mx(px(3.)))
            .child(div().flex_1().min_w_0().truncate().child(tr!("shared-taken-me", time = when(t)))),
        (None, _) => div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(10.))
            .child(icon(glyph::CONTACTS, 16.).mx(px(3.)))
            .child(div().flex_1().min_w_0().truncate().child(tr!("shared-nobody"))),
    };
    let button = match act {
        Act::Release => div()
            .id("shared-act")
            .h(px(28.))
            .px(px(12.))
            .flex()
            .flex_none()
            .items_center()
            .rounded(l.px(Metric::RadiusControl))
            .text_color(l.accent_text())
            .cursor_pointer()
            .hover(|d| d.bg(l.c(pets_ui::theme::Role::FillSubtleHover))),
        _ => crate::buttons::secondary("shared-act", l).h(px(28.)),
    };
    let label = act.label();
    let button = button
        .text_size(l.px(Metric::TypeCaption))
        .child(label.clone())
        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::actions::mail::Take), cx));
    let button = crate::keytips::command(button, "mail.take", Some(label), l, cx);
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(36.))
            .py(px(4.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(6.))
            .bg(l.hover())
            .text_size(px(13.))
            .text_color(l.text2())
            .child(lead)
            .child(button),
    )
}

/// Ответ на письмо общего ящика (окно письма): адрес ящика и кто держит письмо, если не я.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Reply {
    /// Адрес общего ящика — «От» по умолчанию.
    pub(crate) address: String,
    /// Письмо в работе у коллеги: его имя.
    pub(crate) taken_by: Option<String>,
}

impl Reply {
    /// Ответ на письмо `row` общего ящика с адресом `address`.
    pub(crate) fn of(row: &EmailRow, address: &str, cx: &App) -> Reply {
        let me = me(cx);
        let taken_by = taken(row).filter(|t| me.as_deref().is_none_or(|m| !t.by(m))).map(|t| name(&t.who, cx));
        Reply { address: address.to_owned(), taken_by }
    }

    /// `soc@` — имя ящика в подсказках.
    pub(crate) fn short(&self) -> String {
        match self.address.split_once('@') {
            Some((local, _)) => format!("{local}@"),
            None => self.address.clone(),
        }
    }

    /// Строка под текстом ответа: «От» — свой адрес (ответ мимо ящика) или письмо держит коллега.
    pub(crate) fn note(&self, from: &str) -> Option<String> {
        if !from.trim().eq_ignore_ascii_case(&self.address) {
            return Some(tr!("shared-reply-personal", mailbox = self.short()));
        }
        self.taken_by.as_ref().map(|who| tr!("shared-reply-taken", who = who.clone()))
    }

    /// Подсказка под адресом в меню «От»: ящик — копия в его «Отправленных», свой — клиент увидит меня.
    pub(crate) fn hint(&self, address: &str) -> String {
        if address.eq_ignore_ascii_case(&self.address) {
            tr!("shared-from-shared-hint", mailbox = self.short())
        } else {
            tr!("shared-from-own-hint", mailbox = self.short())
        }
    }
}

/// Строка-предупреждение под текстом ответа (макет): значок «i», текст — серым на слабой подложке.
pub(crate) fn note_line(text: String, l: &Look) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .mx(px(12.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(4.))
        .bg(l.hover())
        .text_size(l.px(Metric::TypeCaption))
        .text_color(l.text2())
        .child(icon(glyph::INFO, 14.))
        .child(div().flex_1().child(text))
}

impl Mailer {
    /// `mail.take` (Ctrl+J, кнопка командной строки и шапки): взять в работу, забрать себе или вернуть в очередь
    /// письмо общего ящика. Кэш — сразу, сервер — `Command::Patch`; тост с «Отменить».
    pub(crate) fn take(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.lifecycle_target(cx) else {
            return self.toast(Toast::Info, tr!("actions-select-letter"), cx);
        };
        if !is_shared(&row) {
            return self.toast(Toast::Info, tr!("shared-take-own"), cx);
        }
        if !crate::list::may(&self.list.read(cx).rights, &row, |r| r.keywords) {
            return self.toast(Toast::Info, tr!("shared-read-only-take"), cx);
        }
        let Some(me) = me(cx) else {
            return self.toast(Toast::Info, tr!("shared-take-no-me"), cx);
        };
        let act = Act::of(&row, Some(&me));
        let Some(p) = patch(&row, act, &me, jiff::Timestamp::now()) else {
            return self.toast(Toast::Info, tr!("shared-take-no-me"), cx);
        };
        let back = undo(&row, &p);
        let mailbox = postman_cache::account_of(&row.id).and_then(|a| {
            self.folders.read(cx).accounts.iter().find(|x| x.id == a).map(|x| Reply { address: x.name.clone(), taken_by: None }.short())
        });
        let text = match act {
            Act::Take => tr!("shared-toast-taken"),
            Act::Steal => {
                let who = taken(&row).map(|t| name(&t.who, cx)).unwrap_or_default();
                tr!("shared-toast-stolen", who = who)
            }
            Act::Release => tr!("shared-toast-released", mailbox = mailbox.unwrap_or_default()),
        };
        self.apply_patches(vec![p], cx);
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("bulk-undo"), move |_, cx| {
            let back = back.clone();
            let _ = mailer.update(cx, |m, cx| m.apply_patches(vec![back], cx));
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Success, text, action, cx));
    }

    /// Кнопка «Взять в работу» в командной строке: открыта папка общего ящика, где можно менять ключевые слова;
    /// подпись — по письму («Вернуть в очередь» — у моего).
    pub(crate) fn take_button(&self, cx: &App) -> Option<Act> {
        let list = self.list.read(cx);
        let folder = list.mailbox.as_deref()?;
        account_of(folder)?;
        if list.rights.get(folder).is_some_and(|r| !r.keywords) {
            return None;
        }
        let me = me(cx);
        Some(self.lifecycle_target(cx).filter(is_shared).map_or(Act::Take, |r| Act::of(&r, me.as_deref())))
    }

    /// Кнопка командной строки — по открытой папке и письму (список или область чтения изменились).
    pub(crate) fn sync_take_button(&mut self, cx: &mut Context<Self>) {
        let take = self.take_button(cx);
        self.bar.update(cx, |b, cx| {
            if b.take != take {
                b.take = take;
                cx.notify();
            }
        });
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "shared", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/shared.ftl")), ("en", include_str!("../i18n/en/shared.ftl"))];

/// Сцены `--screens` общих ящиков (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // «Входящие» общего ящика в дереве папок, первое письмо открыто (S4-06): никто не взял его в работу — полоса
        // «Взять в работу», в командной строке — та же кнопка (S5-28).
        Scene::new("shared", shared, shared_ready).live(),
        // Тот же ящик (S5-28): письмо, взятое коллегой, — «Взято в работу: …» и «Забрать себе», в списке — метки
        // «Назначено: …»; ответ в области чтения — от ящика, строка «Письмо взято в работу…».
        Scene::new("taken", taken, taken_ready),
    ];

    /// «Входящие» первого общего ящика, где можно писать (иначе первого любого).
    fn shared_inbox(m: &Mailer, cx: &App) -> Option<String> {
        let f = m.folders.read(cx);
        // Общие ящики, а не другие учётные записи профиля (S7-03б: `@<n>` — тоже строка дерева).
        let shared: Vec<&postman_cache::AccountRow> =
            f.accounts.iter().filter(|a| postman_cache::profile_of(Some(&a.id)) != Some(a.id.as_str())).collect();
        let account = shared.iter().find(|a| !f.read_only(a)).or(shared.first())?;
        let mut boxes = f.of(Some(&account.id));
        let first = f.of(Some(&account.id)).next().map(|b| b.id.clone());
        boxes.find(|b| b.role.as_deref() == Some("inbox")).map(|b| b.id.clone()).or(first)
    }

    fn shared(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let Some(id) = shared_inbox(m, cx) else { return Err("no shared mailbox in the folder tree".into()) };
        m.open_mailbox(id, cx);
        m.list.update(cx, |l, cx| {
            if l.count > 0 {
                l.select(0, cx);
            }
        });
        Ok(())
    }

    fn shared_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Живой общий ящик может быть пуст: тогда снимок — без письма.
        let list = m.list.read(cx);
        let want = shared_inbox(m, cx);
        let opened = want.is_some() && list.mailbox == want && list.search_ids.is_none();
        let first = list.row(0).map(|r| r.id.as_str());
        let letter = screens::rows_loaded(list) && screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && Some(r.row.id.as_str()) == first);
        m.section == Section::Mail && opened && (list.count == 0 || letter)
    }

    fn taken(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.tabs.activate_kind(crate::tabs::TabKind::Main);
        let Some(id) = shared_inbox(m, cx) else { return Err("no shared mailbox in the folder tree".into()) };
        m.open_mailbox(id, cx);
        // Демо: второе письмо ящика отдела взял коллега (`postman_demo`, «Взять в работу»).
        let i = (0..m.list.read(cx).count).find(|&i| m.list.read(cx).row(i).and_then(super::taken).is_some_and(|t| !t.by(postman_demo::ME)));
        let Some(i) = i else { return Err("no message taken by a colleague in the shared mailbox".into()) };
        screens::remember_composer(m, cx);
        m.list.update(cx, |l, cx| l.select(i, cx));
        let mailer = cx.entity();
        cx.defer(move |cx| mailer.update(cx, |m, cx| m.respond(postman_compose::Mode::Reply, cx)));
        Ok(())
    }

    fn taken_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let before = screens::composer_before(m);
        let reply = m.compose.inline_view().is_some_and(|c| Some(c.read(cx).id) != before && c.read(cx).drawn());
        let taken = screens::reading(m, cx).is_some_and(|r| super::taken(&r.row).is_some());
        m.section == Section::Mail && screens::rows_loaded(m.list.read(cx)) && taken && reply
    }
}

#[cfg(test)]
mod tests {
    use super::{Act, Reply, patch, undo};
    use postman_cache::EmailRow;

    fn row(id: &str, keywords: &[&str]) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: "t".into(),
            mailbox_ids: vec!["s/inbox".into()],
            received_at: 0,
            subject: String::new(),
            from_name: String::new(),
            from_addr: String::new(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    /// Взять, забрать, вернуть: что делает кнопка и какая правка уходит; отмена возвращает слова как были.
    #[test]
    fn take_steal_release() {
        let me = "me@corp.example";
        let now: jiff::Timestamp = "2026-10-22T14:05:00Z".parse().unwrap();
        let other = "pm.taken.2026-10-22t1042z.a.petrov@corp.example";
        let mine = "pm.taken.2026-10-22t0900z.me@corp.example";
        assert_eq!(Act::of(&row("s/1", &[]), Some(me)), Act::Take);
        assert_eq!(Act::of(&row("s/1", &["pm.taken", other]), Some(me)), Act::Steal);
        assert_eq!(Act::of(&row("s/1", &["pm.taken", mine]), Some(me)), Act::Release);
        assert_eq!(Act::of(&row("1", &["pm.taken", mine]), Some(me)), Act::Take, "своё письмо — не общий ящик");

        let r = row("s/1", &["pm.taken", other, "pm.cat.team"]);
        let p = patch(&r, Act::Steal, me, now).unwrap();
        let new = "pm.taken.2026-10-22t1405z.me@corp.example".to_string();
        assert_eq!(p.keywords, vec![(other.into(), false), ("pm.taken".into(), true), (new.clone(), true)]);
        assert_eq!(undo(&r, &p).keywords, vec![(other.into(), true), ("pm.taken".into(), true), (new, false)]);
        let r = row("s/1", &["pm.taken", mine]);
        let p = patch(&r, Act::Release, me, now).unwrap();
        assert_eq!(p.keywords, vec![("pm.taken".into(), false), (mine.into(), false)]);
        assert!(patch(&row("s/1", &[]), Act::Take, "не адрес с пробелом", now).is_none());
    }

    /// Предупреждение в ответе: «От» — свой адрес; письмо у коллеги; ящик — без строки.
    #[test]
    fn reply_note() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        let r = Reply { address: "sales@corp.example".into(), taken_by: None };
        assert_eq!(r.short(), "sales@");
        assert!(r.note("sales@corp.example").is_none());
        assert!(r.note("SALES@corp.example ").is_none());
        assert!(r.note("me@corp.example").is_some_and(|t| t.contains("sales@")));
        let r = Reply { taken_by: Some("Петров Алексей".into()), ..r };
        assert!(r.note("sales@corp.example").is_some_and(|t| t.contains("Петров Алексей")));
    }
}
