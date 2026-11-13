//! Быстрый ответ внизу ленты беседы (MSGR-02, решение владельца `docs/decisions/2026-11-14-messenger-comforts.md`).
//!
//! Под лентой (`thread.toggle`, «лента вместо письма») — поле «Ответить всем»: Enter, Ctrl+Enter (⌘Enter) — отправить,
//! Shift+Enter — новая строка; «Развернуть» — полный ответ в области чтения с набранным. Фрагменты (`fragments.rs`)
//! из ленты встают сюда цитатами над полем, текст поля — ответ на последнюю. Отправка — обычным ответом всем через
//! окно письма (`Composer::dispatch`): адресаты, подпись, «Перед отправкой» и окно «Отменить» — как у него. `@` —
//! подсказка участников беседы и адресной книги (`mentions.rs`), выбранный — в «Кому».

use gpui_kit::component::input::{InputEvent, MoveDown, MoveUp, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_cache::EmailRow;
use postman_compose::fragments::Fragment;
use postman_compose::{Address, Mode};

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::composer::Composer;
use crate::fragments::Prepared;
use crate::mentions::Mentions;
use crate::reading::{ReadingEvent, ReadingPane};

/// Быстрый ответ ленты: поле, фрагменты над ним, подсказка упоминания, упомянутые.
#[derive(Default)]
pub(crate) struct Quick {
    input: Option<(Entity<TextareaState>, Subscription)>,
    pub(crate) fragments: Vec<Fragment>,
    pub(crate) mention: Mentions,
    mentioned: Vec<Address>,
    /// Беседа, к которой набрано: другая беседа — поле пустое.
    thread: String,
    /// Фокус — в поле при следующей отрисовке.
    focus: bool,
}

/// Участники беседы открытого письма: отправители ленты и автор письма, без меня.
pub(crate) fn participants(r: &ReadingPane) -> Vec<Address> {
    let me: Vec<&str> = r.mine.iter().map(String::as_str).chain(r.me.as_deref()).collect();
    let rows = r.reading.iter().map(|x| &x.row).chain(r.feed.iter().flat_map(|f| f.conv.messages.iter().map(|m| &m.row)));
    let mut out: Vec<Address> = Vec::new();
    for row in rows {
        let mine = me.iter().any(|m| m.eq_ignore_ascii_case(&row.from_addr));
        if !mine && !out.iter().any(|a| a.email.eq_ignore_ascii_case(&row.from_addr)) {
            out.push(Address::new(&row.from_name, &row.from_addr));
        }
    }
    out
}

impl Quick {
    /// Набранный текст.
    pub(crate) fn text(&self, cx: &App) -> String {
        self.input.as_ref().map(|(s, _)| s.read(cx).value().to_string()).unwrap_or_default()
    }

    /// Сколько людей упомянуто в набранном.
    pub(crate) fn mentioned_count(&self) -> usize {
        self.mentioned.len()
    }
}

impl ReadingPane {
    /// Перед отрисовкой: поле быстрого ответа (один раз) и сброс набранного при смене беседы.
    pub(crate) fn quick_prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.reading.as_ref().map(|r| r.row.thread_id.clone()).unwrap_or_default();
        if self.quick.input.is_none() {
            let state = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true).auto_grow(1, 6).placeholder(tr!("quick-placeholder")));
            let sub = cx.subscribe_in(&state, window, |this: &mut ReadingPane, state, ev: &InputEvent, window, cx| match ev {
                // Enter и Ctrl+Enter (⌘Enter) — отправить; Shift+Enter — новая строка (её вставило поле). В подсказке
                // упоминания Enter — выбрать человека.
                InputEvent::PressEnter { shift: false, .. } => {
                    if this.quick.mention.open() {
                        this.quick_mention(None, window, cx);
                    } else {
                        this.quick_submit(true, window, cx);
                    }
                }
                InputEvent::Change => {
                    let (text, cursor) = {
                        let s = state.read(cx);
                        (s.value().to_string(), s.cursor())
                    };
                    let people = participants(this);
                    let me: Vec<String> = this.mine.iter().cloned().chain(this.me.clone()).collect();
                    let book = this.book.clone();
                    let was = this.quick.mention.clone();
                    this.quick.mention.refresh(&text, cursor, &people, book.as_ref(), &me);
                    if this.quick.mention != was {
                        cx.notify();
                    }
                }
                // Адресная книга — для подсказки упоминаний (грузит экран почты, один раз).
                InputEvent::Focus => cx.emit(ReadingEvent::LoadBook),
                _ => {}
            });
            self.quick.input = Some((state, sub));
        }
        if self.quick.thread != thread {
            self.quick.thread = thread;
            self.quick.fragments.clear();
            self.quick.mentioned.clear();
            self.quick.mention.clear();
            if let Some((s, _)) = &self.quick.input {
                s.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
        if std::mem::take(&mut self.quick.focus)
            && let Some((s, _)) = &self.quick.input
        {
            s.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// Фрагмент из ленты — цитатой над полем; фокус — в поле, лента прокручена к нему.
    pub(crate) fn quick_add_fragment(&mut self, f: Fragment, _window: &mut Window, cx: &mut Context<Self>) {
        self.quick.fragments.push(f);
        self.quick.focus = true;
        // Поле — внизу ленты: показать его.
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Отправить (`send`) или развернуть набранное в полный ответ всем.
    fn quick_submit(&mut self, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((state, _)) = &self.quick.input else { return };
        let text = state.read(cx).value().to_string();
        if text.trim().is_empty() && self.quick.fragments.is_empty() && send {
            return;
        }
        let Some(row) = self.reading.as_ref().map(|r| r.row.clone()) else { return };
        let p = Prepared {
            letter: row.id.clone(),
            fragments: std::mem::take(&mut self.quick.fragments),
            text: text.trim().to_owned(),
            mentioned: std::mem::take(&mut self.quick.mentioned),
            send,
        };
        state.update(cx, |s, cx| s.set_value("", window, cx));
        self.quick.mention.clear();
        cx.emit(ReadingEvent::QuickReply(Box::new((row, p))));
        cx.notify();
    }

    /// Упомянуть `i`-го из подсказки (или выбранного).
    fn quick_mention(&mut self, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((state, _)) = self.quick.input.as_ref().map(|(s, _)| (s.clone(), ())) else { return };
        let Some((range, token, who)) = self.quick.mention.take(i) else { return };
        state.update(cx, |s, cx| {
            s.set_selected_range(range, cx);
            s.replace(token, window, cx);
            s.focus(window, cx);
        });
        self.quick.mention.clear();
        if !self.quick.mentioned.iter().any(|a| a.email.eq_ignore_ascii_case(&who.email)) {
            self.quick.mentioned.push(who);
        }
        cx.notify();
    }

    /// Быстрый ответ под лентой: цитаты фрагментов, подсказка упоминания, поле, «Развернуть» и «Отправить».
    pub(crate) fn quick_view(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let (state, _) = self.quick.input.as_ref()?;
        let quotes: Vec<Div> = self
            .quick
            .fragments
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let caption = tr!("fragments-caption", who = f.who.clone(), date = f.date.clone());
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl(px(10.))
                            .border_l_2()
                            .border_color(l.c(Role::StrokeStrong))
                            .text_color(l.text2())
                            .child(div().text_size(px(12.)).child(caption))
                            .child(div().max_h(px(60.)).overflow_hidden().child(f.text.clone())),
                    )
                    .child(
                        crate::composer::quiet_plain(("quick-fragment-remove", i), &l)
                            .size(px(24.))
                            .px_0()
                            .justify_center()
                            .text_color(l.text2())
                            .hover(|d| d.bg(l.hover()))
                            .child(icon(glyph::CLOSE, 11.))
                            .tooltip(|window, cx| crate::screens::tip(tr!("fragments-remove"), window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if i < this.quick.fragments.len() {
                                    this.quick.fragments.remove(i);
                                }
                                cx.notify();
                            })),
                    )
            })
            .collect();
        let mention =
            crate::mentions::panel(&self.quick.mention, &l, cx, |this: &mut ReadingPane, i, window, cx| this.quick_mention(Some(i), window, cx))
                .map(|p| p.mx_0());
        let focused = state.focus_handle(cx).contains_focused(window, cx);
        let send_tip = tr!("quick-send");
        let field = div()
            .flex()
            .items_end()
            .gap(px(6.))
            .child(div().flex_1().min_w_0().child(Textarea::new(state).appearance(false)))
            .child(
                crate::composer::quiet_plain("quick-expand", &l)
                    .size(px(30.))
                    .px_0()
                    .justify_center()
                    .text_color(l.text2())
                    .hover(|d| d.bg(l.hover()))
                    .child(icon(glyph::EXPAND, 14.))
                    .tooltip(|window, cx| crate::screens::tip(tr!("quick-expand"), window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.quick_submit(false, window, cx))),
            )
            .child(
                crate::buttons::primary("quick-send", &l)
                    .size(px(30.))
                    .px_0()
                    .justify_center()
                    .child(icon(glyph::SEND, 14.))
                    .tooltip(move |window, cx| crate::screens::tip(send_tip.clone(), window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.quick_submit(true, window, cx))),
            );
        Some(
            div()
                .id("quick-reply")
                .children(crate::screens::spot("quick-reply"))
                .mt(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(12.))
                .py(px(8.))
                .rounded(l.px(Metric::RadiusControl))
                .border_1()
                .border_color(if focused { l.accent() } else { l.stroke() })
                .bg(l.panel())
                // ↑ / ↓ в подсказке упоминания — по людям, а не по строкам поля.
                .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                    if this.quick.mention.open() {
                        cx.stop_propagation();
                        this.quick.mention.step(-1);
                        cx.notify();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                    if this.quick.mention.open() {
                        cx.stop_propagation();
                        this.quick.mention.step(1);
                        cx.notify();
                    }
                }))
                .children(quotes)
                .children(mention)
                .child(field)
                .into_any_element(),
        )
    }
}

impl Mailer {
    /// Быстрый ответ ленты: ответ всем на открытое письмо с набранным (и сразу отправить). Ответ на беседу уже
    /// открыт — набранное и фрагменты встают в него, без отправки.
    pub(crate) fn quick_reply(&mut self, row: EmailRow, p: Prepared, cx: &mut Context<Self>) {
        let reading = self.reading.read(cx);
        let mut ids: Vec<String> = vec![row.id.clone()];
        if let Some(f) = &reading.feed {
            ids.extend(f.conv.messages.iter().map(|m| m.row.id.clone()));
        }
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        if let Some((view, _)) = self.composer_for(&ids, cx) {
            let handle = self.window;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| view.update(cx, |c, cx| c.take_quick(p, window, cx)));
            });
            return;
        }
        self.fragments.pending = Some(p);
        self.respond_to(row, Mode::ReplyAll, cx);
    }

    /// Отправить ответ в области чтения, как только он откроется (быстрый ответ, Enter).
    pub(crate) fn send_inline_soon(&mut self, cx: &mut Context<Self>) {
        let mailer = cx.entity();
        cx.defer(move |cx| {
            mailer.update(cx, |m, cx| {
                if let Some(v) = m.compose.inline_view().cloned() {
                    v.update(cx, |c, cx| c.dispatch(crate::compose::Purpose::Send, cx));
                }
            })
        });
    }
}

impl Composer {
    /// Набранное в быстром ответе — в уже открытый ответ: фрагменты — репликами, текст — к своему тексту, упомянутые
    /// — в «Кому».
    fn take_quick(&mut self, p: Prepared, window: &mut Window, cx: &mut Context<Self>) {
        let text = p.text.trim().to_owned();
        let had = self.model.has_fragments();
        let n = p.fragments.len();
        for (i, mut f) in p.fragments.into_iter().enumerate() {
            if i + 1 == n && !text.is_empty() {
                f.answer.clone_from(&text);
            }
            self.add_fragment(f, window, cx);
        }
        if n == 0 && !text.is_empty() {
            let at = if had { self.model.body.len() } else { crate::fragments::tail_start(&self.model.body, &self.model.signature) };
            let (head, tail) = self.model.body.split_at(at);
            let body = format!("{}\n\n{text}\n\n{}", head.trim_end_matches('\n'), tail.trim_start_matches('\n'));
            self.model.set(crate::compose::Field::Body, body.trim_matches('\n').to_owned());
            self.sync_inputs = true;
        }
        for who in &p.mentioned {
            self.sync_inputs |= crate::mentions::add_recipient(&mut self.model, who);
        }
        self.schedule_autosave(cx);
        cx.notify();
    }
}

/// Опись части (ADR 0005, `parts.rs`): тексты и сцены быстрого ответа и упоминаний.
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "quick", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/quick.ftl")), ("en", include_str!("../i18n/en/quick.ftl"))];

/// Сцены `--screens` (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Input, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Ответ в области чтения на письмо беседы: «@Сок» в тексте — подсказка участников беседы под текстом.
        Scene::new("compose-mention", reply, composer_ready).after("reply-fragments").input(COMPOSE_MENTION),
        // Enter — «**@Соколова Мария**» в тексте, она же — в «Кому».
        Scene::new("compose-mention-pick", stay, compose_picked).continues().input(&[Input::Keys(&["enter"])]),
        // Лента беседы, быстрый ответ внизу: набрано «Коллеги, @Ив» — подсказка участников над полем.
        Scene::new("quick-mention", letter, quick_shown).input(QUICK_MENTION),
        // Enter выбирает человека, дописан текст, Enter — отправка «Ответить всем» через окно письма (в демо сервера
        // нет: ответ остаётся над письмом со строкой «нет сервера»).
        Scene::new("quick-send", stay, sent).continues().input(QUICK_SEND),
    ];

    const COMPOSE_MENTION: &[Input] = &[Input::Until(composer_ready), Input::Text("@Сок"), Input::Until(compose_open)]; // i18n-ok: ввод сценария
    const QUICK_MENTION: &[Input] = &[
        Input::Until(quick_shown),
        Input::Run(bottom),
        Input::Press("quick-reply"),
        Input::Text("Коллеги, @Ив"), // i18n-ok: ввод сценария
        Input::Until(quick_open),
    ];
    const QUICK_SEND: &[Input] = &[
        Input::Keys(&["enter"]),
        Input::Until(quick_picked),
        Input::Text("посмотрите итоги к пятнице."), // i18n-ok: ввод сценария
        Input::Keys(&["enter"]),
    ];

    /// Без ответа прошлых сцен над письмом: письмо беседы с картинкой и цитатой (как `reply-inline`).
    fn fresh(m: &mut Mailer, cx: &mut Context<Mailer>) -> Result<(), String> {
        if let Some(id) = m.compose.inline.as_ref().map(|i| i.id) {
            m.close_composer(id, cx);
        }
        let Some(i) = screens::demo_row(m.list.read(cx), screens::outlook_letter) else { return Err("no message".into()) };
        m.tabs.activate_kind(crate::tabs::TabKind::Main);
        m.set_section(Section::Mail, cx);
        m.list.update(cx, |l, cx| l.select(i, cx));
        Ok(())
    }

    fn letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        fresh(m, cx)
    }

    fn reply(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        fresh(m, cx)?;
        let mailer = cx.entity();
        cx.defer(move |cx| mailer.update(cx, |m, cx| m.respond(postman_compose::Mode::Reply, cx)));
        Ok(())
    }

    /// Лента — к быстрому ответу внизу.
    fn bottom(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.reading.update(cx, |r, cx| {
            r.scroll.scroll_to_bottom();
            cx.notify();
        });
        Ok(())
    }

    fn stay(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
        Ok(())
    }

    fn composer_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.compose.inline_view().is_some_and(|c| c.read(cx).drawn())
    }

    fn compose_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.compose.inline_view().is_some_and(|c| c.read(cx).mention.open())
    }

    fn compose_picked(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.compose.inline_view().is_some_and(|c| {
            let c = c.read(cx);
            c.model.body.contains("**@Соколова Мария**") && c.model.to.contains("Соколова") // i18n-ok: образец демо
        })
    }

    fn quick_shown(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).feed.as_ref().is_some_and(crate::thread::Feed::ready) && screens::spot_bounds("quick-reply").is_some()
    }

    fn quick_open(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).quick.mention.open()
    }

    fn quick_picked(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.reading.read(cx).quick.mentioned_count() == 1
    }

    fn sent(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.compose.inline_view().is_some_and(|c| {
            let c = c.read(cx);
            c.drawn() && c.model.body.contains("посмотрите итоги к пятнице.") && !matches!(c.model.status, crate::compose::Status::None) // i18n-ok: ввод сценария
        })
    }
}

#[cfg(test)]
mod tests {
    use postman_compose::fragments::Fragment;
    use postman_compose::{Address, Draft};

    use crate::fragments::{Pending, Prepared};

    /// Быстрый ответ: текст — над подписью, упомянутые — в «Кому», Enter — отправить; с фрагментами текст — ответ на
    /// последний; ждущее другого письма — не трогается.
    #[test]
    fn quick_reply_goes_into_the_draft() {
        let mut pending = Pending::default();
        let anna = Address::new("Анна", "anna@corp.example");
        pending.pending =
            Some(Prepared { letter: "m1".into(), text: "Согласен.".into(), mentioned: vec![anna.clone()], send: true, ..Default::default() });
        let mut d = Draft {
            markdown: "

Подпись"
                .into(),
            to: vec![Address::new("", "ivan@corp.example")],
            ..Default::default()
        };
        assert!(!pending.apply("m2", &mut d), "другое письмо");
        assert!(pending.apply("m1", &mut d), "Enter — отправить");
        assert_eq!(
            d.markdown,
            "Согласен.

Подпись"
        );
        assert_eq!(d.to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["ivan@corp.example", "anna@corp.example"]);
        assert!(pending.pending.is_none());
        let f = Fragment::new("m1", "Иван", "17.10.2026 12:00", "Сроки?");
        pending.pending = Some(Prepared { letter: "m1".into(), fragments: vec![f], text: "До пятницы.".into(), ..Default::default() });
        let mut d = Draft {
            markdown: "

Подпись"
                .into(),
            ..Default::default()
        };
        assert!(!pending.apply("m1", &mut d), "«Развернуть» — без отправки");
        assert_eq!(d.fragments[0].answer, "До пятницы.");
        assert_eq!(
            d.markdown,
            "

Подпись"
        );
    }
}
