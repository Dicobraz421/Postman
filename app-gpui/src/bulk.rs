//! Выделение и массовые действия (S5-08, DESIGN «Действия и состояния», макет «Выделение»): флажки на месте
//! аватаров, панель массовых действий вместо строки над списком, сводка выделения в области чтения, «Сообщить о
//! фишинге», полоса «Нет подключения» над списком.
//!
//! Что поменять у писем, решает `postman_core::bulk` (без UI): правки — сразу в кэш (в фоне) и серверу одним
//! `Command::Patch` на аккаунт (`Mailer::apply_patches`, как сроки S5-04), обратные правки — тостом «Отменить».
//! Строки выделения читаются из кэша в фоне: поток интерфейса не ждёт хранилище.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::{Metric, Role};
use pets_ui::{Level as Toast, ToastAction};
use postman_cache::{EmailRow, account_of};
use postman_core::bulk::{self, Act, Need, Plan, Summary};
use postman_core::marks::Kind;

use crate::Mailer;
use crate::chrome::{glyph, icon};
use crate::list::{MailList, PageSource};
use crate::roles::{Look, Pm};
use crate::select::span;

/// Писем в списке сводки — не больше (остальные — «и ещё N»).
const PICKED_SHOWN: usize = 6;

// --- события списка ---

/// Выделение изменилось (или пересчитана его сводка): экран — сводка вместо письма, панель, меню.
pub(crate) struct Picked;

impl EventEmitter<Picked> for MailList {}

/// Контекстное меню у строки `index`: правой кнопкой (`at` — где) или с клавиатуры (у строки).
pub(crate) struct OpenMenu {
    pub(crate) index: usize,
    pub(crate) at: Option<Point<Pixels>>,
}

impl EventEmitter<OpenMenu> for MailList {}

/// Кнопка панели массовых действий или сводки.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ask {
    /// Прочитано: `None` — по выделению (есть непрочитанные — прочитать, иначе — непрочитанными).
    Seen(Option<bool>),
    Flag(Option<bool>),
    /// Категория по ключевому слову: стоит у всех — снять, иначе поставить.
    Category(String),
    /// Снять категории таблицы.
    Uncategorize,
    Move(String),
    Archive,
    Trash,
    Phishing,
    /// Панель выбора срока (S5-04) для выделения.
    Lifecycle(Kind),
    /// Снять выделение.
    Clear,
}

impl EventEmitter<Ask> for MailList {}

/// «Повторить сейчас» в полосе «Нет подключения».
pub(crate) struct RetryNow;

impl EventEmitter<RetryNow> for MailList {}

/// Выпадающий список панели массовых действий.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drop {
    Category,
    Folder,
}

/// Сводка выделения: числа и отправители, первые письма (id, тема, отправитель, цвет полосы категории).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Selected {
    pub(crate) summary: Summary,
    pub(crate) first: Vec<(String, String, String, Option<String>)>,
}

/// Сводка по строкам выделения (в фоне): первые [`PICKED_SHOWN`] писем — id, тема, отправитель, цвет полосы.
fn selected_of(rows: &[EmailRow], book: &postman_core::labels::Book) -> Selected {
    let first = rows
        .iter()
        .take(PICKED_SHOWN)
        .map(|r| {
            let from = if r.from_name.is_empty() { r.from_addr.clone() } else { r.from_name.clone() };
            let stripe = crate::labels::row_labels(book, r).stripe.map(str::to_owned);
            (r.id.clone(), r.subject.clone(), from, stripe)
        })
        .collect();
    Selected { summary: bulk::summary(rows), first }
}

// --- выделение в списке ---

impl MailList {
    /// Ctrl+щелчок (`seed` — открытое письмо тоже в выделение, как в Outlook) или флажок: письмо — в выделение или
    /// из него; строка беседы (вид «как Outlook», S5-05б) — все письма беседы.
    pub(crate) fn pick(&mut self, i: usize, seed: bool, cx: &mut Context<Self>) {
        let Some(id) = self.row(i).map(|r| r.id.clone()) else { return };
        if seed
            && self.selection.is_empty()
            && let Some(c) = self.selected.filter(|&c| c != i)
            && self.row(c).is_some()
        {
            let open = self.members_of(c);
            self.selection.toggle_all(&open, c);
        }
        let members = self.members_of(i);
        if members.len() > 1 {
            self.selection.toggle_all(&members, i);
        } else {
            self.selection.toggle(&id, i);
        }
        self.picked_changed(cx);
    }

    /// Убрать письмо из выделения (крестик в сводке).
    pub(crate) fn unpick(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.selection.contains(id) {
            let index = self.selection.anchor().unwrap_or(0);
            self.selection.toggle(id, index);
            self.picked_changed(cx);
        }
    }

    /// Shift+щелчок: диапазон от якоря (последний Ctrl-щелчок или открытое письмо) до строки `i`.
    pub(crate) fn extend_to(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.count == 0 {
            return;
        }
        let i = i.min(self.count - 1);
        let anchor = self.selection.anchor().or(self.selected).unwrap_or(i);
        if self.selection.anchor().is_none()
            && let Some(id) = self.row(anchor).map(|r| r.id.clone())
        {
            self.selection.start(&id, anchor);
        }
        // Курсор — на конце диапазона (Shift+↑/↓ идут от него); письмо не открывается.
        self.selected = Some(i);
        self.scroll.scroll_to_item(self.layout.item_of(i).unwrap_or(i), ScrollStrategy::Center);
        let range = span(anchor, i);
        let loaded: Option<Vec<String>> = range.clone().map(|k| self.row(k).map(|r| r.id.clone())).collect();
        match loaded {
            Some(ids) => {
                let ids = self.expand_members(ids);
                self.selection.set_range(&ids);
                self.picked_changed(cx);
            }
            // Диапазон шире окна строк — id из кэша в фоне.
            None => {
                let (source, cache) = (self.page_source(), self.cache.clone());
                let (start, len) = (*range.start(), range.count());
                let task = cx.background_spawn(async move { ids_of(&source, &cache, start, len) });
                cx.spawn(async move |this, cx| {
                    let ids = task.await;
                    let _ = this.update(cx, |l, cx| {
                        let ids = l.expand_members(ids);
                        l.selection.set_range(&ids);
                        l.picked_changed(cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Shift+↑ / Shift+↓.
    pub(crate) fn extend(&mut self, delta: isize, cx: &mut Context<Self>) {
        let cur = self.selected.unwrap_or(0) as isize;
        let to = (cur + delta).max(0) as usize;
        self.extend_to(to, cx);
    }

    /// Ctrl+A: все письма списка (id — в фоне: в большом ящике их десятки тысяч).
    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        // Беседы (S5-05б): и письма свёрнутых бесед; их id уже в памяти.
        if let Some(o) = self.threads() {
            let ids: Vec<String> = o.all_ids().cloned().collect();
            self.selection.select_all(ids);
            return self.picked_changed(cx);
        }
        let (source, cache, count) = (self.page_source(), self.cache.clone(), self.count);
        if count == 0 {
            return;
        }
        let task = cx.background_spawn(async move { ids_of(&source, &cache, 0, count) });
        cx.spawn(async move |this, cx| {
            let ids = task.await;
            let _ = this.update(cx, |l, cx| {
                l.selection.select_all(ids);
                l.picked_changed(cx);
            });
        })
        .detach();
    }

    /// Снять выделение; `true` — было что снимать (Esc идёт дальше, если нет).
    pub(crate) fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.bulk_menu = None;
        if self.selection.is_empty() {
            return false;
        }
        self.selection.clear();
        self.picked_changed(cx);
        true
    }

    /// Выделение изменилось: сводка — в фоне, экран — событием.
    pub(crate) fn picked_changed(&mut self, cx: &mut Context<Self>) {
        self.summary_gen += 1;
        let generation = self.summary_gen;
        self.bulk_menu = None;
        if self.selection.is_empty() {
            self.summary = None;
        } else {
            let (ids, cache, book) = (self.selection.ids().to_vec(), self.cache.clone(), self.labels.clone());
            let task = cx.background_spawn(async move { selected_of(&cache.rows_by_ids(&ids).unwrap_or_default(), &book) });
            cx.spawn(async move |this, cx| {
                let s = task.await;
                let _ = this.update(cx, |l, cx| {
                    if l.summary_gen == generation {
                        l.summary = Some(Arc::new(s));
                        cx.emit(Picked);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.emit(Picked);
        cx.notify();
    }

    /// Список перечитан: выделенные письма, которых в нём больше нет, — из выделения (в фоне, по кэшу); сводка — по
    /// новым данным (прочитано, флаги).
    pub(crate) fn prune_selection(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }
        self.summary_gen += 1;
        let generation = self.summary_gen;
        let (ids, cache, book, source) = (self.selection.ids().to_vec(), self.cache.clone(), self.labels.clone(), self.alive_source());
        let task = cx.background_spawn(async move {
            let rows = cache.rows_by_ids(&ids).unwrap_or_default();
            let rows: Vec<EmailRow> = match &source {
                PageSource::Ids(list) => {
                    let set: HashSet<&str> = list.iter().map(String::as_str).collect();
                    rows.into_iter().filter(|r| set.contains(r.id.as_str())).collect()
                }
                PageSource::Mailbox(mb) => rows.into_iter().filter(|r| r.mailbox_ids.contains(mb)).collect(),
                PageSource::Empty => Vec::new(),
            };
            let alive: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
            (alive, selected_of(&rows, &book))
        });
        cx.spawn(async move |this, cx| {
            let (alive, s) = task.await;
            let _ = this.update(cx, |l, cx| {
                if l.summary_gen != generation {
                    return;
                }
                let (offset, rows) = (l.rows.0, &l.rows.1);
                let index_of = |id: &str| rows.iter().position(|r| r.id == id).map(|p| p + offset);
                l.selection.retain(&alive, index_of);
                l.summary = (!l.selection.is_empty()).then(|| Arc::new(s));
                cx.emit(Picked);
                cx.notify();
            });
        })
        .detach();
    }

    /// Где открыть контекстное меню строки `i` с клавиатуры: у строки, в окне.
    pub(crate) fn row_point(&self, i: usize) -> Option<Point<Pixels>> {
        let b = self.bounds.get();
        let item = self.layout.item_of(i)?;
        let top: f32 = self.sizes.iter().take(item).map(|s| f32::from(s.height)).sum();
        let y = f32::from(b.origin.y) + top + f32::from(self.scroll.offset().y) + self.row_h * 0.6;
        let y = y.clamp(f32::from(b.origin.y), f32::from(b.origin.y + b.size.height));
        Some(point(b.origin.x + px(140.), px(y)))
    }

    /// Категории своей таблицы: ключевое слово, имя, цвет, клавиша.
    pub(crate) fn category_items(&self) -> Vec<(String, String, String, Option<u8>)> {
        self.labels
            .for_account(None)
            .map(|t| t.visible().map(|c| (c.keyword.clone(), c.name.clone(), c.shown_color().to_owned(), c.key)).collect())
            .unwrap_or_default()
    }

    /// Папки для «В папку»: того же аккаунта, что открытая папка, кроме неё самой.
    pub(crate) fn folder_items(&self) -> Vec<(String, String)> {
        let account = self.mailbox.as_deref().and_then(account_of);
        self.order
            .iter()
            .filter(|id| account_of(id) == account && self.mailbox.as_deref() != Some(id.as_str()))
            .map(|id| (id.clone(), self.names.get(id).cloned().unwrap_or_default()))
            .collect()
    }

    /// Панель массовых действий (макет «Выделение»): «Выделить все», «Выбрано: N из M», «×»; ниже — кнопки.
    pub(crate) fn bulk_header(&self, l: &Look, cx: &mut Context<Self>) -> Div {
        let n = self.selection.len();
        let all = n >= self.count && self.count > 0;
        let s = self.summary.as_deref();
        let unread = s.is_none_or(|s| s.summary.unread > 0);
        let unflagged = s.is_none_or(|s| s.summary.flagged < s.summary.count);
        let all_box = div()
            .id("bulk-all")
            .w(px(52.))
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(
                div()
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .bg(l.accent())
                    .text_color(l.on_accent())
                    .child(if all { icon(glyph::CHECK, 12.) } else { div().w(px(8.)).h(px(2.)).bg(l.on_accent()) }),
            )
            .tooltip({
                let tip = crate::keytips::with_key(tr!("bulk-select-all"), crate::keytips::key("mail.select_all", cx));
                move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if all {
                    this.clear_selection(cx);
                } else {
                    this.select_all(cx);
                }
            }));
        let top = div()
            .h(px(36.))
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(4.))
            .pr(px(6.))
            .child(all_box)
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(tr!("bulk-selected", n = n)))
            .child(div().flex_none().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("bulk-of", total = self.count)))
            .child(div().flex_1())
            .child(
                div()
                    .id("bulk-clear")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(l.px(Metric::RadiusControl))
                    .text_color(l.text2())
                    .cursor_pointer()
                    .hover(|d| d.bg(l.hover()))
                    .child(icon(glyph::CLOSE, 11.))
                    .tooltip({
                        let tip = crate::keytips::with_key(tr!("bulk-clear-hint"), crate::keytips::key("search.clear", cx));
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_selection(cx);
                    })),
            );
        let open = self.bulk_menu;
        // Кнопка: значок и подпись; у «Категория», «Срок», «В папку» — стрелка выпадающего списка.
        let button = |id: &'static str, g: char, label: String, on: bool, drop: bool| {
            div()
                .id(id)
                .flex_1()
                .min_w_0()
                .h(px(44.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(3.))
                .rounded(l.px(Metric::RadiusControl))
                .cursor_pointer()
                .when(on, |d| d.bg(l.pressed()))
                .hover(|d| d.bg(l.hover()))
                .child(icon(g, 16.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .text_size(px(11.))
                        .whitespace_nowrap()
                        .child(label)
                        .when(drop, |d| d.child(icon(glyph::CHEVRON_DOWN, 7.).text_color(l.text2()))),
                )
        };
        let ask = |a: Ask| cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(a.clone()));
        let drop = |d: Drop| {
            cx.listener(move |this: &mut MailList, _: &ClickEvent, _, cx| {
                this.bulk_menu = if this.bulk_menu == Some(d) { None } else { Some(d) };
                cx.notify();
            })
        };
        let seen = if unread { tr!("bulk-read") } else { tr!("bulk-unread") };
        let flag = if unflagged { tr!("bulk-flag") } else { tr!("bulk-unflag") };
        let actions = div()
            .h(px(48.))
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .pb(px(4.))
            .child(button("bulk-seen", glyph::READ, seen, false, false).on_click(ask(Ask::Seen(None))))
            .child(button("bulk-flag", glyph::FLAG, flag, false, false).on_click(ask(Ask::Flag(None))))
            .child(
                button("bulk-cat", glyph::TAG, tr!("bulk-category-plain"), open == Some(Drop::Category), true)
                    .on_click(drop(Drop::Category)),
            )
            .child(button("bulk-due", glyph::CLOCK, tr!("bulk-due-plain"), false, true).on_click(ask(Ask::Lifecycle(Kind::Due))))
            .child(button("bulk-move", glyph::MOVE, tr!("bulk-move-plain"), open == Some(Drop::Folder), true).on_click(drop(Drop::Folder)))
            .child(button("bulk-archive", glyph::ARCHIVE, tr!("bulk-archive"), false, false).on_click(ask(Ask::Archive)))
            .child(button("bulk-delete", glyph::DELETE, tr!("bulk-delete"), false, false).on_click(ask(Ask::Trash)));
        div().flex_none().flex().flex_col().border_b_1().border_color(l.border()).child(top).child(actions)
    }

    /// Выпадающий список «Категория ▾» или «В папку ▾» под панелью.
    pub(crate) fn bulk_dropdown(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let which = self.bulk_menu.filter(|_| !self.selection.is_empty())?;
        let width = f32::from(self.bounds.get().size.width).max(300.);
        let (left, items): (f32, Vec<AnyElement>) = match which {
            Drop::Category => {
                let mut items: Vec<AnyElement> = self
                    .category_items()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (keyword, name, color, key))| {
                        menu_line(("bulk-cat-item", i), l)
                            .child(div().size(px(12.)).flex_none().rounded(px(3.)).bg(l.cat(&color).dot))
                            .child(div().flex_1().truncate().child(name))
                            .child(div().text_size(px(11.)).text_color(l.text3()).child(key.map(|k| k.to_string()).unwrap_or_default()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.bulk_menu = None;
                                cx.emit(Ask::Category(keyword.clone()));
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect();
                items.push(separator(l).into_any_element());
                items.push(
                    menu_line("bulk-cat-none", l)
                        .text_color(l.text2())
                        .child(div().size(px(12.)).flex_none().rounded(px(3.)).border_1().border_color(l.text3()))
                        .child(div().flex_1().child(tr!("bulk-uncategorize")))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.bulk_menu = None;
                            cx.emit(Ask::Uncategorize);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                (width * 2. / 7., items)
            }
            Drop::Folder => {
                let items = self
                    .folder_items()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (id, name))| {
                        menu_line(("bulk-folder", i), l)
                            .child(icon(glyph::FOLDER, 14.).text_color(l.text2()))
                            .child(div().flex_1().truncate().child(name))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.bulk_menu = None;
                                cx.emit(Ask::Move(id.clone()));
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect();
                (width * 4. / 7., items)
            }
        };
        let left = left.min(width - 228.).max(4.);
        Some(
            div()
                .id("bulk-dropdown")
                .occlude()
                .absolute()
                .top(px(84.))
                .left(px(left))
                .w(px(224.))
                .max_h(px(360.))
                .overflow_y_scroll()
                .p(px(4.))
                .flex()
                .flex_col()
                .rounded(l.px(Metric::RadiusOverlay))
                .border_1()
                .border_color(l.c(Role::StrokeCard))
                .bg(l.c(Role::LayerOverlay))
                .shadow_lg()
                .text_size(px(13.))
                .children(items)
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.bulk_menu = None;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// Полоса над списком без связи (макет «Состояния»): что видно и что будет с правками, «Повторить сейчас», «×».
    pub(crate) fn offline_banner(&self, l: &Look, cx: &mut Context<Self>) -> Option<Div> {
        let (host, queued) = self.offline.as_ref().filter(|_| !self.offline_hidden)?;
        let text = if *queued > 0 { tr!("states-banner-queued", host = host.clone(), n = *queued) } else { tr!("states-banner", host = host.clone()) };
        Some(
            div()
                .flex()
                .flex_none()
                .items_start()
                .gap(px(8.))
                .pl(px(14.))
                .pr(px(6.))
                .py(px(6.))
                .border_b_1()
                .border_color(l.border())
                .bg(l.fill_passive())
                .text_size(px(12.))
                .line_height(px(18.))
                .text_color(l.text2())
                .child(icon(glyph::OFFLINE, 14.).mt(px(2.)).text_color(l.text3()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_start()
                        .child(div().w_full().child(text))
                        .child(
                            div()
                                .id("offline-retry")
                                .text_color(l.accent_text())
                                .underline()
                                .whitespace_nowrap()
                                .cursor_pointer()
                                .child(tr!("states-retry"))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(RetryNow))),
                        ),
                )
                .child(
                    div()
                        .id("offline-hide")
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(l.px(Metric::RadiusControl))
                        .cursor_pointer()
                        .hover(|d| d.bg(l.hover()))
                        .child(icon(glyph::CLOSE, 9.))
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("states-banner-hide")).build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.offline_hidden = true;
                            cx.notify();
                        })),
                ),
        )
    }
}

/// id писем строк `start..start+len` источника списка (в фоне).
fn ids_of(source: &PageSource, cache: &postman_cache::Cache, start: usize, len: usize) -> Vec<String> {
    match source {
        PageSource::Ids(ids) => ids.iter().skip(start).take(len).cloned().collect(),
        _ => source.read(cache, start, len).into_iter().map(|r| r.id).collect(),
    }
}

/// Строка выпадающего списка и контекстного меню.
pub(crate) fn menu_line(id: impl Into<ElementId>, l: &Look) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(8.))
        .rounded(l.px(Metric::RadiusControl))
        .cursor_pointer()
        .hover(|d| d.bg(l.hover()))
}

pub(crate) fn separator(l: &Look) -> Div {
    div().h(px(1.)).my(px(4.)).mx(px(-4.)).bg(l.border())
}

/// Значок «Сообщить о фишинге» — красная маска (DESIGN, #CD605A — роль `app.phish`): квадами, без путей gpui.
pub(crate) fn mask(size: f32, l: &Look, behind: Hsla) -> Div {
    let w = size;
    let h = size * 0.62;
    let eye = |left: bool| {
        div()
            .absolute()
            .top(px(h * 0.3))
            .when(left, |d| d.left(px(w * 0.16)))
            .when(!left, |d| d.right(px(w * 0.16)))
            .w(px(w * 0.26))
            .h(px(h * 0.3))
            .rounded_full()
            .bg(behind)
    };
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .relative()
                .w(px(w))
                .h(px(h))
                .rounded_tl(px(h * 0.35))
                .rounded_tr(px(h * 0.35))
                .rounded_bl(px(h * 0.6))
                .rounded_br(px(h * 0.6))
                .bg(l.pm(Pm::Phish))
                .child(eye(true))
                .child(eye(false)),
        )
}

// --- действия экрана ---

/// Клавиша команды для подсказки («Ctrl+Q»; в «Одиночных клавишах» — буква): своя или из реестра, первая.
pub(crate) fn key_hint(id: &str, cx: &App) -> String {
    let set = crate::keymap::KeySet::of(&crate::prefs::get(cx));
    pets_commands::find(crate::commands::all(), id)
        .and_then(|c| crate::keymap::hint(&crate::commands::SINGLE, &set, c))
        .map(|k| crate::keymap::human(&k))
        .unwrap_or_default()
}

/// Писем — для текстов тостов.
fn plural_n(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl Mailer {
    /// Что выделено — над тем и действие; иначе открытое (или выбранное) письмо.
    pub(crate) fn targets(&self, cx: &App) -> Vec<String> {
        let list = self.list.read(cx);
        if !list.selection.is_empty() {
            return list.selection.ids().to_vec();
        }
        let open = self.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone());
        open.or_else(|| list.selected_row().map(|r| r.id)).into_iter().collect()
    }

    /// Что-то выделено в списке (команды писем действуют на выделение).
    pub(crate) fn picking(&self, cx: &App) -> bool {
        !self.list.read(cx).selection.is_empty()
    }

    /// Кнопка панели, сводки или меню над письмами `ids`.
    pub(crate) fn ask(&mut self, ids: Vec<String>, a: Ask, cx: &mut Context<Self>) {
        if ids.is_empty() && a != Ask::Clear {
            return self.toast(Toast::Info, tr!("actions-select-letter"), cx);
        }
        match a {
            Ask::Clear => {
                self.list.update(cx, |l, cx| {
                    l.clear_selection(cx);
                });
            }
            Ask::Lifecycle(kind) => self.open_picker_for(kind, ids, cx),
            Ask::Phishing => self.report_phishing(ids, cx),
            other => self.run_bulk(ids, other, cx),
        }
    }

    /// Правки писем `ids` по кнопке `a`: строки — из кэша в фоне, план — `postman_core::bulk`, затем кэш и сервер.
    pub(crate) fn run_bulk(&mut self, ids: Vec<String>, a: Ask, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let mailboxes = self.folders.read(cx).mailboxes.clone();
        let list = self.list.read(cx);
        let rights = list.rights.clone();
        let cats: Vec<String> = list.category_items().into_iter().map(|c| c.0).collect();
        let task = cx.background_spawn(async move {
            let rows = cache.rows_by_ids(&ids).unwrap_or_default();
            let act = match &a {
                Ask::Seen(on) => Act::Seen(on.unwrap_or_else(|| bulk::seen_target(&rows))),
                Ask::Flag(on) => Act::Flag(on.unwrap_or_else(|| bulk::flag_target(&rows))),
                Ask::Category(k) => {
                    let (add, remove) = postman_core::labels::toggle(rows.iter().map(|r| r.keywords.as_slice()), k);
                    Act::Keywords { add, remove }
                }
                Ask::Uncategorize => Act::Keywords { add: Vec::new(), remove: cats },
                Ask::Move(to) => Act::Move { to: to.clone() },
                Ask::Trash => Act::Trash,
                Ask::Phishing => Act::Phishing,
                Ask::Archive | Ask::Lifecycle(_) | Ask::Clear => Act::Archive,
            };
            let plan = bulk::plan(&rows, &act, &mailboxes, |r, need| {
                crate::list::may(&rights, r, |x| match need {
                    Need::Seen => x.seen,
                    Need::Keywords => x.keywords,
                    Need::Remove => x.remove,
                })
            });
            (act, plan, rows.len())
        });
        cx.spawn(async move |this, cx| {
            let (act, plan, n) = task.await;
            let _ = this.update(cx, |m, cx| m.bulk_planned(act, plan, n, cx));
        })
        .detach();
    }

    /// План готов: правки — в кэш и серверу, тост с «Отменить»; из «Удалённых» — строка «Удалить навсегда?».
    pub(crate) fn bulk_planned(&mut self, act: Act, plan: Plan, asked: usize, cx: &mut Context<Self>) {
        if plan.touched() == 0 {
            let text = if plan.skipped > 0 { tr!("bulk-read-only") } else { tr!("bulk-nothing") };
            let _ = asked;
            return self.toast(Toast::Info, text, cx);
        }
        let moved = matches!(act, Act::Move { .. } | Act::Archive | Act::Trash | Act::Phishing);
        // Открытое письмо ушло из папки — область чтения пуста.
        let open = self.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone());
        if moved && open.is_some_and(|id| plan.patches.iter().any(|p| p.id == id && !p.mailboxes.is_empty())) {
            self.reading.update(cx, |r, cx| r.clear(cx));
        }
        if !plan.purge.is_empty() {
            let purge = plan.purge.clone();
            self.list.update(cx, |l, cx| {
                l.confirm_many = Some(purge);
                cx.notify();
            });
        }
        if plan.patches.is_empty() {
            return;
        }
        let n = plan.patches.len();
        let moved_any = plan.patches.iter().any(|p| !p.mailboxes.is_empty());
        let text = self.done_text(&act, n, moved_any, cx);
        let undo = plan.undo.clone();
        self.apply_patches(plan.patches, cx);
        if plan.skipped > 0 {
            self.toast(Toast::Info, tr!("bulk-skipped", n = plural_n(plan.skipped)), cx);
        }
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("bulk-undo"), move |_, cx| {
            let undo = undo.clone();
            let _ = mailer.update(cx, |m, cx| {
                m.apply_patches(undo, cx);
                m.toast(Toast::Info, tr!("bulk-undone"), cx);
            });
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Success, text, action, cx));
    }

    /// Текст тоста «Сделано · N писем».
    fn done_text(&self, act: &Act, n: usize, moved: bool, cx: &App) -> String {
        let n64 = plural_n(n);
        match act {
            Act::Seen(true) => tr!("bulk-done-read", n = n64),
            Act::Seen(false) => tr!("bulk-done-unread", n = n64),
            Act::Flag(true) => tr!("bulk-done-flag", n = n64),
            Act::Flag(false) => tr!("bulk-done-unflag", n = n64),
            Act::Keywords { add, .. } => match add.first() {
                Some(k) => {
                    let name = self.list.read(cx).category_items().into_iter().find(|c| &c.0 == k).map(|c| c.1).unwrap_or_default();
                    tr!("bulk-done-category", n = n64, name = name)
                }
                None => tr!("bulk-done-uncategorize", n = n64),
            },
            Act::Move { to } => {
                let name = self.list.read(cx).names.get(to).cloned().unwrap_or_default();
                tr!("bulk-done-move", n = n64, folder = name)
            }
            Act::Archive => tr!("bulk-done-archive", n = n64),
            Act::Trash => tr!("bulk-done-trash", n = n64),
            // Папки «Нежелательные» нет — письма только помечены.
            Act::Phishing if moved => tr!("bulk-done-phishing", n = n64),
            Act::Phishing => tr!("bulk-done-phishing-marked", n = n64),
        }
    }

    /// «Да» в «Удалить навсегда N писем?»: кэш — в фоне, серверу — `Destroy` на каждое.
    pub(crate) fn purge_many(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let keys = ids.clone();
        let task = cx.background_spawn(async move { cache.delete_emails(&keys) });
        cx.spawn(async move |this, cx| {
            let stored = task.await;
            let _ = this.update(cx, |m, cx| {
                if let Err(e) = stored {
                    tracing::warn!("purge: {e}");
                    return m.toast(Toast::Error, tr!("actions-delete-failed", error = e.to_string()), cx);
                }
                if m.backend.is_live() {
                    for id in &ids {
                        if !m.backend.send(postman_core::sync::Command::Destroy(id.clone())) {
                            m.sync_stopped(cx);
                            break;
                        }
                    }
                }
                let open = m.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone());
                if open.is_some_and(|id| ids.contains(&id)) {
                    m.reading.update(cx, |r, cx| r.clear(cx));
                }
                m.list.update(cx, |l, cx| l.refresh(cx));
                m.toast(Toast::Success, tr!("bulk-done-purge", n = plural_n(ids.len())), cx);
            });
        })
        .detach();
    }

    /// «Сообщить о фишинге»: в «Нежелательные» с `$phishing` (быстрый фильтр «Угрозы»), тост «Отменить»; адрес в
    /// «Настройки → Общие» задан — ещё и письма вложениями на него (живой ящик; пусто — без пересылки).
    pub(crate) fn report_phishing(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let to = crate::prefs::get(cx).general.phishing_report.filter(|a| !a.trim().is_empty());
        if let Some(to) = to
            && self.backend.is_live()
        {
            let ticket = self.live.ticket();
            let n = plural_n(ids.len());
            let cmd = postman_core::sync::Command::ReportPhishing {
                ids: ids.clone(),
                to: postman_compose::Address::new("", to.trim()),
                subject: tr!("bulk-phishing-subject", n = n),
                text: tr!("bulk-phishing-text", n = n),
                ticket,
            };
            if !self.backend.send(cmd) {
                self.sync_stopped(cx);
            }
        }
        self.run_bulk(ids, Ask::Phishing, cx);
    }

    /// Ответ сервера на `ReportPhishing`.
    pub(crate) fn on_reported(&mut self, error: Option<&str>, cx: &mut Context<Self>) {
        let to = crate::prefs::get(cx).general.phishing_report.unwrap_or_default();
        match error {
            None => self.toast(Toast::Success, tr!("bulk-phishing-sent", to = to), cx),
            Some(e) => self.toast(Toast::Error, tr!("bulk-phishing-failed", error = e.to_owned()), cx),
        }
    }

    /// События списка о выделении: панель, меню, полоса связи.
    pub(crate) fn watch_bulk(list: &Entity<MailList>, cx: &mut Context<Self>) -> Vec<Subscription> {
        vec![
            cx.subscribe(list, |this, list, _: &Picked, cx| {
                let data = list.read(cx).summary.clone();
                let n = list.read(cx).selection.len();
                this.summary.update(cx, |s, cx| s.set(data, n, cx));
                cx.notify();
            }),
            cx.subscribe(list, |this, _, a: &Ask, cx| {
                let ids = this.targets(cx);
                this.ask(ids, a.clone(), cx);
            }),
            cx.subscribe(list, |this, _, m: &OpenMenu, cx| this.open_menu_at_row(m.index, m.at, cx)),
            cx.subscribe(list, |this, _, _: &RetryNow, cx| this.retry_now(cx)),
            cx.subscribe(list, |this, _, p: &crate::list::PurgeMany, cx| this.purge_many(p.0.clone(), cx)),
        ]
    }
}

impl Mailer {
    /// `mail.mark_read` / `mail.mark_unread`: выделение — пачкой с «Отменить», иначе выбранное письмо.
    pub(crate) fn mark(&mut self, seen: Option<bool>, cx: &mut Context<Self>) {
        if self.picking(cx) {
            let ids = self.targets(cx);
            return self.ask(ids, Ask::Seen(seen), cx);
        }
        self.list.update(cx, |l, cx| l.set_flags(seen, false, cx));
    }

    /// `mail.toggle_flag` (без клавиши: кнопка, меню, панель выделения; S5-16).
    pub(crate) fn toggle_flag(&mut self, cx: &mut Context<Self>) {
        if self.picking(cx) {
            let ids = self.targets(cx);
            return self.ask(ids, Ask::Flag(None), cx);
        }
        self.list.update(cx, |l, cx| l.set_flags(None, true, cx));
    }

    /// Клавиши 1–9: категория с этой клавишей — на выделении (у всех есть — снять) или на выбранном письме.
    pub(crate) fn categorize(&mut self, key: u8, cx: &mut Context<Self>) {
        if !self.picking(cx) {
            return self.list.update(cx, |l, cx| l.toggle_category(key, cx));
        }
        let keyword = self.list.read(cx).category_items().into_iter().find(|c| c.3 == Some(key)).map(|c| c.0);
        match keyword {
            Some(k) => {
                let ids = self.targets(cx);
                self.ask(ids, Ask::Category(k), cx);
            }
            None => self.toast(Toast::Info, tr!("labels-no-category", key = key), cx),
        }
    }

    /// Кнопка сводки выделения.
    pub(crate) fn on_summary(&mut self, a: &SummaryAsk, cx: &mut Context<Self>) {
        match a {
            SummaryAsk::Ask(a) => {
                let ids = self.targets(cx);
                self.ask(ids, a.clone(), cx);
            }
            SummaryAsk::Sub(sub, at) => {
                let list = self.list.read(cx);
                let ids = list.selection.ids().to_vec();
                let s = list.summary.as_ref().map(|s| s.summary.clone()).unwrap_or_default();
                let (unread, unflagged) = (s.unread > 0, s.flagged < s.count);
                self.open_menu(*at, ids, None, unread, unflagged, Some(*sub), cx);
            }
            SummaryAsk::Unpick(id) => self.list.update(cx, |l, cx| l.unpick(id, cx)),
        }
    }

    /// Панель выбора срока (S5-04) над письмами `ids` (строки — из кэша в фоне).
    pub(crate) fn open_picker_for(&mut self, kind: Kind, ids: Vec<String>, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move { cache.rows_by_ids(&ids).unwrap_or_default() });
        cx.spawn(async move |this, cx| {
            let rows = task.await;
            let _ = this.update(cx, |m, cx| m.picker_on(kind, rows, cx));
        })
        .detach();
    }
}

// --- сводка выделения в области чтения ---

/// Сводка выделения (макет «Выделение»): сколько писем, от кого, общий размер, первые письма, действия.
pub(crate) struct SummaryView {
    data: Option<Arc<Selected>>,
    count: usize,
}

/// Кнопка сводки — событие экрану.
pub(crate) enum SummaryAsk {
    Ask(Ask),
    /// «В папку» и «Категория» — контекстным меню у кнопки, подменю открыто.
    Sub(crate::menu::Sub, Point<Pixels>),
    Unpick(String),
}

impl EventEmitter<SummaryAsk> for SummaryView {}

impl SummaryView {
    pub(crate) fn new() -> Self {
        Self { data: None, count: 0 }
    }

    pub(crate) fn set(&mut self, data: Option<Arc<Selected>>, count: usize, cx: &mut Context<Self>) {
        if self.data != data || self.count != count {
            self.data = data;
            self.count = count;
            cx.notify();
        }
    }
}

/// Размер: «84 КБ», «1,2 МБ».
pub(crate) fn size_text(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.;
    if kb < 1024. {
        tr!("bulk-size-kb", n = format!("{}", kb.ceil() as u64))
    } else {
        tr!("bulk-size-mb", n = format!("{:.1}", kb / 1024.).replace('.', ","))
    }
}

/// Отправители: «Пётр (2), Анна и ещё 3».
fn senders_text(s: &Summary) -> String {
    let named: Vec<String> =
        s.senders.iter().take(2).map(|(w, n)| if *n > 1 { format!("{w} ({n})") } else { w.clone() }).collect();
    let rest = s.senders.len().saturating_sub(2);
    let head = named.join(", ");
    if rest > 0 { tr!("bulk-senders-more", who = head, n = plural_n(rest)) } else { head }
}

impl Render for SummaryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = crate::roles::look(cx);
        let n = self.count;
        let data = self.data.clone().unwrap_or_default();
        let s = &data.summary;
        let tile = div()
            .size(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.))
            .bg(l.picked())
            .text_color(l.accent_text())
            .child(icon(glyph::MULTI, 22.));
        let head = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(tile)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .line_height(px(22.))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child(tr!("bulk-summary-title", n = plural_n(n))))
                    .child(div().text_size(px(12.)).text_color(l.text2()).child(tr!("bulk-summary-hint"))),
            );
        let facts = (s.count > 0).then(|| {
            let mut parts = vec![tr!("bulk-summary-from", who = senders_text(s)), size_text(s.size)];
            if s.unread > 0 {
                parts.push(tr!("bulk-summary-unread", n = plural_n(s.unread)));
            }
            div().text_size(px(12.)).text_color(l.text2()).child(parts.join(" · "))
        });
        let mut picked = div().flex().flex_col().rounded(l.px(Metric::RadiusOverlay)).border_1().border_color(l.border()).overflow_hidden();
        for (i, (id, subject, from, stripe)) in data.first.iter().enumerate() {
            let id = id.clone();
            let stripe = stripe.as_deref().map_or(l.border(), |c| l.cat(c).dot);
            picked = picked.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(40.))
                    .pr(px(12.))
                    .border_b_1()
                    .border_color(l.border())
                    .child(div().w(px(3.)).h_full().bg(stripe))
                    .child(div().flex_1().min_w_0().truncate().child(subject.clone()))
                    .child(div().max_w(px(130.)).truncate().text_size(px(12.)).text_color(l.text2()).child(from.clone()))
                    .child(
                        div()
                            .id(("summary-unpick", i))
                            .size(px(24.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(l.px(Metric::RadiusControl))
                            .text_color(l.text2())
                            .cursor_pointer()
                            .hover(|d| d.bg(l.hover()))
                            .child(icon(glyph::CLOSE, 9.))
                            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("bulk-unpick")).build(window, cx))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SummaryAsk::Unpick(id.clone())))),
                    ),
            );
        }
        let more = n.saturating_sub(data.first.len());
        if more > 0 {
            picked = picked.child(div().h(px(32.)).px(px(12.)).flex().items_center().text_size(px(12.)).text_color(l.text2()).child(tr!("bulk-summary-more", n = plural_n(more))));
        }
        let unread = s.unread > 0 || s.count == 0;
        let flag_on = s.flagged < s.count || s.count == 0;
        // Клавиши — подсказкой «Название (клавиша)» и KeyTips, не в тексте (S5-18).
        let cell = |id: &'static str, g: Option<char>, label: String, command: &'static str| {
            let c = crate::buttons::secondary(id, &l)
                .h(px(36.))
                .gap(px(8.))
                .px(px(10.))
                .children(g.map(|g| icon(g, 16.)))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()));
            crate::keytips::command(c, command, Some(label), &l, cx)
        };
        let ask = |a: Ask| cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(SummaryAsk::Ask(a.clone())));
        let sub = |s: crate::menu::Sub| cx.listener(move |_, ev: &ClickEvent, _, cx| cx.emit(SummaryAsk::Sub(s, ev.position())));
        let seen = if unread { tr!("bulk-read") } else { tr!("bulk-unread") };
        let flag = if flag_on { tr!("bulk-flag") } else { tr!("bulk-unflag") };
        let phish = crate::buttons::secondary("summary-phishing", &l)
            .h(px(36.))
            .gap(px(8.))
            .px(px(10.))
            .child(mask(16., &l, l.pm(Pm::Secondary)))
            .child(div().flex_1().min_w_0().truncate().child(tr!("bulk-phishing")))
            .on_click(ask(Ask::Phishing));
        let grid = div()
            .grid()
            .grid_cols(2)
            .gap(px(8.))
            .child(cell("summary-archive", Some(glyph::ARCHIVE), tr!("bulk-archive"), "mail.archive").on_click(ask(Ask::Archive)))
            .child(cell("summary-delete", Some(glyph::DELETE), tr!("bulk-delete"), "mail.delete").on_click(ask(Ask::Trash)))
            .child(cell("summary-move", Some(glyph::MOVE), tr!("bulk-move-plain"), "mail.move").on_click(sub(crate::menu::Sub::Move)))
            .child(cell("summary-cat", Some(glyph::TAG), tr!("bulk-category-plain"), "mail.categorize").on_click(sub(crate::menu::Sub::Category)))
            .child(cell("summary-seen", Some(glyph::READ), seen, if unread { "mail.mark_read" } else { "mail.mark_unread" }).on_click(ask(Ask::Seen(None))))
            .child(cell("summary-due", Some(glyph::CLOCK), tr!("bulk-due-plain"), "mail.due").on_click(ask(Ask::Lifecycle(Kind::Due))))
            .child(cell("summary-flag", Some(glyph::FLAG), flag, "mail.toggle_flag").on_click(ask(Ask::Flag(None))))
            .child(phish);
        let foot = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .text_color(l.text2())
            .child(div().flex_1())
            .child(
                div()
                    .id("summary-clear")
                    .text_color(l.accent_text())
                    .cursor_pointer()
                    .child(tr!("bulk-summary-clear"))
                    .on_click(ask(Ask::Clear)),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.))
            .text_color(l.text())
            .text_size(px(13.))
            .child(div().w(px(440.)).flex().flex_col().gap(px(18.)).child(head).children(facts).child(picked).child(grid).child(foot))
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "bulk", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/bulk.ftl")),
    ("en", include_str!("../i18n/en/bulk.ftl")),
];

/// Сцены `--screens` выделения и контекстного меню (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Выделение (S5-08): три письма с флажками, панель массовых действий над списком, сводка в области чтения.
        Scene::new("bulk", bulk, bulk_ready),
        // Контекстное меню над выделением с открытым подменю «Категория».
        Scene::new("context-menu", menu, menu_ready).continues(),
        // «Сообщить о фишинге»: тост «Отменить» после сообщения и меню у другого письма на пункте с красной маской.
        Scene::new("phishing", phishing, menu_ready).continues(),
    ];

    fn bulk(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.reset_filters(cx);
        m.filters.update(cx, |f, cx| f.set(cx, |f| f.quick_open = false));
        m.close_picker(cx);
        m.reading.update(cx, |r, cx| r.clear(cx));
        let inbox = crate::live::inbox(&m.folders.read(cx).mailboxes).ok_or("no inbox")?;
        m.open_mailbox(inbox, cx);
        // Как Ctrl+щелчки по второй, третьей и пятой строкам.
        m.list.update(cx, |l, cx| {
            for i in [1, 2, 4] {
                l.pick(i, false, cx);
            }
        });
        Ok(())
    }

    fn bulk_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        let s = list.summary.as_ref().map(|s| s.summary.count);
        m.section == Section::Mail && list.selection.len() == 3 && s == Some(3) && screens::rows_loaded(list)
    }

    fn menu(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Как правая кнопка по третьей строке (она в выделении), затем наведение на «Категория».
        m.open_menu_at_row(2, None, cx);
        m.screen_menu_sub(crate::menu::Sub::Category);
        Ok(())
    }

    fn menu_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.menu.is_some() && screens::rows_loaded(m.list.read(cx))
    }

    fn phishing(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.close_menu(cx);
        m.list.update(cx, |l, cx| {
            l.clear_selection(cx);
        });
        // Как «Сообщить о фишинге» у первого письма (план — тот же, что у действия; «Отменить» — после снимков).
        let rows: Vec<postman_cache::EmailRow> = m.list.read(cx).rows.1.iter().take(2).cloned().collect();
        let Some(first) = rows.first().cloned() else { return Err("no letters in the inbox".into()) };
        let boxes = m.folders.read(cx).mailboxes.clone();
        let act = postman_core::bulk::Act::Phishing;
        let plan = postman_core::bulk::plan(std::slice::from_ref(&first), &act, &boxes, |_, _| true);
        if let Some(d) = &mut m.screens {
            d.undo.extend(plan.undo.clone());
        }
        m.bulk_planned(act, plan, 1, cx);
        // …и меню у второго письма на пункте «Сообщить о фишинге».
        m.open_menu_at_row(1, None, cx);
        m.screen_menu_last();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::size_text;

    #[test]
    fn sizes_read_like_the_mockup() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        assert_eq!(size_text(84 * 1024), "84 КБ");
        assert_eq!(size_text(1_300_000), "1,2 МБ");
    }
}
