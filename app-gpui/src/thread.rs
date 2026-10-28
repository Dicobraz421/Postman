//! Беседа в области чтения (S5-05). Вид — из настроек «Чтение», свой у папки (`Reading::conversation_in`):
//! - лента (по умолчанию): переключатель «Беседа (N)» и лента писем цепочки — только новый текст (границу истории
//!   ставит воркер-песочница), разделители по дням, свои письма отмечены и сдвинуты вправо; цитаты — переключателем;
//! - стопка (S5-05б, макет «Беседа»): письма карточками от старых к новым; прочитанные свёрнуты строкой (отправитель,
//!   начало текста, дата), последнее и непрочитанные раскрыты; щелчок или Enter — раскрыть, цитата — переключателем;
//! - «как Outlook» (S5-05б): беседа — в списке (`outline.rs`), здесь — одно письмо и строка «Беседа: N писем · ← · →».
//!
//! Ctrl+→ / Ctrl+← — по письмам беседы в любом виде. Письма беседы — из кэша (индекс цепочек, все папки); живой ящик
//! при первом показе ленты или стопки просит беседу у сервера (`Command::Thread` → `Event::ThreadReady`): письма из
//! папок за пределами первой страницы и недостающие тела.

use std::collections::{HashMap, HashSet};
use std::sync::PoisonError;

use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::tz::TimeZone;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_cache::{BodyRow, EmailRow};
use postman_compose::Mode;
use postman_core::prefs::Conversation as View;
use postman_core::thread::{self as feed, Conversation, Item};
use postman_ir::Document;
use postman_worker::Request;

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::ir_view::{self, Images};
use crate::outline::Fold;
use crate::reading::{Reading, ReadingEvent, ReadingPane, SyncRequest};

/// Лента беседы открытого письма.
pub(crate) struct Feed {
    pub(crate) conv: Conversation,
    /// Разобранные воркером письма (полный IR с границей истории), по ключу письма.
    pub(crate) docs: HashMap<String, Document>,
    /// Разбор в воркере идёт; `again` — за это время пришли новые тела, разобрать и их.
    rendering: bool,
    again: bool,
    /// Беседу уже просили у сервера; `waiting` — ответа ещё нет.
    fetched: bool,
    pub(crate) waiting: bool,
    /// Стопка (S5-05б): раскрытые письма, письма с показанной цитатой, «Кому» каждого письма (из тела).
    pub(crate) open: HashSet<String>,
    pub(crate) quoted: HashSet<String>,
    pub(crate) to: HashMap<String, String>,
}

impl Feed {
    fn new(conv: Conversation) -> Self {
        Self {
            conv,
            docs: HashMap::new(),
            rendering: false,
            again: false,
            fetched: false,
            waiting: false,
            open: HashSet::new(),
            quoted: HashSet::new(),
            to: HashMap::new(),
        }
    }

    /// Раскрыты в стопке: последнее письмо, непрочитанные и открытое (`current`).
    fn unfold_fresh(&mut self, current: Option<&str>) {
        let last = self.conv.messages.last().map(|m| m.row.id.clone());
        let unread = self.conv.messages.iter().filter(|m| !m.row.seen).map(|m| m.row.id.clone());
        self.open.extend(last.into_iter().chain(unread).chain(current.map(str::to_owned)));
    }

    /// Участники: разные отправители; меня нет среди отправителей — я получатель, тоже участник.
    fn people(&self) -> usize {
        let senders: HashSet<String> = self.conv.messages.iter().map(|m| m.row.from_addr.to_lowercase()).collect();
        senders.len() + usize::from(!self.conv.messages.iter().any(|m| m.mine))
    }

    /// Все письма ленты разобраны (снимок `thread`).
    pub(crate) fn ready(&self) -> bool {
        self.conv.messages.iter().all(|m| self.docs.contains_key(&m.row.id))
    }
}

/// Тело письма — запрос воркеру (HTML, иначе текст).
fn request(body: BodyRow) -> Request {
    match (body.html, body.text) {
        (Some(h), _) => Request::Html(h),
        (None, Some(t)) => Request::Text(t),
        _ => Request::Text(String::new()),
    }
}

impl ReadingPane {
    /// Мои адреса: Identity своего аккаунта и общих ящиков (`Event::Senders`), адрес учётной записи (`Event::Me`).
    fn my_addresses(&self) -> Vec<String> {
        self.mine.iter().cloned().chain(self.me.clone()).collect()
    }

    /// Открыто письмо: беседа — из кэша (в фоне). Та же беседа — лента остаётся, меняется текущее письмо.
    pub(crate) fn sync_thread(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &self.reading else { return };
        let key = r.row.thread_id.clone();
        if let Some(f) = self.feed.as_mut().filter(|f| f.conv.thread == key) {
            // Стопка: открытое письмо (Ctrl+→, щелчок по строке) — раскрыто.
            f.open.insert(r.row.id.clone());
            return;
        }
        self.feed = None;
        self.reload_thread(key, cx);
    }

    fn reload_thread(&mut self, key: String, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let rows = cache.thread(&key);
            (key, rows)
        });
        cx.spawn(async move |this, cx| {
            let (key, rows) = task.await;
            this.update(cx, |this, cx| match rows {
                Ok(rows) => this.set_conversation(&key, rows, cx),
                Err(e) => tracing::warn!("thread {key} is not read from the cache: {e}"),
            })
            .ok();
        })
        .detach();
    }

    fn set_conversation(&mut self, key: &str, rows: Vec<EmailRow>, cx: &mut Context<Self>) {
        if self.reading.as_ref().is_none_or(|r| r.row.thread_id != key) {
            return;
        }
        let conv = Conversation::assemble(key, rows, &self.my_addresses(), &TimeZone::system());
        let current = self.reading.as_ref().map(|r| r.row.id.clone());
        match &mut self.feed {
            Some(f) if f.conv.thread == key => f.conv = conv,
            // Одно письмо — не беседа: переключателя нет.
            _ if conv.len() < 2 => return,
            _ => self.feed = Some(Feed::new(conv)),
        }
        if let Some(f) = &mut self.feed {
            f.unfold_fresh(current.as_deref());
        }
        if self.feed_on || self.view(cx) == View::Stack {
            self.load_feed(cx);
        }
        cx.notify();
    }

    /// Вид беседы в папке письма (настройки «Чтение», свой у папки — поверх общего).
    pub(crate) fn view(&self, cx: &App) -> View {
        crate::prefs::get(cx).reading.conversation_in(self.folder.as_deref())
    }

    /// Стопка показана: беседа из 2+ писем, вид — стопка, просмотр вложения закрыт.
    pub(crate) fn stacked(&self, cx: &App) -> bool {
        self.feed.is_some() && self.view(cx) == View::Stack && self.reading.as_ref().is_some_and(|r| r.preview.is_none())
    }

    /// Вид беседы сменился (настройки, меню папки): стопке нужны разобранные письма.
    pub(crate) fn view_changed(&mut self, cx: &mut Context<Self>) {
        if self.feed.is_some() && self.view(cx) == View::Stack {
            self.load_feed(cx);
        }
        cx.notify();
    }

    /// «Беседа (N)»: лента вместо одного письма и обратно; выбор помнится для следующих писем.
    pub(crate) fn toggle_feed(&mut self, cx: &mut Context<Self>) {
        if self.feed.is_none() {
            return;
        }
        self.feed_on = !self.feed_on;
        if self.feed_on {
            self.load_feed(cx);
        }
        cx.notify();
    }

    /// «Показать цитаты» в ленте.
    pub(crate) fn toggle_quotes(&mut self, cx: &mut Context<Self>) {
        self.quotes = !self.quotes;
        cx.notify();
    }

    /// Беседа пришла с сервера: перечитать письма из кэша и разобрать новые тела.
    pub(crate) fn thread_ready(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(f) = self.feed.as_mut().filter(|f| f.conv.thread == key) else { return };
        f.waiting = false;
        self.reload_thread(key.to_owned(), cx);
    }

    /// Запрос беседы не ушёл (сервера нет): не ждать.
    pub(crate) fn thread_not_sent(&mut self) {
        if let Some(f) = &mut self.feed {
            f.waiting = false;
        }
    }

    /// Разобрать в воркере письма ленты, у которых есть тело; живой ящик — один раз спросить беседу у сервера.
    fn load_feed(&mut self, cx: &mut Context<Self>) {
        let live = self.live;
        let Some(f) = &mut self.feed else { return };
        if live && !f.fetched {
            f.fetched = true;
            f.waiting = true;
            cx.emit(ReadingEvent::Sync(SyncRequest::Thread(f.conv.thread.clone())));
        }
        if f.rendering {
            f.again = true;
            return;
        }
        let ids: Vec<String> = f.conv.messages.iter().map(|m| m.row.id.clone()).filter(|id| !f.docs.contains_key(id)).collect();
        if ids.is_empty() {
            return;
        }
        f.rendering = true;
        let (cache, worker, key) = (self.cache.clone(), self.worker.clone(), f.conv.thread.clone());
        let task = cx.background_spawn(async move {
            let mut out = Vec::new();
            for id in ids {
                let body = if live { cache.body(&id).ok().flatten() } else { Some(postman_demo::body(&id)) };
                // Тела нет — придёт с сервера (`ThreadReady`).
                let Some(body) = body else { continue };
                let to = body.to.iter().map(|a| if a.name.is_empty() { a.email.clone() } else { a.name.clone() }).collect::<Vec<_>>();
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                out.push((id, w.render(&request(body)), to.join(", ")));
            }
            out
        });
        cx.spawn(async move |this, cx| {
            let docs = task.await;
            this.update(cx, |this, cx| {
                let Some(f) = this.feed.as_mut().filter(|f| f.conv.thread == key) else { return };
                for (id, doc, to) in docs {
                    let doc = doc.unwrap_or_else(|e| postman_ir::from_plain(&tr!("reading-error", error = e)));
                    f.docs.insert(id.clone(), doc);
                    f.to.insert(id, to);
                }
                f.rendering = false;
                if std::mem::take(&mut f.again) {
                    this.load_feed(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Соседнее письмо беседы от открытого (Ctrl+→ / Ctrl+←).
    /// Ctrl+→ / Ctrl+← — по порядку на экране (T1-16б): при «сначала новые» вперёд — к более старому.
    pub(crate) fn thread_neighbor(&self, delta: isize, newest: bool) -> Option<EmailRow> {
        let id = &self.reading.as_ref()?.row.id;
        self.feed.as_ref()?.conv.neighbor(id, screen_step(delta, newest)).cloned()
    }

    /// Переключатель порядка в строке беседы (T1-16б): та же настройка, что в «Настройки → Чтение».
    fn order_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let l = crate::roles::look(cx);
        let newest = newest_first(cx);
        crate::buttons::secondary("thread-order", &l)
            .h(px(26.))
            .px_2()
            .child(tr!(if newest { "thread-order-newest" } else { "thread-order-oldest" }))
            .on_click(cx.listener(|_, _, _, cx| {
                crate::prefs::update(cx, |p| p.reading.thread_order = flip(p.reading.thread_order));
                cx.notify();
            }))
    }

    /// Ряд над письмом: «Беседа (N)», в ленте — «Показать цитаты»; у письма без беседы — ничего.
    pub(crate) fn feed_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let f = self.feed.as_ref()?;
        // Переключатели — тонированные кнопки (S5-19); включённый — с рамкой акцента.
        let button = |id: &'static str, label: String, on: bool| {
            crate::buttons::secondary(id, &l).h(px(26.)).px_2().border_1().border_color(if on { l.accent() } else { l.none() }).child(label)
        };
        let quotes = if self.quotes { tr!("thread-quotes-hide") } else { tr!("thread-quotes-show") };
        let bar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                button("thread-toggle", tr!("thread-toggle", n = f.conv.len()), self.feed_on)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_feed(cx))),
            )
            .when(self.feed_on, |d| {
                d.child(button("thread-quotes", quotes, self.quotes).on_click(cx.listener(|this, _, _, cx| this.toggle_quotes(cx))))
            })
            .when(f.waiting, |d| d.child(div().text_color(l.text2()).child(tr!("thread-loading"))))
            .child(div().flex_1())
            // Порядок писем (T1-16б) — у ленты, когда она показана.
            .when(self.feed_on, |d| d.child(self.order_button(cx)));
        Some(bar.into_any_element())
    }

    /// Лента: разделители дней и письма — только новый текст (пересылка и «показать цитаты» — целиком).
    pub(crate) fn feed_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let f = self.feed.as_ref().filter(|_| self.feed_on && self.view(cx) == View::Feed)?;
        let current = self.reading.as_ref().map(|r| r.row.id.as_str());
        let today = jiff::Zoned::now().date();
        // Картинки `cid:` в ленте не грузим: на их месте подписи; письмо целиком — в обычном виде.
        let images = Images::new();
        let mut col = div().flex().flex_col().gap_2();
        for item in &ordered(&f.conv.items, newest_first(cx)) {
            match *item {
                Item::Day(day) => {
                    let label = if day == today {
                        tr!("thread-today")
                    } else if today.yesterday().is_ok_and(|y| y == day) {
                        tr!("thread-yesterday")
                    } else {
                        day.strftime("%d.%m.%Y").to_string()
                    };
                    col = col.child(div().flex().justify_center().pt_1().text_color(l.text2()).child(label));
                }
                Item::Message(n) => {
                    let Some(m) = f.conv.messages.get(n) else { continue };
                    let who = if m.mine {
                        tr!("thread-me")
                    } else if m.row.from_name.is_empty() {
                        m.row.from_addr.clone()
                    } else {
                        m.row.from_name.clone()
                    };
                    let doc = f.docs.get(&m.row.id);
                    let whole = self.quotes || feed::is_forward(&m.row.subject);
                    let body: AnyElement = match doc {
                        None => div().text_color(l.text2()).child("…").into_any_element(),
                        Some(d) => {
                            let blocks = if whole { &d.blocks[..] } else { d.new_text() };
                            let hidden = !whole && blocks.len() < d.blocks.len();
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(ir_view::blocks(blocks, &format!("feed{n}"), &images, self.column.get(), cx))
                                .when(hidden, |d| d.child(div().text_color(l.text2()).child(tr!("thread-quotes-hidden"))))
                                .into_any_element()
                        }
                    };
                    let row = m.row.clone();
                    let card = div()
                        .id(("feed-msg", n))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .max_w(relative(0.85))
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(if current == Some(m.row.id.as_str()) { l.accent() } else { l.border() })
                        .bg(if m.mine { l.picked() } else { l.panel() })
                        .cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReadingEvent::Open(row.clone()))))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(who))
                                .child(div().text_color(l.text2()).child(crate::format_date(m.row.received_at))),
                        )
                        .child(body);
                    col = col.child(div().flex().w_full().when(m.mine, |d| d.justify_end()).child(card));
                }
            }
        }
        Some(col.into_any_element())
    }
}

// --- стопка (S5-05б) ---

/// Начало нового текста письма — строка свёрнутого письма стопки, если у письма нет превью.
fn snippet(doc: Option<&Document>) -> String {
    let Some(doc) = doc else { return String::new() };
    doc.new_text()
        .iter()
        .find_map(|b| match b {
            postman_ir::Block::Paragraph { content, .. } | postman_ir::Block::Heading { content, .. } => {
                let t = postman_ir::plain_text(content);
                let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
                (!t.is_empty()).then_some(t)
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Круг с инициалами (нейтральный цвет, как в шапке письма).
fn avatar(name: &str, size: f32, text: f32, l: &crate::roles::Look) -> Div {
    let c = l.cat(postman_core::palette::NEUTRAL);
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(c.bg)
        .text_color(c.text)
        .text_size(px(text))
        .child(crate::list::initials(name))
}

impl ReadingPane {
    /// Щелчок по свёрнутому письму стопки — раскрыть и открыть его (шапка, вложения, Ctrl+→ — от него); по шапке
    /// раскрытого — свернуть.
    fn stack_click(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(f) = &mut self.feed else { return };
        if f.open.remove(id) {
            return cx.notify();
        }
        f.open.insert(id.to_owned());
        if let Some(row) = f.conv.messages.iter().find(|m| m.row.id == id).map(|m| m.row.clone()) {
            cx.emit(ReadingEvent::Open(row));
        }
        cx.notify();
    }

    /// Enter — свернуть или раскрыть открытое письмо; «Развернуть» и «Свернуть» — все письма стопки.
    pub(crate) fn stack_fold(&mut self, how: Fold, cx: &mut Context<Self>) -> bool {
        if !self.stacked(cx) {
            return false;
        }
        let current = self.reading.as_ref().map(|r| r.row.id.clone());
        let Some(f) = &mut self.feed else { return false };
        match how {
            Fold::Expand => f.open = f.conv.messages.iter().map(|m| m.row.id.clone()).collect(),
            Fold::Collapse => f.open.clear(),
            Fold::Toggle => {
                let Some(id) = current else { return false };
                if !f.open.remove(&id) {
                    f.open.insert(id);
                }
            }
        }
        cx.notify();
        true
    }

    /// «Показать цитату» у письма стопки; без `id` — у открытого (команда `thread.quotes`).
    fn stack_quote(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let Some(id) = id.or_else(|| self.reading.as_ref().map(|r| r.row.id.clone())) else { return };
        let Some(f) = &mut self.feed else { return };
        if !f.quoted.remove(&id) {
            f.quoted.insert(id);
        }
        cx.notify();
    }

    /// `thread.quotes`: в стопке — цитата открытого письма, иначе — цитаты ленты.
    pub(crate) fn quotes_key(&mut self, cx: &mut Context<Self>) {
        if self.stacked(cx) { self.stack_quote(None, cx) } else { self.toggle_quotes(cx) }
    }

    /// Строка под темой: «4 письма · 3 участника» и «Развернуть все» / «Свернуть все».
    pub(crate) fn stack_info(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let f = self.feed.as_ref()?;
        let all = f.conv.messages.iter().all(|m| f.open.contains(&m.row.id));
        let (label, how) = if all { (tr!("thread-collapse-all"), Fold::Collapse) } else { (tr!("thread-expand-all"), Fold::Expand) };
        let info = tr!("thread-info", n = f.conv.len(), people = f.people());
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_color(l.text2())
                .child(info)
                .child(
                    div()
                        .id("stack-all")
                        .px(px(4.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .text_size(l.px(Metric::TypeCaption))
                        .text_color(l.accent_text())
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stack_fold(how, cx);
                        })),
                )
                .child(div().flex_1())
                .when(f.waiting, |d| d.child(tr!("thread-loading")))
                .child(self.order_button(cx))
                .into_any_element(),
        )
    }

    /// Карточки стопки — от старых писем к новым. У открытого письма — его вложения и приглашение.
    pub(crate) fn stack_view(&self, r: &Reading, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let f = self.feed.as_ref()?;
        let none = Images::new();
        let mut col = div().flex().flex_col();
        let mut order: Vec<usize> = (0..f.conv.messages.len()).collect();
        if newest_first(cx) {
            order.reverse();
        }
        for (n, m) in order.into_iter().filter_map(|n| f.conv.messages.get(n).map(|m| (n, m))) {
            let id = m.row.id.clone();
            let current = r.row.id == id;
            let who = if m.mine {
                tr!("thread-me")
            } else if m.row.from_name.is_empty() {
                m.row.from_addr.clone()
            } else {
                m.row.from_name.clone()
            };
            // Открытое письмо — разобранное целиком, с картинками `cid:`; остальные — из ленты.
            let (doc, images) = match (current, &r.doc) {
                (true, Some(d)) => (Some(d), &r.images),
                _ => (f.docs.get(&id), &none),
            };
            let card = if !f.open.contains(&id) {
                let text = if m.row.preview.is_empty() { snippet(doc) } else { m.row.preview.clone() };
                let click = id.clone();
                div()
                    .id(("stack-row", n))
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(avatar(&who, 24., 10., &l))
                    .child(div().w(px(128.)).flex_none().truncate().font_weight(FontWeight::SEMIBOLD).child(who))
                    .child(div().flex_1().min_w_0().truncate().text_color(l.text2()).child(text))
                    .when(m.row.has_attachment, |d| d.child(icon(glyph::ATTACH, 13.).text_color(l.text3())))
                    .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(crate::format_date(m.row.received_at)))
                    .on_click(cx.listener(move |this, _, _, cx| this.stack_click(&click, cx)))
                    .into_any_element()
            } else {
                self.stack_card(n, m, &who, doc, images, current.then_some(r), cx)
            };
            col = col.child(div().w_full().when(n > 0, |d| d.border_t_1().border_color(l.border())).child(card));
        }
        Some(col.into_any_element())
    }

    /// Раскрытое письмо стопки: шапка (щелчок — свернуть), «Ответить» и «Переслать», новый текст, цитата
    /// переключателем; у открытого (`open`) — вложения и приглашение.
    #[allow(clippy::too_many_arguments)]
    fn stack_card(
        &self,
        n: usize,
        m: &feed::Message,
        who: &str,
        doc: Option<&Document>,
        images: &Images,
        open: Option<&Reading>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let l = crate::roles::look(cx);
        let Some(f) = &self.feed else { return div().into_any_element() };
        let id = m.row.id.clone();
        let to = f.to.get(&id).cloned().unwrap_or_default();
        let quoted = f.quoted.contains(&id);
        let whole = feed::is_forward(&m.row.subject);
        let action = |key: &'static str, g: char, mode: Mode, row: EmailRow, cx: &mut Context<Self>| {
            div()
                .id((key, n))
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_color(l.text2())
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(icon(g, 16.))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!(key)).build(window, cx))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReadingEvent::Respond(row.clone(), mode))))
        };
        let click = id.clone();
        let head = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .pt(px(12.))
            .child(
                div()
                    .id(("stack-head", n))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(4.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(avatar(who, 32., 12., &l))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .items_baseline()
                                    .child(div().flex_none().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(who.to_owned()))
                                    .child(div().min_w_0().truncate().text_color(l.text2()).child(format!("<{}>", m.row.from_addr))),
                            )
                            .when(!to.is_empty(), |d| {
                                d.child(div().truncate().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("thread-to", to = to)))
                            }),
                    )
                    .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(crate::format_date(m.row.received_at)))
                    .on_click(cx.listener(move |this, _, _, cx| this.stack_click(&click, cx))),
            )
            .child(action("thread-reply", glyph::REPLY, Mode::Reply, m.row.clone(), cx))
            .child(action("thread-forward", glyph::FORWARD, Mode::Forward, m.row.clone(), cx));
        let body: AnyElement = match doc {
            None => div().text_color(l.text2()).child("…").into_any_element(),
            Some(d) => {
                let fresh = if whole { &d.blocks[..] } else { d.new_text() };
                let history = &d.blocks[fresh.len()..];
                let key = format!("stack{n}");
                let mut col = div().flex().flex_col().gap_1().children(ir_view::blocks(fresh, &key, images, self.column.get(), cx));
                if !history.is_empty() {
                    let label = if quoted { tr!("thread-quote-hide") } else { tr!("thread-quote-show") };
                    let qid = id.clone();
                    col = col.child(
                        div().flex().pt(px(8.)).child(
                            div()
                                .id(("stack-quote", n))
                                .h(px(20.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .px(px(8.))
                                .rounded(px(10.))
                                .bg(l.fill())
                                .text_color(l.text2())
                                .text_size(l.px(Metric::TypeCaption))
                                .cursor_pointer()
                                .hover(|d| d.bg(l.hover()))
                                .child(div().font_weight(FontWeight::SEMIBOLD).child("···"))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| this.stack_quote(Some(qid.clone()), cx))),
                        ),
                    );
                    if quoted {
                        col = col.child(
                            div()
                                .mt(px(8.))
                                .pl(px(10.))
                                .border_l_2()
                                .border_color(l.stroke())
                                .text_color(l.text2())
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(ir_view::blocks(history, &format!("{key}q"), images, self.column.get(), cx)),
                        );
                    }
                }
                col.into_any_element()
            }
        };
        let strip = open.and_then(|r| self.ribbon(r, cx));
        let invite = open.and_then(|r| r.invite.as_ref()).map(|c| crate::invite_card::render(c, self.me.as_deref(), cx));
        div()
            .flex()
            .flex_col()
            .child(head)
            .child(div().pl(px(48.)).pt(px(10.)).pb(px(16.)).flex().flex_col().gap(px(12.)).children(strip).children(invite).child(body))
            .into_any_element()
    }

    /// Вид «как Outlook»: над письмом — «Беседа: N писем · ← пред. · след. → · k из N».
    pub(crate) fn outlook_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = crate::roles::look(cx);
        let f = self.feed.as_ref()?;
        let id = &self.reading.as_ref()?.row.id;
        let at = f.conv.position(id)?;
        let n = f.conv.len();
        let step = |key: &'static str, label: String, delta: isize, on: bool, cx: &mut Context<Self>| {
            div()
                .id(key)
                .h(px(22.))
                .flex()
                .items_center()
                .px(px(6.))
                .rounded(px(4.))
                .when(on, |d| d.text_color(l.accent_text()).cursor_pointer().hover(|d| d.bg(l.hover())))
                .when(!on, |d| d.text_color(l.text3()))
                .child(label)
                .when(on, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(row) = this.thread_neighbor(delta, false) {
                            cx.emit(ReadingEvent::Open(row));
                        }
                    }))
                })
        };
        Some(
            div()
                .h(px(28.))
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(8.))
                .pr(px(4.))
                .rounded(px(4.))
                .bg(l.fill_passive())
                .text_size(l.px(Metric::TypeCaption))
                .text_color(l.text2())
                .child(div().text_color(l.text()).child(tr!("thread-outlook", n = n)))
                .child("·")
                .child(step("thread-prev", tr!("thread-prev"), -1, at > 0, cx))
                .child("·")
                .child(step("thread-next", tr!("thread-next"), 1, at + 1 < n, cx))
                .child(div().flex_1())
                .child(div().pr(px(4.)).child(tr!("thread-position", at = at + 1, n = n)))
                .into_any_element(),
        )
    }
}

impl Mailer {
    /// Ctrl+→ / Ctrl+←: следующее или предыдущее письмо беседы — в области чтения (список не трогаем).
    pub(crate) fn thread_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let newest = newest_first(cx);
        if let Some(row) = self.reading.read(cx).thread_neighbor(delta, newest) {
            self.open_letter(row, cx);
        }
    }

    /// Enter, «Развернуть», «Свернуть» (S5-05б): беседа в списке «как Outlook», иначе — письма стопки.
    pub(crate) fn thread_fold(&mut self, how: Fold, cx: &mut Context<Self>) {
        if self.section != crate::Section::Mail {
            return;
        }
        if !self.list.update(cx, |l, cx| l.fold(how, cx)) {
            self.reading.update(cx, |r, cx| r.stack_fold(how, cx));
        }
    }

    /// → / ← без открытого просмотра вложения: беседа в списке «как Outlook». `false` — клавиша не нужна.
    pub(crate) fn thread_arrow(&mut self, how: Fold, cx: &mut Context<Self>) -> bool {
        self.section == crate::Section::Mail && self.list.update(cx, |l, cx| l.fold(how, cx))
    }
}

/// Порядок в беседе — «сначала новые» (T1-16б, `Reading::thread_order`).
fn newest_first(cx: &App) -> bool {
    crate::prefs::get(cx).reading.thread_order == postman_core::prefs::ThreadOrder::NewestFirst
}

fn flip(o: postman_core::prefs::ThreadOrder) -> postman_core::prefs::ThreadOrder {
    use postman_core::prefs::ThreadOrder::*;
    if o == NewestFirst { OldestFirst } else { NewestFirst }
}

/// Шаг по беседе в порядке хранения (старые первыми) для шага на экране.
pub(crate) fn screen_step(delta: isize, newest: bool) -> isize {
    if newest { -delta } else { delta }
}

/// Лента в порядке на экране: «сначала новые» — дни от нового к старому, в дне — письма от нового к старому.
pub(crate) fn ordered(items: &[Item], newest: bool) -> Vec<Item> {
    if !newest {
        return items.to_vec();
    }
    let mut groups: Vec<Vec<Item>> = Vec::new();
    for it in items {
        match it {
            Item::Day(_) => groups.push(vec![*it]),
            _ => match groups.last_mut() {
                Some(g) => g.push(*it),
                None => groups.push(vec![*it]),
            },
        }
    }
    groups.reverse();
    groups
        .into_iter()
        .flat_map(|g| {
            let (day, msgs): (Vec<Item>, Vec<Item>) = g.into_iter().partition(|i| matches!(i, Item::Day(_)));
            day.into_iter().chain(msgs.into_iter().rev())
        })
        .collect()
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "thread", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Обработчики команд части — на корне экрана почты (`Mailer::render`).
fn handlers(root: Div, cx: &mut Context<crate::Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &thread::Next, _, cx| this.thread_step(1, cx)))
        .on_action(cx.listener(|this, _: &thread::Prev, _, cx| this.thread_step(-1, cx)))
        .on_action(cx.listener(|this, _: &thread::Toggle, _, cx| this.reading.update(cx, |r, cx| r.toggle_feed(cx))))
        .on_action(cx.listener(|this, _: &thread::Quotes, _, cx| this.reading.update(cx, |r, cx| r.quotes_key(cx))))
        .on_action(cx.listener(|this, _: &thread::Fold, _, cx| this.thread_fold(crate::outline::Fold::Toggle, cx)))
        .on_action(cx.listener(|this, _: &thread::Expand, _, cx| this.thread_fold(crate::outline::Fold::Expand, cx)))
        .on_action(cx.listener(|this, _: &thread::Collapse, _, cx| this.thread_fold(crate::outline::Fold::Collapse, cx)))
}

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/threads.ftl")), ("en", include_str!("../i18n/en/threads.ftl"))];

/// Беседа в области чтения (S5-05).
// Имя — область действий (`thread::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod thread {
    gpui_kit::actions!(thread, [Toggle, Quotes, Next, Prev, Fold, Expand, Collapse]);
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{LIST, MAIL, cmd};
    &[
        // Не Outlook (DESIGN, «Беседы»): лента беседы и цитаты — кнопками; по письмам беседы — Ctrl+→ / Ctrl+←
        // (в поле поиска эти клавиши — по словам).
        cmd("thread.toggle", &[], MAIL),
        cmd("thread.quotes", &[], MAIL),
        cmd("thread.next", &["ctrl-right"], LIST),
        cmd("thread.prev", &["ctrl-left"], LIST),
        // Вид беседы (S5-05б): Enter — свернуть или развернуть письмо стопки или беседу в списке «как Outlook»;
        // «Развернуть» и «Свернуть» — все письма стопки или беседу в списке. В списке «как Outlook» → и ← делают то же
        // (как стрелки в дереве Outlook): это клавиши `attach.next` / `attach.prev` — без открытого просмотра вложения
        // они идут беседе (`attachments::handlers`), второй привязки той же клавиши в почте нет (`keymap::conflicts`).
        cmd("thread.fold", &["enter"], LIST),
        cmd("thread.expand", &[], MAIL),
        cmd("thread.collapse", &[], MAIL),
    ]
};

/// Сцены `--screens` бесед (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::prefs::Conversation;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Лента беседы (S5-05): письмо цепочки демо с ответами из разных клиентов, только новый текст. После первого
        // запуска, последней из прежних: лента остаётся включённой, прежние сцены снимаются без неё.
        Scene::new("thread", thread, thread_ready).after(crate::signin::scenes::FIRSTRUN),
        // Виды беседы (S5-05б): свой вид «Входящих» — только в этих сценах.
        // Меню письма с открытым подменю «Вид беседы» (T1-14; было в меню папки).
        Scene::new("thread-view-menu", view_menu, view_menu_ready).continues(),
        // Стопка беседы в панели чтения (S5-05б): старые письма свёрнуты строками, последнее и открытое раскрыты.
        Scene::new("thread-stack", view, stack_ready).continues(),
        // «Входящие» в виде «как Outlook» (S5-05б): беседа цепочки демо развёрнута, выбрано письмо под головой.
        Scene::new("thread-list", view, list_ready).continues().wait(pick_child),
    ];

    /// Письмо под головой беседы демо во «Входящих» (сцена `thread-list`): старшее из двух писем цепочки там.
    const CHILD: &str = "chain1";

    fn thread(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.live.signin = None;
        m.set_section(Section::Mail, cx);
        // Без фильтров прошлых сцен.
        m.reset_filters(cx);
        m.filters.update(cx, |f, cx| f.set(cx, |f| f.quick_open = false));
        m.open_mailbox("inbox".into(), cx);
        let row = m.cache.rows_by_ids(&[postman_demo::CHAIN_OPEN.into()]).map_err(|e| e.to_string())?.pop();
        let Some(row) = row else { return Err("no demo conversation in the cache".into()) };
        m.reading.update(cx, |r, _| r.feed_on = true);
        m.open_letter(row, cx);
        Ok(())
    }

    fn thread_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let r = m.reading.read(cx);
        let open = r.reading.as_ref().is_some_and(|r| r.row.id == postman_demo::CHAIN_OPEN);
        open && r.feed_on && r.feed.as_ref().is_some_and(|f| f.conv.len() == postman_demo::CHAIN.len() && f.ready())
    }

    fn view_menu(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Как правая кнопка по первому письму и наведение на «Вид беседы».
        m.reading.update(cx, |r, _| r.feed_on = false);
        m.open_menu_at_row(0, None, cx);
        m.screen_menu_sub(crate::menu::Sub::View);
        Ok(())
    }

    fn view_menu_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.menu.is_some() && screens::rows_loaded(m.list.read(cx))
    }

    fn view(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.close_menu(cx);
        let stack = s.name == "thread-stack";
        let view = if stack { Conversation::Stack } else { Conversation::Outlook };
        // Как выбор «Вида беседы» в меню письма.
        crate::prefs::update(cx, |p| p.reading.set_folder("inbox", Some(view)));
        m.open_mailbox("inbox".into(), cx);
        if stack {
            let row = m.cache.rows_by_ids(&[postman_demo::CHAIN_OPEN.into()]).map_err(|e| e.to_string())?.pop();
            let Some(row) = row else { return Err("no demo conversation in the cache".into()) };
            m.open_letter(row, cx);
        } else {
            // Как щелчок по стрелке беседы; письмо под головой выбирается, когда беседы посчитаны (`pick_child`).
            m.list.update(cx, |l, _| l.threads_open.insert("chain".into()));
        }
        Ok(())
    }

    fn stack_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let r = m.reading.read(cx);
        let open = r.reading.as_ref().is_some_and(|r| r.row.id == postman_demo::CHAIN_OPEN && r.doc.is_some());
        open && r.stacked(cx) && r.feed.as_ref().is_some_and(|f| f.conv.len() == postman_demo::CHAIN.len() && f.ready())
    }

    fn list_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let open = screens::reading(m, cx).is_some_and(|r| r.row.id == CHILD && r.doc.is_some());
        let picked = list.selected.and_then(|i| list.row(i)).is_some_and(|r| r.id == CHILD);
        list.outline.is_some() && !list.computing && screens::rows_loaded(list) && picked && open
    }

    /// Беседы посчитаны — выбрать письмо под головой, как щелчок по нему.
    fn pick_child(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.list.update(cx, |l, cx| {
            let at = l.outline.as_ref().filter(|_| !l.computing).and_then(|o| o.position(CHILD));
            let picked = l.selected.and_then(|i| l.row(i)).is_some_and(|r| r.id == CHILD);
            if let (Some(i), false) = (at, picked) {
                l.select(i, cx);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{ordered, screen_step};
    use postman_core::thread::Item;

    /// Порядок беседы (T1-16б): «сначала новые» — дни и письма в дне от нового к старому; Ctrl+→ ведёт к более
    /// старому письму.
    #[test]
    fn newest_first_reverses_days_and_letters() {
        let d1 = jiff::civil::date(2026, 10, 27);
        let d2 = jiff::civil::date(2026, 10, 28);
        let items = [Item::Day(d1), Item::Message(0), Item::Message(1), Item::Day(d2), Item::Message(2)];
        assert_eq!(ordered(&items, false), items);
        assert_eq!(ordered(&items, true), [Item::Day(d2), Item::Message(2), Item::Day(d1), Item::Message(1), Item::Message(0)]);
        assert_eq!(screen_step(1, false), 1);
        assert_eq!(screen_step(1, true), -1, "вперёд на экране — к более старому");
    }
}
