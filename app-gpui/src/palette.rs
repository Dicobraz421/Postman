//! Палитра команд Ctrl+K (S5-10, `docs/DESIGN.md` «Палитра команд и поиск»): оверлей под заголовком окна поверх
//! командной строки, не модальное окно. Группы: Команды (реестр `commands.rs`, названия — `cmd-…`, клавиши — с
//! учётом своих), Папки (все аккаунты, общие ящики тоже), Быстрые фильтры, Письма (первые из поиска по своим данным).
//! Совпадение — нечёткое и в любой раскладке (`postman_core::fuzzy`); при пустом запросе недавние — первыми.
//! ↑/↓, Enter, Esc — команды `palette.*`; работает из любой вкладки: команда уходит туда, где был фокус.

use std::ops::Range;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use postman_cache::{Cache, EmailRow};
use postman_core::filters::Quick;
use postman_core::fuzzy::{Candidate, highlight_terms, rank, term_spans};
use postman_core::query::Query;

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::roles::{Look, look};

/// Ширина палитры (макет).
pub(crate) const WIDTH: f32 = 640.0;
const INPUT_H: f32 = 44.0;
const CAPTION_H: f32 = 28.0;
const ROW_H: f32 = 32.0;
const FOOTER_H: f32 = 32.0;
/// Поля списка сверху и снизу.
const PAD: f32 = 8.0;
/// Пауза ввода перед поиском писем.
const LETTERS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
/// Недавних — не больше.
pub(crate) const RECENT: usize = 12;

/// Группа строк палитры (порядок групп — порядок вариантов).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Command,
    Folder,
    Quick,
    Letter,
}

const KINDS: [Kind; 4] = [Kind::Command, Kind::Folder, Kind::Quick, Kind::Letter];

impl Kind {
    fn caption(self) -> String {
        tr!(match self {
            Kind::Command => "palette-group-commands",
            Kind::Folder => "palette-group-folders",
            Kind::Quick => "palette-group-quick",
            Kind::Letter => "palette-group-letters",
        })
    }

    /// Строк группы: при пустом запросе — и при наборе.
    fn limit(self, empty: bool) -> usize {
        match (self, empty) {
            (Kind::Command, true) => 7,
            (Kind::Command, false) => 6,
            (Kind::Folder, _) => 3,
            (Kind::Quick, _) => 5,
            (Kind::Letter, _) => 4,
        }
    }
}

/// Что сделать по строке.
#[derive(Clone, Debug)]
pub(crate) enum Act {
    Command(&'static str),
    Folder(String),
    Quick(Quick),
    Letter(Box<EmailRow>),
}

/// Значок строки: глиф Segoe Fluent Icons или цветная точка (быстрый фильтр).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mark {
    Glyph(char),
    Dot(Hsla),
}

/// Строка палитры.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    /// Ключ недавних: `cmd:mail.reply`, `folder:<id>`, `quick:<n>`.
    pub(crate) key: String,
    kind: Kind,
    pub(crate) label: String,
    /// Перед названием, вторичным цветом (родительская папка «Проекты ›»).
    path: String,
    /// Подпись (аккаунт, тема письма).
    extra: String,
    /// Справа: число писем, дата.
    meta: String,
    keys: Vec<String>,
    mark: Mark,
    act: Act,
}

impl Item {
    pub(crate) fn command(id: &'static str, label: String, keys: Vec<String>) -> Item {
        Item {
            key: format!("cmd:{id}"),
            kind: Kind::Command,
            label,
            path: String::new(),
            extra: String::new(),
            meta: String::new(),
            keys,
            mark: Mark::Glyph(command_icon(id).unwrap_or(glyph::CHEVRON_RIGHT)),
            act: Act::Command(id),
        }
    }

    pub(crate) fn folder(id: String, label: String, path: String, extra: String, meta: String, g: char) -> Item {
        Item {
            key: format!("folder:{id}"),
            kind: Kind::Folder,
            label,
            path,
            extra,
            meta,
            keys: Vec::new(),
            mark: Mark::Glyph(g),
            act: Act::Folder(id),
        }
    }

    pub(crate) fn quick(q: Quick, label: String, extra: String, n: usize, dot: Hsla) -> Item {
        let extra = if extra == label { String::new() } else { extra };
        Item {
            key: format!("quick:{q:?}"),
            kind: Kind::Quick,
            label,
            path: String::new(),
            extra,
            meta: if n > 0 { n.to_string() } else { String::new() },
            keys: Vec::new(),
            mark: Mark::Dot(dot),
            act: Act::Quick(q),
        }
    }

    fn letter(row: EmailRow) -> Item {
        let from = if row.from_name.is_empty() { row.from_addr.clone() } else { row.from_name.clone() };
        Item {
            key: format!("letter:{}", row.id),
            kind: Kind::Letter,
            label: from,
            path: String::new(),
            extra: row.subject.clone(),
            meta: crate::format_date(row.received_at),
            keys: Vec::new(),
            mark: Mark::Glyph(glyph::MAIL),
            act: Act::Letter(Box::new(row)),
        }
    }
}

/// Показанная строка: заголовок группы или строка с подсветкой названия и подписи.
enum Row {
    Caption(Kind),
    Item { item: Box<Item>, label: Vec<Range<usize>>, extra: Vec<Range<usize>> },
}

/// Открытая палитра: view, куда вернуть фокус, подписка на её события.
pub(crate) struct Open {
    pub(crate) view: Entity<Palette>,
    back: Option<FocusHandle>,
    _events: Subscription,
}

pub(crate) enum PaletteEvent {
    Run(Act),
    Close,
    /// Высота изменилась: экрану — переложить оверлей.
    Resized,
}

impl EventEmitter<PaletteEvent> for Palette {}

pub(crate) struct Palette {
    pub(crate) input: Entity<InputState>,
    items: Vec<Item>,
    letters: Vec<Item>,
    rows: Vec<Row>,
    /// Выбранная строка — номер среди строк с действием (без заголовков).
    selected: usize,
    recent: Vec<String>,
    cache: Cache,
    index: postman_core::sync::Index,
    typed: u64,
    /// Высота по содержимому (кэшированной view размер задаёт экран).
    pub(crate) height: f32,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    pub(crate) fn new(
        items: Vec<Item>,
        recent: Vec<String>,
        cache: Cache,
        index: postman_core::sync::Index,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(tr!("palette-placeholder")));
        let subscriptions = vec![cx.subscribe(&input, |this: &mut Self, _, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                this.typed(cx);
            }
        })];
        let mut p = Self {
            input,
            items,
            letters: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            recent,
            cache,
            index,
            typed: 0,
            height: 0.,
            _subscriptions: subscriptions,
        };
        p.refresh(cx);
        p
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Набор: строки — сразу, письма — после паузы, в фоне.
    pub(crate) fn typed(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        self.refresh(cx);
        self.typed += 1;
        let typed = self.typed;
        let text = Query::parse(&self.query(cx)).text();
        if text.is_empty() {
            self.letters.clear();
            return self.refresh(cx);
        }
        let (cache, index) = (self.cache.clone(), self.index.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LETTERS_DEBOUNCE).await;
            let alive = this.update(cx, |p, _| p.typed == typed).unwrap_or(false);
            if !alive {
                return;
            }
            let limit = Kind::Letter.limit(false);
            let task = cx.background_spawn(async move {
                let mut ids = crate::live::local_search(&index, &cache, &text, None);
                // Набрано не в той раскладке, а индекса нет (демо, кэш): то же в другой.
                if ids.is_empty()
                    && let Some(other) = postman_core::fuzzy::other_layout(&text)
                {
                    ids = crate::live::local_search(&index, &cache, &other, None);
                }
                let ids: Vec<String> = ids.into_iter().take(limit).collect();
                cache.rows_by_ids(&ids).unwrap_or_default()
            });
            let rows = task.await;
            let _ = this.update(cx, |p, cx| {
                if p.typed == typed {
                    p.letters = rows.into_iter().map(Item::letter).collect();
                    p.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// Строки по запросу: группы по порядку, в группе — по силе совпадения, недавние — выше.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        let empty = q.trim().is_empty();
        let marks = highlight_terms(&Query::parse(&q).words);
        let mut rows = Vec::new();
        for kind in KINDS {
            let pool: Vec<&Item> = match kind {
                Kind::Letter if empty => Vec::new(),
                Kind::Letter => self.letters.iter().collect(),
                _ => self.items.iter().filter(|i| i.kind == kind).collect(),
            };
            let picked: Vec<Row> = if kind == Kind::Letter {
                // Письма уже найдены поиском: тема — с подсветкой совпадений.
                pool.iter().map(|i| Row::Item { item: Box::new((*i).clone()), label: Vec::new(), extra: term_spans(&i.extra, &marks) }).collect()
            } else {
                let cands: Vec<Candidate<'_>> = pool.iter().map(|i| Candidate { key: &i.key, text: &i.label, extra: &i.extra }).collect();
                rank(&q, &cands, &self.recent)
                    .into_iter()
                    .take(kind.limit(empty))
                    .map(|r| {
                        let spans = r.hit.map(|m| m.spans).unwrap_or_default();
                        let (label, extra) = if r.in_extra { (Vec::new(), spans) } else { (spans, Vec::new()) };
                        Row::Item { item: Box::new(pool[r.index].clone()), label, extra }
                    })
                    .collect()
            };
            if !picked.is_empty() {
                rows.push(Row::Caption(kind));
                rows.extend(picked);
            }
        }
        self.rows = rows;
        let n = self.count();
        if self.selected >= n {
            self.selected = n.saturating_sub(1);
        }
        let captions = self.rows.iter().filter(|r| matches!(r, Row::Caption(_))).count() as f32;
        let body = if n == 0 { ROW_H } else { captions * CAPTION_H + n as f32 * ROW_H };
        let height = INPUT_H + PAD + body + FOOTER_H;
        if height != self.height {
            self.height = height;
            cx.emit(PaletteEvent::Resized);
        }
        cx.notify();
    }

    /// Строк с действием (снимки экранов).
    pub(crate) fn shown(&self) -> usize {
        self.count()
    }

    /// Письма по запросу пришли и показаны (снимки экранов).
    pub(crate) fn has_letters(&self) -> bool {
        self.rows.iter().any(|r| matches!(r, Row::Item { item, .. } if item.kind == Kind::Letter))
    }

    /// Строк с действием.
    fn count(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r, Row::Item { .. })).count()
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.count() as isize;
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
            cx.notify();
        }
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        let item = self.rows.iter().filter_map(|r| if let Row::Item { item, .. } = r { Some(item) } else { None }).nth(index);
        if let Some(item) = item {
            cx.emit(PaletteEvent::Run(item.act.clone()));
        }
    }
}

/// Текст с жирными (или янтарными — письма) совпадениями.
fn spans_text(text: &str, spans: &[Range<usize>], style: HighlightStyle) -> AnyElement {
    if spans.is_empty() {
        return text.to_owned().into_any_element();
    }
    StyledText::new(text.to_owned()).with_highlights(spans.iter().map(|r| (r.clone(), style))).into_any_element()
}

/// Клавиша плашкой.
pub(crate) fn kbd(text: String, l: &Look) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .line_height(px(16.))
        .rounded(l.px(Metric::RadiusControl))
        .border_1()
        .border_color(l.stroke())
        .text_size(px(11.))
        .text_color(l.text2())
        .child(text)
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = look(cx);
        let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
        let amber = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..crate::search::hit_style(&l) };
        let n = self.count();
        let head = div()
            .h(px(INPUT_H))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(14.))
            .pr(px(12.))
            .border_b_1()
            .border_color(l.border())
            .child(icon(glyph::SEARCH, 16.).text_color(l.text3()))
            .child(div().flex_1().min_w_0().text_size(px(14.)).child(Input::new(&self.input).appearance(false)))
            .when(!self.query(cx).trim().is_empty(), |d| {
                d.child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("palette-count", n = n)))
            })
            .child(kbd("Esc".into(), &l));
        let mut list = div().id("palette-list").flex_1().min_h_0().overflow_y_scroll().px(px(6.)).pt(px(2.)).pb(px(6.)).flex().flex_col();
        let mut at = 0;
        for row in &self.rows {
            match row {
                Row::Caption(kind) => {
                    list = list.child(
                        div()
                            .h(px(CAPTION_H))
                            .flex_none()
                            .flex()
                            .items_end()
                            .pb(px(4.))
                            .px(px(10.))
                            .text_size(l.px(Metric::TypeCaption))
                            .text_color(l.text2())
                            .child(kind.caption()),
                    )
                }
                Row::Item { item, label, extra } => {
                    let index = at;
                    at += 1;
                    let on = index == self.selected;
                    let mark = match item.mark {
                        Mark::Glyph(g) => icon(g, 14.).text_color(l.text2()).into_any_element(),
                        Mark::Dot(c) => div().size(px(8.)).rounded(px(2.)).bg(c).into_any_element(),
                    };
                    let letter = item.kind == Kind::Letter;
                    let keys = item.keys.iter().map(|k| kbd(k.clone(), &l));
                    list = list.child(
                        div()
                            .id(("palette-row", index))
                            .relative()
                            .h(px(ROW_H))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .pl(px(12.))
                            .pr(px(10.))
                            .rounded(l.px(Metric::RadiusControl))
                            .cursor_pointer()
                            .when(on, |d| d.bg(l.pressed()))
                            .when(!on, |d| d.hover(|d| d.bg(l.hover())))
                            .child(div().absolute().left_0().top(px(7.)).bottom(px(7.)).w(px(3.)).rounded(px(2.)).when(on, |d| d.bg(l.accent())))
                            .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().child(mark))
                            .child(
                                div()
                                    .flex_initial()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .when(!item.path.is_empty(), |d| d.child(div().flex_none().text_color(l.text2()).child(item.path.clone())))
                                    .child(div().min_w_0().truncate().child(spans_text(&item.label, label, bold))),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(spans_text(
                                &item.extra,
                                extra,
                                if letter { amber } else { bold },
                            )))
                            .when(!item.meta.is_empty(), |d| {
                                d.child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(item.meta.clone()))
                            })
                            .child(div().flex_none().flex().gap(px(4.)).children(keys))
                            .on_click(cx.listener(move |this, _, _, cx| this.run(index, cx))),
                    );
                }
            }
        }
        if n == 0 {
            list = list.child(div().h(px(ROW_H)).flex().items_center().px(px(12.)).text_color(l.text2()).child(tr!("palette-nothing")));
        }
        let footer = div()
            .h(px(FOOTER_H))
            .flex_none()
            .flex()
            .items_center()
            .px(px(14.))
            .border_t_1()
            .border_color(l.border())
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .child(tr!("palette-hint"));
        div()
            .id("palette")
            .key_context("Palette")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(l.px(Metric::RadiusOverlay))
            .border_1()
            .border_color(l.c(Role::StrokeCard))
            .bg(l.c(Role::LayerOverlay))
            .shadow_lg()
            .text_size(px(13.))
            .text_color(l.text())
            .on_action(cx.listener(|this, _: &palette::Next, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &palette::Prev, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &palette::Run, _, cx| {
                let i = this.selected;
                this.run(i, cx);
            }))
            .on_action(cx.listener(|_, _: &palette::Close, _, cx| cx.emit(PaletteEvent::Close)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Close)))
            .child(head)
            .child(list)
            .child(footer)
    }
}

// --- экран почты ---

/// Значок команды в палитре (S5-14): глиф Segoe Fluent Icons по id реестра; команды без своего — шеврон.
/// Таблица — здесь, а не в реестре `pets-commands`: значок — дело экрана, реестр общий для приложений Pets.
pub(crate) fn command_icon(id: &str) -> Option<char> {
    use glyph::*;
    Some(match id {
        "mail.next" | "mail.page_down" | "mail.select_down" => DOWN,
        "mail.prev" | "mail.page_up" | "mail.select_up" => UP,
        "mail.first" => CHEVRON_UP,
        "mail.last" => CHEVRON_DOWN,
        "mail.mark_read" => READ,
        "mail.mark_unread" | "search.unread" | "view.mail" | "contacts.write" | "tab.new" => MAIL,
        "mail.toggle_flag" => FLAG,
        "mail.categorize" => TAG,
        "mail.new" => NEW_MAIL,
        "mail.reply" => REPLY,
        "mail.reply_all" => REPLY_ALL,
        "mail.forward" => FORWARD,
        "mail.forward_attached" => ATTACH,
        "folder.new" => FOLDER,
        "folder.rename" => EDIT,
        "mail.delete" | "compose.discard" | "meeting.discard" | "contacts.delete" => DELETE,
        "mail.due" => CLOCK,
        "mail.remind" | "view.notifications" => RINGER,
        "mail.wait" => WAIT,
        "mail.expire" => EXPIRE,
        "mail.select_all" => SELECT_ALL,
        "mail.clear_selection" | "search.clear" | "filters.reset" => CLEAR,
        "mail.context_menu" | "contacts.context_menu" => MORE,
        "mail.move" => MOVE,
        "mail.archive" => ARCHIVE,
        "mail.report_phishing" => PHISHING,
        "thread.toggle" | "thread.fold" | "thread.expand" | "thread.collapse" => CONVERSATION,
        "thread.quotes" | "compose.quote" => QUOTE,
        "thread.next" | "attach.next" | "tab.next" | "calendar.next_week" | "meeting.next_day" => CHEVRON_RIGHT,
        "thread.prev" | "attach.prev" | "tab.prev" | "calendar.prev_week" | "meeting.prev_day" => CHEVRON_LEFT,
        "attach.preview" => PREVIEW,
        "attach.save_all" => SAVE,
        "view.calendar" | "calendar.new_meeting" | "contacts.meeting" => CALENDAR,
        "view.contacts" => CONTACTS,
        "view.tasks" | "task.new" | "task.complete" | "task.open" | "task.next" | "task.prev" => crate::tasks::g::TASKS,
        "task.due" => crate::tasks::g::FLAG,
        "task.slot_next" | "task.slot_prev" | "task.block" => crate::calendar::g::CALENDAR,
        "task.edit" => crate::tasks::g::EDIT,
        "task.delete" => crate::tasks::g::DELETE,
        "task.delegate" => crate::tasks::g::DELEGATE,
        "task.remind" | "task.remind_all" => crate::tasks::g::RINGER,
        "task.revoke" => crate::tasks::g::REVOKE,
        "tab.close" | "compose.close" | "meeting.close" | "invite.decline" => CLOSE,
        "search.focus" | "palette.open" => SEARCH,
        "search.next_match" => DOWN,
        "search.prev_match" => UP,
        "search.all" => GLOBE,
        "search.folder" => FOLDER,
        "search.attachments" | "compose.attach" => ATTACH,
        "search.recent" => HISTORY,
        "calendar.today" | "meeting.today" => TODAY,
        "compose.send" | "meeting.send" => SEND,
        "compose.bold" => BOLD,
        "compose.italic" => ITALIC,
        "compose.underline" => UNDERLINE,
        "compose.bullets" | "compose.numbers" => BULLETS,
        "compose.link" => LINK,
        "compose.expand" => EXPAND,
        "contacts.new" | "meeting.add_person" => ADD_CONTACT,
        "contacts.edit" => EDIT,
        "contacts.copy_address" => COPY,
        "invite.accept" => CHECK,
        "invite.tentative" => TENTATIVE,
        "settings.open" => SETTINGS,
        "mail.signature" => CERTIFICATE,
        "mail.take" => CONTACTS,
        "settings.vacation" => VACATION,
        "settings.rules" => RULES,
        "settings.calendar" => crate::calendar::g::CALENDAR,
        // Виды календаря (S5-21): «День» и «Неделя» Segoe Fluent Icons, месяц — календарь.
        "calendar.view_day" => '\u{E8BF}',
        "calendar.view_work_week" | "calendar.view_week" => '\u{E8C0}',
        "calendar.view_month" => crate::calendar::g::CALENDAR,
        "view.filters" | "view.quick_filters" => FILTER,
        "view.sort_by_date" | "view.sort_by_sender" | "view.sort_by_size" => SORT,
        _ => return None,
    })
}

/// Команды, которых в палитре нет: сама палитра, клавиши с данными (категория — клавишей 1–9) и шаги по списку и
/// вложениям и окнам встречи — это клавиши, а не действия.
fn hidden(id: &str) -> bool {
    const STEPS: [&str; 13] = [
        "mail.next",
        "mail.prev",
        "mail.first",
        "mail.last",
        "mail.page_down",
        "mail.page_up",
        "attach.next",
        "attach.prev",
        "meeting.next_slot",
        "meeting.prev_slot",
        "meeting.pick_slot",
        "meeting.longer",
        "meeting.shorter",
    ];
    id.starts_with("palette.") || id == "mail.categorize" || STEPS.contains(&id)
}

impl Mailer {
    /// `palette.open` (Ctrl+K): открыть; открыта — закрыть.
    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return self.close_palette(window, cx);
        }
        // Команды — те, что сработали бы там, где сейчас фокус (письмо во вкладке, список, календарь).
        let back = window.focused(cx);
        let set = crate::keymap::KeySet::of(&crate::prefs::get(cx));
        let mut items = Vec::new();
        let features = crate::features::get(cx);
        for c in crate::commands::all().iter().filter(|c| !hidden(c.id) && crate::features::available(c.id, features)) {
            let Ok(action) = cx.build_action(&c.action_name(), None) else { continue };
            if !window.is_action_available(action.as_ref(), cx) {
                continue;
            }
            let keys = crate::keymap::hint(&crate::commands::SINGLE, &set, c).map(|k| crate::keymap::human(&k)).into_iter().collect();
            items.push(Item::command(c.id, tr!(&c.name_key()), keys));
        }
        items.extend(self.palette_folders(cx));
        let counts = self.filters.read(cx).counts.clone();
        let l = look(cx);
        for q in Self::quick_tabs(cx) {
            let (short, long) = crate::filters::quick_keys(q);
            items.push(Item::quick(q, tr!(short), tr!(long), counts.quick(q), crate::filters::quick_color(q, &l)));
        }
        let (cache, index, recent) = (self.cache.clone(), self.live.index(), self.recent.clone());
        let view = cx.new(|cx| Palette::new(items, recent, cache, index, window, cx));
        let events = cx.subscribe_in(&view, window, |this, _, ev: &PaletteEvent, window, cx| match ev {
            PaletteEvent::Run(a) => this.run_palette(a.clone(), window, cx),
            PaletteEvent::Close => this.close_palette(window, cx),
            PaletteEvent::Resized => cx.notify(),
        });
        let input = view.read(cx).input.clone();
        input.update(cx, |s, cx| s.focus(window, cx));
        self.palette = Some(Open { view, back, _events: events });
        cx.notify();
    }

    /// Папки всех аккаунтов: путь — родительские папки, подпись — общий ящик (свой — без подписи).
    fn palette_folders(&self, cx: &App) -> Vec<Item> {
        let f = self.folders.read(cx);
        let name = |id: &str| f.mailboxes.iter().find(|m| m.id == id).map(|m| m.name.clone());
        // Как в дереве: свои папки, затем общие ящики.
        let tree: Vec<&postman_cache::MailboxRow> = f.of(None).chain(f.accounts.iter().flat_map(|a| f.of(Some(a.id.as_str())))).collect();
        tree.into_iter()
            .map(|m| {
                let mut path = Vec::new();
                let mut parent = m.parent_id.clone();
                while let Some(p) = parent.filter(|_| path.len() < 8) {
                    let row = f.mailboxes.iter().find(|x| x.id == p);
                    path.insert(0, name(&p).unwrap_or_default());
                    parent = row.and_then(|r| r.parent_id.clone());
                }
                let path = if path.is_empty() { String::new() } else { format!("{} ›", path.join(" › ")) };
                let account =
                    postman_cache::account_of(&m.id).and_then(|a| f.accounts.iter().find(|x| x.id == a)).map(|a| a.name.clone()).unwrap_or_default();
                let meta = if m.total > 0 { m.total.to_string() } else { String::new() };
                Item::folder(m.id.clone(), m.name.clone(), path, account, meta, crate::folders::folder_glyph(m.role.as_deref()))
            })
            .collect()
    }

    /// Закрыть палитру: фокус — туда, где был.
    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.palette.take() else { return };
        match open.back {
            Some(h) => window.focus(&h, cx),
            None => window.focus(&self.focus, cx),
        }
        cx.notify();
    }

    fn run_palette(&mut self, a: Act, window: &mut Window, cx: &mut Context<Self>) {
        let key = match &a {
            Act::Command(id) => Some(format!("cmd:{id}")),
            Act::Folder(id) => Some(format!("folder:{id}")),
            Act::Quick(q) => Some(format!("quick:{q:?}")),
            Act::Letter(_) => None,
        };
        if let Some(k) = key {
            self.recent.retain(|r| *r != k);
            self.recent.insert(0, k);
            self.recent.truncate(RECENT);
        }
        self.close_palette(window, cx);
        match a {
            // Действие — в фокус, который палитра вернула: как от клавиши там же.
            Act::Command(id) => {
                let Some(c) = pets_commands::find(crate::commands::all(), id) else { return };
                match cx.build_action(&c.action_name(), None) {
                    Ok(action) => window.dispatch_action(action, cx),
                    Err(e) => tracing::warn!("palette: {id}: {e}"),
                }
            }
            Act::Folder(id) => {
                self.open_mail_tab(window, cx);
                self.open_mailbox(id, cx);
            }
            Act::Quick(q) => {
                self.open_mail_tab(window, cx);
                self.filters.update(cx, |m, cx| m.change(cx, |x| x.criteria.toggle_quick(q)));
            }
            Act::Letter(row) => {
                self.open_mail_tab(window, cx);
                self.open_letter(*row, cx);
            }
        }
    }

    /// Папка, фильтр или письмо из палитры — во вкладке «Почта», без поиска.
    fn open_mail_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_section(crate::Section::Mail, cx);
        if self.find.active() {
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
            self.end_search(cx);
        }
        window.focus(&self.focus, cx);
    }

    /// Палитра поверх окна: под заголовком, по центру.
    pub(crate) fn palette_overlay(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let p = &self.palette.as_ref()?.view;
        let l = look(cx);
        let height = p.read(cx).height;
        let width = f32::from(window.viewport_size().width);
        let left = ((width - WIDTH) / 2.).max(8.);
        let top = l.m(Metric::HTitlebar) - 2.;
        Some(
            div()
                .absolute()
                .top(px(top))
                .left(px(left))
                .w(px(WIDTH))
                .h(px(height))
                .child(p.clone().cached(StyleRefinement::default().size_full()))
                .into_any_element(),
        )
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "palette", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &palette::Open, window, cx| this.toggle_palette(window, cx)))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/palette.ftl")), ("en", include_str!("../i18n/en/palette.ftl"))];

/// Палитра команд (S5-10): открыть, по строкам, выполнить, закрыть.
// Имя — область действий (`palette::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod palette {
    gpui_kit::actions!(palette, [Open, Next, Prev, Run, Close]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAILER, PALETTE, cmd};
    &[
        // Не Outlook (там Ctrl+K — «Проверить имена» в письме): палитра команд, как в новом Outlook и редакторах.
        cmd("palette.open", &["ctrl-k"], MAILER),
        cmd("palette.next", &["down"], PALETTE),
        cmd("palette.prev", &["up"], PALETTE),
        cmd("palette.run", &["enter"], PALETTE),
        cmd("palette.close", &["escape"], PALETTE),
    ]
};

/// Сцены `--screens` палитры (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::Mailer;
    use crate::screens::Scene;

    pub(crate) const SCENES: &[Scene] = &[
        // Палитра команд Ctrl+K с пустым запросом: недавние — первыми.
        Scene::new("palette", open, open_ready),
        // Палитра с запросом в другой раскладке.
        Scene::new("palette-query", query, query_ready).continues(),
    ];

    /// Палитра с запросом: «от», набранное в английской раскладке, — команды, папки и письма (S5-10).
    const QUERY: &str = "jn";

    fn open(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Недавние — как после трёх запусков из палитры.
        m.recent = vec!["cmd:mail.due".into(), "folder:archive".into(), "cmd:mail.reply_all".into()];
        m.toggle_palette(window, cx);
        Ok(())
    }

    fn open_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.palette.as_ref().is_some_and(|p| p.view.read(cx).shown() > 0)
    }

    fn query(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let Some(p) = m.palette.as_ref().map(|p| p.view.clone()) else { return Err("palette is not open".into()) };
        let input = p.read(cx).input.clone();
        input.update(cx, |s, cx| s.set_value(QUERY, window, cx));
        // Как набор: `set_value` не шлёт `Change`.
        p.update(cx, |p, cx| p.typed(cx));
        Ok(())
    }

    fn query_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.palette.as_ref().is_some_and(|p| p.view.read(cx).has_letters())
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{command_icon, hidden};

    /// Каждая команда палитры — со своим значком (S5-14), а не общим шевроном: новая команда — строкой в
    /// `command_icon`.
    #[test]
    fn palette_commands_have_icons() {
        for c in crate::commands::all().iter().filter(|c| !hidden(c.id)) {
            assert!(command_icon(c.id).is_some(), "{}: нет значка в палитре (command_icon)", c.id);
        }
    }
}
