//! KeyTips и подсказки кнопок (S5-18, DESIGN «Хоткеи»): в тексте кнопок клавиш нет. Подсказка при наведении —
//! «Название (клавиша)»; удержание Ctrl 0,4 с без другой клавиши показывает плашки с клавишами над кнопками, отпустил
//! или нажал клавишу — пропали. Клавиши — по выбранной схеме (`keymap::bound`), собираются вместе с привязками
//! (`commands::bind` → [`rebuild`]).
//!
//! Таймер — [`Timer`], без gpui (тест); экран почты ставит его по `on_modifiers_changed` и будит себя один раз через
//! [`DELAY`] — только пока Ctrl нажат, в простое кадров нет.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;

use crate::commands::SINGLE;
use crate::keymap::{self, Column, KeySet, human};
use crate::roles::Look;

/// Сколько держать Ctrl до плашек.
pub(crate) const DELAY: Duration = Duration::from_millis(400);

/// Таймер KeyTips: нажали Ctrl (один, без других клавиш) — через [`DELAY`] плашки; отпустили или нажали другую
/// клавишу — плашек нет.
#[derive(Debug)]
pub(crate) struct Timer {
    since: Option<Instant>,
    shown: bool,
    /// Задержка («Настройки → Внешний вид», S5-30); по умолчанию — [`DELAY`].
    pub(crate) delay: Duration,
}

impl Default for Timer {
    fn default() -> Self {
        Self { since: None, shown: false, delay: DELAY }
    }
}

impl Timer {
    /// Модификаторы сменились: `ctrl_only` — нажат один Ctrl. `true` — показ сменился.
    pub(crate) fn modifiers(&mut self, ctrl_only: bool, now: Instant) -> bool {
        if ctrl_only {
            if self.since.is_none() && !self.shown {
                self.since = Some(now);
            }
            false
        } else {
            self.reset()
        }
    }

    /// Нажата клавиша (сочетание с Ctrl — тоже): плашки не нужны. `true` — показ сменился.
    pub(crate) fn key(&mut self) -> bool {
        self.reset()
    }

    /// Когда проверить снова (`None` — не ждём).
    pub(crate) fn due(&self) -> Option<Instant> {
        self.since.filter(|_| !self.shown).map(|s| s + self.delay)
    }

    /// Прошло время: держат Ctrl [`DELAY`] — показать. `true` — показ сменился.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        match self.due() {
            Some(at) if now >= at => {
                self.shown = true;
                self.since = None;
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn shown(&self) -> bool {
        self.shown
    }

    fn reset(&mut self) -> bool {
        let was = self.shown;
        self.since = None;
        self.shown = false;
        was
    }
}

/// Клавиши команд по выбранной схеме и показ плашек — `Global`: кнопки любого view читают его при отрисовке, view
/// с кнопками перерисовывает экран почты (`Mailer::keytips_changed`).
#[derive(Default)]
pub(crate) struct KeyTips {
    keys: HashMap<&'static str, String>,
    shown: bool,
    /// Настройка «KeyTips» (выключена — плашек нет, подсказки остаются).
    enabled: bool,
}

impl Global for KeyTips {}

/// Клавиша команды для подсказки и плашки: в «Одиночных клавишах» — буква схемы, если есть, иначе первая.
pub(crate) fn keys_of(set: &KeySet) -> HashMap<&'static str, String> {
    let mut map: HashMap<&'static str, String> = HashMap::new();
    let bound = keymap::bound(crate::commands::all(), &SINGLE, set);
    for b in bound.iter().filter(|b| b.column == Column::Single).chain(bound.iter().filter(|b| b.column == Column::Outlook)) {
        map.entry(b.id).or_insert_with(|| b.keys.clone());
    }
    // Категории — клавиши 1…9, по одной на категорию: на кнопке — все.
    if map.contains_key("mail.categorize") {
        map.insert("mail.categorize", "1\u{2013}9".to_owned());
    }
    map
}

/// Клавиши по настройкам — вместе с привязками.
pub(crate) fn rebuild(cx: &mut App, prefs: &postman_core::prefs::Prefs) {
    let keys = keys_of(&KeySet::of(prefs));
    let enabled = prefs.keytips;
    let shown = cx.try_global::<KeyTips>().is_some_and(|k| k.shown) && enabled;
    cx.set_global(KeyTips { keys, shown, enabled });
}

/// Плашки показаны.
pub(crate) fn shown(cx: &App) -> bool {
    cx.try_global::<KeyTips>().is_some_and(|k| k.shown)
}

/// Показать или убрать плашки (настройка выключена — не показывать).
pub(crate) fn set_shown(on: bool, cx: &mut App) {
    let Some(k) = cx.try_global::<KeyTips>() else { return };
    let on = on && k.enabled;
    if k.shown != on {
        cx.update_global::<KeyTips, _>(|k, _| k.shown = on);
    }
}

/// Клавиша команды в записи Windows (`Ctrl+R`).
pub(crate) fn key(id: &str, cx: &App) -> Option<String> {
    cx.try_global::<KeyTips>().and_then(|k| k.keys.get(id)).map(|k| human(k))
}

/// «Название (клавиша)»: название команды и её клавиша по схеме; без клавиши — название.
pub(crate) fn tip(id: &str, cx: &App) -> String {
    let name = crate::commands::all().iter().find(|c| c.id == id).map(|c| tr!(&c.name_key())).unwrap_or_default();
    with_key(name, key(id, cx))
}

/// Текст подсказки со своим названием (кнопка называет действие точнее команды).
pub(crate) fn with_key(name: String, key: Option<String>) -> String {
    match key {
        Some(k) => format!("{name} ({k})"),
        None => name,
    }
}

/// Кнопка команды: подсказка «Название (клавиша)» и плашка KeyTips. `name` — своя подпись подсказки (иначе — название
/// команды).
pub(crate) fn command(el: Stateful<Div>, id: &'static str, name: Option<String>, l: &Look, cx: &App) -> Stateful<Div> {
    let key = key(id, cx);
    let text = match name {
        Some(n) => with_key(n, key.clone()),
        None => tip(id, cx),
    };
    let el = el.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx));
    badge(el, key, l, cx)
}

/// Плашка KeyTips над элементом (сам элемент без подсказки — поле, вкладка).
pub(crate) fn badge<E: ParentElement + Styled>(el: E, key: Option<String>, l: &Look, cx: &App) -> E {
    match key.filter(|_| shown(cx)) {
        Some(k) => el.relative().child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-6.))
                .px(px(4.))
                .rounded(px(3.))
                .bg(l.accent())
                .text_color(l.on_accent())
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(k),
        ),
        None => el,
    }
}

// --- экран почты ---

impl crate::Mailer {
    /// Модификаторы сменились: один Ctrl — ждать [`DELAY`]; другие или отпущен — плашек нет.
    pub(crate) fn keytips_modifiers(&mut self, m: Modifiers, cx: &mut Context<Self>) {
        let ctrl_only = m.control && !m.alt && !m.shift && !m.platform && !m.function;
        self.keytips.delay = Duration::from_millis(u64::from(crate::prefs::get(cx).look.keytips_delay));
        if self.keytips.modifiers(ctrl_only, Instant::now()) {
            set_shown(false, cx);
        }
        self.keytips_wait(cx);
    }

    /// Нажата клавиша: плашек нет, ждать нечего.
    pub(crate) fn keytips_key(&mut self, cx: &mut Context<Self>) {
        if self.keytips.key() {
            set_shown(false, cx);
        }
        self.keytips_task = None;
    }

    /// Одно пробуждение на срок таймера; не ждём — задачи нет (простой без кадров).
    fn keytips_wait(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.keytips.due() else {
            self.keytips_task = None;
            return;
        };
        if self.keytips_task.is_some() {
            return;
        }
        let wait = at.saturating_duration_since(Instant::now());
        self.keytips_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |m: &mut crate::Mailer, cx| {
                m.keytips_task = None;
                if m.keytips.tick(Instant::now()) {
                    set_shown(true, cx);
                }
            });
        }));
    }

    /// Плашки показаны, убраны или клавиши сменились: части с кнопками — заново (у кэшируемых view своё `notify`).
    pub(crate) fn keytips_changed(&mut self, cx: &mut Context<Self>) {
        self.bar.update(cx, |_, cx| cx.notify());
        self.reading.update(cx, |_, cx| cx.notify());
        self.summary.update(cx, |_, cx| cx.notify());
        self.cal_bar.update(cx, |_, cx| cx.notify());
        self.contacts_bar.update(cx, |_, cx| cx.notify());
        for o in &self.compose.open {
            o.view.update(cx, |_, cx| cx.notify());
        }
        if let Some(v) = self.settings.view.clone() {
            v.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "keytips", scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

/// Сцены `--screens` KeyTips (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::prefs::KeyScheme;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // KeyTips (S5-18): письмо открыто, Ctrl удержан 0,4 с — плашки с клавишами над кнопками (схема «Outlook»).
        Scene::new("keytips", keytips, keytips_ready),
        // То же в схеме «Одиночные клавиши»: открыто приглашение без ответа — A / T / N на его кнопках.
        Scene::new("keytips-single", single, single_ready).continues(),
    ];

    /// Плашки KeyTips — только в своих сценах: дальше Ctrl отпущен, схема — «Outlook».
    pub(crate) fn reset(s: &Scene, _: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) {
        if super::shown(cx) && !matches!(s.name, "keytips" | "keytips-single") {
            screens::send_input(window, cx, PlatformInput::ModifiersChanged(screens::ctrl(false)));
            crate::prefs::update(cx, |p| p.key_scheme = KeyScheme::Outlook);
        }
    }

    fn keytips(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        if m.toasts.read(cx).is_history_open() {
            m.toasts.update(cx, |t, cx| t.toggle_history(cx));
        }
        m.set_section(Section::Mail, cx);
        m.open_mailbox("inbox".into(), cx);
        m.list.update(cx, |l, cx| l.select(0, cx));
        // Как удержание Ctrl: событие окна, таймер экрана почты (0,4 с).
        screens::send_input(window, cx, PlatformInput::ModifiersChanged(screens::ctrl(true)));
        Ok(())
    }

    fn keytips_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        super::shown(cx) && screens::reading(m, cx).is_some_and(|r| r.doc.is_some())
    }

    fn single(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        crate::prefs::update(cx, |p| p.key_scheme = KeyScheme::Single);
        let invitation = crate::invite_card::scenes::invitation;
        let Some(i) = screens::demo_row(m.list.read(cx), invitation) else {
            return Err("no invitation in the demo inbox".into());
        };
        m.list.update(cx, |l, cx| l.select(i, cx));
        Ok(())
    }

    fn single_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let single = crate::prefs::get(cx).key_scheme == KeyScheme::Single;
        super::shown(cx) && single && screens::reading(m, cx).is_some_and(|r| r.doc.is_some() && r.invite.is_some())
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`.
    use super::{DELAY, KeySet, SINGLE, Timer, human, keymap, keys_of, with_key};
    use std::time::{Duration, Instant};

    /// 0,4 с с одним Ctrl — плашки; раньше — нет; отпустили или нажали другую клавишу — пропали; Ctrl с Shift — нет.
    #[test]
    fn keytips_show_after_holding_ctrl() {
        let t0 = Instant::now();
        let mut t = Timer::default();
        assert_eq!(t.due(), None, "в простое не ждём");
        assert!(!t.modifiers(true, t0));
        assert_eq!(t.due(), Some(t0 + DELAY));
        assert!(!t.tick(t0 + Duration::from_millis(399)));
        assert!(!t.shown());
        // Повтор нажатия Ctrl (автоповтор) отсчёт не сбрасывает.
        assert!(!t.modifiers(true, t0 + Duration::from_millis(200)));
        assert!(t.tick(t0 + DELAY));
        assert!(t.shown());
        assert_eq!(t.due(), None, "показаны — больше не ждём");
        // Отпустили — пропали.
        assert!(t.modifiers(false, t0 + Duration::from_secs(1)));
        assert!(!t.shown());
        // Нажали другую клавишу до срока (Ctrl+K) — плашек не будет.
        t.modifiers(true, t0);
        assert!(!t.key());
        assert!(!t.tick(t0 + Duration::from_secs(1)));
        // После показа клавиша — убирает.
        t.modifiers(true, t0);
        t.tick(t0 + DELAY);
        assert!(t.key());
        assert!(!t.shown());
        // Ctrl+Shift — не KeyTips.
        t.modifiers(true, t0);
        t.modifiers(false, t0 + Duration::from_millis(100));
        assert_eq!(t.due(), None);
    }

    /// В подписях кнопок и полей клавиш нет (DESIGN «Хоткеи»): ни один текст интерфейса не называет сочетание реестра
    /// с модификатором и не пишет «(Esc)». Исключения — тексты, которые объясняют клавиши (настройки, подсказки форм).
    #[test]
    fn no_keys_in_button_texts() {
        let keys: Vec<String> = keymap::bound(crate::commands::all(), &SINGLE, &KeySet::default())
            .into_iter()
            .map(|b| human(&b.keys))
            .filter(|k| k.contains('+'))
            .collect();
        let explains = ["settings-", "signin-demo", "contacts-form-hint", "palette"];
        for lang in ["ru", "en"] {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("i18n").join(lang);
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let text = std::fs::read_to_string(&path).unwrap();
                for (k, v) in crate::commands::parity::ftl_names(&text) {
                    if explains.iter().any(|e| k.starts_with(e)) {
                        continue;
                    }
                    for key in &keys {
                        assert!(!v.contains(key.as_str()), "{lang}/{k}: «{v}» называет {key}");
                    }
                    assert!(!v.contains("(Esc)"), "{lang}/{k}: «{v}»");
                }
            }
        }
    }

    /// Клавиша кнопки — по схеме: в «Outlook» Ctrl+R, в «Одиночных» — R; подсказка — «Название (клавиша)».
    #[test]
    fn button_keys_follow_the_scheme() {
        let outlook = keys_of(&KeySet::default());
        assert_eq!(outlook.get("mail.reply").map(String::as_str), Some("ctrl-r"));
        assert_eq!(outlook.get("invite.accept"), None);
        let single = keys_of(&KeySet { scheme: postman_core::prefs::KeyScheme::Single, ..KeySet::default() });
        assert_eq!(single.get("mail.reply").map(String::as_str), Some("r"));
        assert_eq!(single.get("invite.accept").map(String::as_str), Some("a"));
        assert_eq!(single.get("mail.delete").map(String::as_str), Some("delete"));
        assert_eq!(with_key("Ответить".into(), Some("Ctrl+R".into())), "Ответить (Ctrl+R)");
        assert_eq!(with_key("Флаг".into(), None), "Флаг");
    }
}
