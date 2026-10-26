//! «Отправить позже» и отмена отправки в интерфейсе (S5-06): тексты сроков, окно «Отменить» поверх экрана почты,
//! отмена запланированного письма из области чтения. Сроки и состояние окна — `postman_core::send_later`; держит
//! письма сервер (FUTURERELEASE), поэтому закрытый Postman ничего не теряет: письмо уходит в свой срок.

use std::collections::HashMap;
use std::time::Instant;

use gpui_kit::prelude::*;
use gpui_kit::*;
use jiff::Zoned;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_compose::{Address, Attachment, Draft};
use postman_core::send_later::{Choice, OfferState, Reject, Scheduled, Undo};
use postman_core::sync::{Command, Event};

use crate::Mailer;
use crate::compose::SentDraft;

/// Состояние экрана: окно «Отменить», отложенные отправки с сервера и черновики запланированных писем.
#[derive(Default)]
pub(crate) struct SendLater {
    undo: Undo<Draft>,
    /// Таймер скрытия окна «Отменить» запущен.
    armed: bool,
    /// Отправки, которые держит сервер (`Event::Scheduled`).
    pub(crate) scheduled: Vec<Scheduled>,
    /// Запланированные в этом запуске: письмо (ключ кэша) → что вернуть в окно при отмене.
    drafts: HashMap<String, Draft>,
    /// Отмены из области чтения: тикет → письмо.
    cancels: HashMap<u64, String>,
    ticket: u64,
}

impl SendLater {
    fn next_ticket(&mut self) -> u64 {
        self.ticket += 1;
        self.ticket
    }
}

fn local(ts: i64) -> Option<Zoned> {
    jiff::Timestamp::from_second(ts).ok().map(|t| t.to_zoned(jiff::tz::TimeZone::system()))
}

fn weekday(at: &Zoned) -> String {
    tr!(&format!("send-weekday-{}", at.weekday().to_monday_one_offset()))
}

/// Срок полностью: «ср 21.10 09:00».
pub(crate) fn when(ts: i64) -> String {
    let Some(at) = local(ts) else { return String::new() };
    tr!("send-later-when", weekday = weekday(&at), date = at.strftime("%d.%m").to_string(), time = at.strftime("%H:%M").to_string())
}

/// Срок коротко для строки письма: «ср 09:00».
pub(crate) fn short(ts: i64) -> String {
    local(ts).map(|at| format!("{} {}", weekday(&at), at.strftime("%H:%M"))).unwrap_or_default()
}

pub(crate) fn choice_label(c: Choice) -> String {
    match c {
        Choice::TomorrowMorning => tr!("send-later-tomorrow-morning"),
        Choice::TomorrowAfternoon => tr!("send-later-tomorrow-afternoon"),
        Choice::Monday => tr!("send-later-monday"),
    }
}

/// Подсказка справа в меню: «ср 09:00»; понедельник — с датой, он может быть и через неделю.
pub(crate) fn menu_hint(c: Choice, at: &Zoned) -> String {
    match c {
        Choice::Monday => {
            tr!("send-later-when", weekday = weekday(at), date = at.strftime("%d.%m").to_string(), time = at.strftime("%H:%M").to_string())
        }
        _ => format!("{} {}", weekday(at), at.strftime("%H:%M")),
    }
}

pub(crate) fn reject_text(r: Reject, max_delay: u64) -> String {
    match r {
        Reject::Format => tr!("send-later-format"),
        Reject::Past => tr!("send-later-past"),
        Reject::TooFar => tr!("send-later-too-far", days = (max_delay / 86_400).to_string()),
        Reject::Unsupported => tr!("send-later-unsupported"),
    }
}

/// Черновик из письма в кэше (запланировано в прошлом запуске): адреса, тема, текст, вложения ссылками. Скрытых
/// получателей в письме нет — их в окне не будет.
fn draft_from_cache(m: &Mailer, key: &str) -> Option<Draft> {
    let row = m.cache.rows_by_ids(&[key.to_owned()]).ok()?.pop()?;
    let body = m.cache.body(key).ok()??;
    let account = postman_cache::account_of(key);
    let from = m
        .compose
        .sender_for(account)
        .or_else(|| m.compose.me().cloned())
        .map(|(a, _)| a)
        .unwrap_or_else(|| Address::new(&row.from_name, &row.from_addr));
    let addresses = |list: &[postman_cache::Addr]| list.iter().map(|a| Address::new(&a.name, &a.email)).collect();
    Some(Draft {
        from,
        to: addresses(&body.to),
        cc: addresses(&body.cc),
        subject: row.subject,
        markdown: body.text.unwrap_or_default(),
        in_reply_to: body.references.last().cloned(),
        references: body.references,
        attachments: body
            .attachments
            .into_iter()
            .map(|a| Attachment { inline_cid: a.cid.filter(|_| a.inline), name: a.name, mime: a.mime, data: Vec::new(), blob_id: Some(a.blob_id) })
            .collect(),
        ..Default::default()
    })
}

impl Mailer {
    /// События отправки: окно «Отменить», итог отмены, список отложенных. `sent` — что отправило окно письма.
    pub(crate) fn on_send_sync(&mut self, ev: &Event, sent: Option<SentDraft>, cx: &mut Context<Self>) {
        match ev {
            Event::Sent { scheduled: Some(s), .. } => {
                let Some(sent) = sent else { return };
                if sent.later {
                    self.send_later.drafts.insert(s.email.clone(), sent.draft);
                    let text = tr!("send-scheduled-toast", when = when(s.send_at));
                    self.toasts.update(cx, |t, cx| t.push(Toast::Success, text, cx));
                } else {
                    self.send_later.undo.offer(s.clone(), sent.draft, Instant::now());
                    self.arm_undo(cx);
                    cx.notify();
                }
            }
            Event::Unsent { ticket, email } => {
                let offered = self.send_later.undo.unsent(*ticket);
                self.send_later.cancels.remove(ticket);
                let kept = self.send_later.drafts.remove(email);
                let draft = offered.or(kept).or_else(|| draft_from_cache(self, email));
                let text = match draft {
                    Some(d) => {
                        let how = crate::compose::OpenAs { letter: None, server_id: Some(email.clone()) };
                        self.open_draft(d, how, cx);
                        tr!("send-unsent")
                    }
                    None => tr!("send-unsent-drafts"),
                };
                self.toasts.update(cx, |t, cx| t.push(Toast::Info, text, cx));
                cx.notify();
            }
            Event::UnsendFailed { ticket, too_late, error } => {
                self.send_later.undo.failed(*ticket);
                self.send_later.cancels.remove(ticket);
                let (level, text) =
                    if *too_late { (Toast::Warning, tr!("send-too-late")) } else { (Toast::Error, tr!("send-unsend-failed", error = error.clone())) };
                self.toasts.update(cx, |t, cx| t.push(level, text, cx));
                cx.notify();
            }
            Event::Scheduled { list, .. } => {
                self.send_later.scheduled = list.clone();
                let rows: HashMap<String, i64> = list.iter().map(|s| (s.email.clone(), s.send_at)).collect();
                self.list.update(cx, |l, cx| l.set_scheduled(rows, cx));
                let list = list.clone();
                self.reading.update(cx, |r, cx| {
                    r.scheduled = list;
                    cx.notify();
                });
            }
            _ => {}
        }
    }

    /// Таймер скрытия окна «Отменить»: только пока оно на экране (в простое кадров нет).
    fn arm_undo(&mut self, cx: &mut Context<Self>) {
        if self.send_later.armed {
            return;
        }
        let Some(due) = self.send_later.undo.next_deadline() else { return };
        self.send_later.armed = true;
        let wait = due.saturating_duration_since(Instant::now());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |m, cx| {
                m.send_later.armed = false;
                if m.send_later.undo.expire(Instant::now()) {
                    cx.notify();
                }
                m.arm_undo(cx);
            });
        })
        .detach();
    }

    /// «Отменить» в окне после отправки.
    fn undo_send(&mut self, id: u64, cx: &mut Context<Self>) {
        let ticket = self.send_later.next_ticket();
        let Some(scheduled) = self.send_later.undo.cancel(id, ticket, Instant::now()) else { return };
        if !self.backend.send(Command::Unsend { scheduled, ticket }) {
            self.send_later.undo.failed(ticket);
            self.needs_server(cx);
        }
        cx.notify();
    }

    /// «Отменить отправку» запланированного письма (область чтения).
    pub(crate) fn unsend_scheduled(&mut self, scheduled: Scheduled, cx: &mut Context<Self>) {
        let ticket = self.send_later.next_ticket();
        self.send_later.cancels.insert(ticket, scheduled.email.clone());
        if !self.backend.send(Command::Unsend { scheduled, ticket }) {
            self.send_later.cancels.remove(&ticket);
            self.needs_server(cx);
        }
    }

    /// Окно «Отменить» отправки на экране (над ним — карточки напоминаний, S5-04).
    pub(crate) fn undo_shown(&self) -> bool {
        self.send_later.undo.shown(Instant::now()).is_some()
    }

    /// Окно «Письмо отправлено · Отменить» внизу экрана почты (тосты `pets-ui` без кнопок действия).
    pub(crate) fn undo_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let offer = self.send_later.undo.shown(Instant::now())?;
        let id = offer.id;
        let l = crate::roles::look(cx);
        let action = match offer.state {
            OfferState::Open => div()
                .id("undo-send")
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(l.accent_text())
                .hover(|d| d.bg(l.hover()))
                .child(tr!("send-undo"))
                .on_click(cx.listener(move |this, _, _, cx| this.undo_send(id, cx)))
                .into_any_element(),
            OfferState::Canceling(_) => div().text_color(l.text3()).child(tr!("send-undoing")).into_any_element(),
        };
        let card = div()
            .id("undo-card")
            .occlude()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .rounded_md()
            .shadow_lg()
            .border_1()
            .border_color(l.c(pets_ui::theme::Role::StrokeCard))
            .bg(l.c(pets_ui::theme::Role::LayerOverlay))
            .text_color(l.text())
            .child(tr!("send-sent"))
            .child(div().text_color(l.text3()).child("·"))
            .child(action);
        Some(div().absolute().left_0().right_0().bottom(px(36.)).flex().justify_center().child(card).into_any_element())
    }
}
