//! Область чтения: шапка письма, вложения, карточка приглашения и тело, разобранное воркером-песочницей.
//!
//! Чужие данные (HTML, картинки, `.ics`) разбирает только воркер. Байты вложений — из демо-ящика, из хранилища
//! или с сервера (`Command::Blob` → `Event::BlobReady` в [`ReadingPane::on_sync`]); диск, воркер и сервер — в фоне.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_cache::{AttachmentRow, BodyRow, Cache, EmailRow};
use postman_calendar::{Invitation, Partstat};
use postman_core::files;
use postman_core::invite::Delivery;
use postman_core::sync::{Command, Event};
use postman_ir::Document;
use postman_worker::{Client as Worker, Level, OfficeScan, Request, Rgba};

use crate::attachments::{self, FileAct};
use crate::invite_card::{self, Card, State};
use crate::ir_view::{self, Images};
use crate::roles::look;
use crate::*;

/// Наибольшая сторона картинки в области чтения.
const MAX_IMAGE_SIDE: u32 = 1600;
/// Ответ воркера дольше — сбой (воркер перезапускается).
const WORKER_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Reading {
    pub(crate) row: EmailRow,
    pub(crate) body: Option<BodyRow>,
    pub(crate) doc: Option<Document>,
    /// Картинки `cid:`, декодированные воркером.
    pub(crate) images: Images,
    /// `cid:`, за которыми уже пошли (байты, декодирование).
    pub(crate) requested: HashSet<String>,
    pub(crate) invite: Option<Card>,
    /// Письмо-поручение (S6-10): вложение `text/calendar` с VTODO вместо приглашения.
    pub(crate) task_mail: Option<Box<postman_calendar::assign::Incoming>>,
    /// Проверки документов Office в воркере (S5-07): `blob_id` → итог; `scanning` — ждут байтов или воркера.
    pub(crate) scans: HashMap<String, OfficeScan>,
    pub(crate) scanning: HashSet<String>,
    /// Встроенный просмотр вложения (на месте текста письма).
    pub(crate) preview: Option<crate::preview::Pv>,
    /// Лента вложений развёрнута («+N»).
    pub(crate) expanded: bool,
    /// Подпись S/MIME (S5-12): итог и карточка; `None` — письмо не подписано (или тело ещё не пришло).
    pub(crate) smime: Option<crate::smime::State>,
}

impl Reading {
    fn new(row: EmailRow, body: Option<BodyRow>) -> Self {
        Self {
            row,
            body,
            doc: None,
            images: Images::new(),
            requested: HashSet::new(),
            invite: None,
            task_mail: None,
            scans: HashMap::new(),
            scanning: HashSet::new(),
            preview: None,
            expanded: false,
            smime: None,
        }
    }
}

// --- область чтения ---

pub(crate) struct ReadingPane {
    pub(crate) worker: Arc<Mutex<Worker>>,
    pub(crate) cache: Cache,
    pub(crate) reading: Option<Reading>,
    /// Ящик на сервере (ставит S3-04 при входе): байты вложений — из хранилища или командой `Blob`,
    /// ответ на приглашение — командой `RespondInvite`. Иначе — демо-ящик.
    pub(crate) live: bool,
    /// Мой адрес — кто я среди участников приглашения (`Event::Me`; в демо — адрес демо-ящика).
    pub(crate) me: Option<String>,
    /// Действия с вложениями, ждущие байтов.
    pub(crate) pending_files: Vec<(AttachmentRow, FileAct)>,
    /// Блобы, за которыми пошли (хранилище или сервер) и которых ещё нет.
    pub(crate) in_flight: HashSet<String>,
    /// Куда «Открыть» кладёт файл перед запуском системного приложения.
    pub(crate) open_dir: PathBuf,
    /// Картинки закрытого письма: убрать из атласа gpui при следующей отрисовке (S4-08). Сам gpui их не
    /// убирает, пока жив процесс: без этого каждое открытое письмо с картинкой оставляло в атласе (память GPU,
    /// на встроенной графике — память процесса) свой кадр.
    pub(crate) retired: Vec<Arc<RenderImage>>,
    /// Беседа открытого письма (S5-05, `thread.rs`); `feed_on` — показана лента, `quotes` — с цитатами.
    pub(crate) feed: Option<crate::thread::Feed>,
    pub(crate) feed_on: bool,
    pub(crate) quotes: bool,
    /// Мои адреса отправителя (`Event::Senders`): свои письма в ленте беседы.
    pub(crate) mine: Vec<String>,
    /// Отправки, которые держит сервер (S5-06): у такого письма — строка «Отправится …» с отменой.
    pub(crate) scheduled: Vec<postman_core::send_later::Scheduled>,
    /// «Сохранить все» ждёт байтов вложений (S5-07).
    pub(crate) save_all: Option<crate::preview::SaveAll>,
    /// Открытая папка: её вид беседы (лента, стопка, «как Outlook», S5-05б).
    pub(crate) folder: Option<String>,
    /// Раздел «Задачи»: своя копия поручения для карточки письма-поручения (S6-10).
    pub(crate) tasks: Option<WeakEntity<crate::tasks::TasksView>>,
    /// Идёт поиск (S5-10): строка запроса и слова — подсветка в теме и тексте, строка «Совпадения».
    pub(crate) marks: Option<(String, Arc<Vec<String>>)>,
    /// Текущее совпадение (S5-14, F3 / Shift+F3): номер по порядку чтения — тема, затем текст.
    pub(crate) hit: usize,
    /// Совпадений в открытом письме — по последней отрисовке (тема и текст).
    pub(crate) hits: std::cell::Cell<usize>,
    /// Перешли к другому совпадению: при отрисовке прокрутить его в вид.
    reveal: bool,
    /// Прокрутка письма (обычный вид): к текущему совпадению.
    pub(crate) scroll: ScrollHandle,
}

/// Итог разбора и действий: уровень песочницы (строка состояния), тосты, запросы к серверу.
pub(crate) enum ReadingEvent {
    Sandbox(Level),
    Error(String),
    Toast(Toast, String),
    /// Действию нужен сервер, а его нет (демо).
    NeedsServer,
    /// Команда синхронизации: `Mailer` отдаёт её в `Backend`; не ушла — [`ReadingPane::not_sent`].
    Sync(SyncRequest),
    /// Открыть письмо (щелчок по письму в ленте беседы).
    Open(EmailRow),
    /// «Отменить отправку» запланированного письма.
    Unsend(postman_core::send_later::Scheduled),
    /// «Ответить» или «Переслать» у письма стопки (S5-05б).
    Respond(EmailRow, postman_compose::Mode),
    /// Письмо-поручение разобрано (S6-10): учесть его в своей копии задачи (шаг, комментарий, срок).
    TaskMail(Box<postman_calendar::assign::Incoming>),
    /// Кнопка карточки поручения в письме: действие над задачей `uid`; `None` — открыть её в «Задачах».
    TaskAction { uid: String, action: Option<postman_calendar::assign::Action> },
    /// Ответ на приглашение ушёл, «Удалять письмо-приглашение после ответа» включено (S5-20): письмо — в «Удалённые».
    TrashAnswered(EmailRow),
}

/// Запрос области чтения к серверу: клонируемая форма `Command` (событие gpui отдаётся по ссылке).
#[derive(Clone)]
pub(crate) enum SyncRequest {
    Blob(AttachmentRow),
    RespondInvite { invite: Box<Invitation>, status: Partstat, in_reply_to: Option<String> },
    /// Беседа с сервера (ключ цепочки).
    Thread(String),
    /// Мои события в сутки встречи из приглашения (S5-11): пересечения и «Ваш день».
    Events { from: i64, to: i64 },
}

impl SyncRequest {
    pub(crate) fn command(&self) -> Command {
        match self {
            SyncRequest::Blob(a) => Command::Blob { blob_id: a.blob_id.clone(), name: a.name.clone(), mime: a.mime.clone() },
            SyncRequest::RespondInvite { invite, status, in_reply_to } => {
                Command::RespondInvite { invite: invite.clone(), status: *status, in_reply_to: in_reply_to.clone() }
            }
            SyncRequest::Thread(key) => Command::Thread(key.clone()),
            SyncRequest::Events { from, to } => Command::Events { from: *from, to: *to },
        }
    }
}

impl EventEmitter<ReadingEvent> for ReadingPane {}

impl ReadingPane {
    pub(crate) fn new(cache: Cache) -> Self {
        Self {
            worker: Arc::new(Mutex::new(Worker::new(Worker::default_exe(), WORKER_TIMEOUT))),
            cache,
            reading: None,
            live: false,
            me: Some(postman_demo::ME.to_string()),
            pending_files: Vec::new(),
            in_flight: HashSet::new(),
            open_dir: data_dir().join("open"),
            retired: Vec::new(),
            feed: None,
            feed_on: false,
            quotes: false,
            mine: Vec::new(),
            scheduled: Vec::new(),
            save_all: None,
            folder: None,
            tasks: None,
            marks: None,
            hit: 0,
            hits: std::cell::Cell::new(0),
            reveal: false,
            scroll: ScrollHandle::new(),
        }
    }

    /// Поиск начался, сменился или закончился (`None`): подсветка — по нему.
    pub(crate) fn set_marks(&mut self, marks: Option<(String, Arc<Vec<String>>)>, cx: &mut Context<Self>) {
        if self.marks != marks {
            self.marks = marks;
            self.hit = 0;
            cx.notify();
        }
    }

    /// F3 / Shift+F3 (S5-14): следующее или предыдущее совпадение по кругу, прокрутить к нему.
    pub(crate) fn step_hit(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.hits.get();
        if self.marks.is_none() || self.reading.is_none() || n == 0 {
            return;
        }
        self.hit = step(self.hit, delta, n);
        self.reveal = true;
        cx.notify();
    }

    /// Строка «Совпадение k из N · ↑ ↓ · «запрос»» над текстом письма во время поиска; `n` — совпадений в теме и
    /// тексте.
    fn match_bar(&self, n: usize, l: &crate::roles::Look) -> Option<Div> {
        let (query, _) = self.marks.as_ref()?;
        let count = if n == 0 { tr!("search-matches-none") } else { tr!("search-matches", k = self.hit.min(n - 1) + 1, n = n) };
        let arrow = |id: &'static str, g: char, name: &'static str, action: fn() -> Box<dyn Action>| {
            div()
                .id(id)
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(l.px(pets_ui::theme::Metric::RadiusControl))
                .text_color(l.text2())
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(crate::chrome::icon(g, 12.))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!(name)).build(window, cx))
                .on_click(move |_, window, cx| window.dispatch_action(action(), cx))
        };
        Some(
            div()
                .h(px(34.))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .rounded(l.px(pets_ui::theme::Metric::RadiusControl))
                .bg(l.fill_passive())
                .text_size(l.px(pets_ui::theme::Metric::TypeCaption))
                .text_color(l.text2())
                .child(crate::chrome::icon(crate::chrome::glyph::SEARCH, 14.))
                .child(div().text_color(l.text()).child(count))
                .when(n > 1, |d| {
                    d.child(arrow("hit-prev", crate::chrome::glyph::UP, "cmd-search-prev-match", || {
                        Box::new(crate::search::search::PrevMatch)
                    }))
                    .child(arrow("hit-next", crate::chrome::glyph::DOWN, "cmd-search-next-match", || {
                        Box::new(crate::search::search::NextMatch)
                    }))
                })
                .child(div().flex_1())
                .child(div().min_w_0().truncate().child(tr!("search-matches-query", query = query.clone()))),
        )
    }

    /// Открытое письмо ждёт отправки на сервере: «Отправится ср 21.10 09:00 · Отменить отправку».
    fn scheduled_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = &self.reading.as_ref()?.row.id;
        let now = jiff::Timestamp::now().as_second();
        let s = self.scheduled.iter().find(|s| &s.email == id && s.send_at > now)?.clone();
        let text = tr!("send-scheduled-bar", when = crate::send_later::when(s.send_at));
        let l = look(cx);
        let bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(l.picked())
            .child(div().flex_1().child(text))
            .child(
                crate::buttons::secondary("unsend", &l)
                    .child(tr!("send-scheduled-cancel"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ReadingEvent::Unsend(s.clone())))),
            );
        Some(bar.into_any_element())
    }

    /// Показать другое письмо (или ничего): картинки прежнего — в [`ReadingPane::retired`].
    fn replace(&mut self, next: Option<Reading>) {
        self.save_all = None;
        if next.as_ref().map(|n| &n.row.id) != self.reading.as_ref().map(|r| &r.row.id) {
            self.hit = 0;
        }
        if let Some(old) = std::mem::replace(&mut self.reading, next) {
            self.retired.extend(old.preview.as_ref().and_then(|p| p.image()));
            self.retired.extend(old.images.into_values());
        }
    }

    pub(crate) fn open(&mut self, row: EmailRow, cx: &mut Context<Self>) {
        if self.reading.as_ref().is_some_and(|r| r.row.id == row.id) {
            return;
        }
        let body = Some(postman_demo::body(&row.id));
        self.replace(Some(Reading::new(row, body)));
        self.render_body(cx);
        self.ensure_invite(cx);
        self.ensure_scans(cx);
        self.ensure_smime(cx);
        self.sync_thread(cx);
        cx.notify();
    }

    /// Живой ящик (S3-04): тело — из кэша или с сервера, приходит позже (`show_body`).
    pub(crate) fn open_live(&mut self, row: EmailRow, cx: &mut Context<Self>) {
        if self.reading.as_ref().is_some_and(|r| r.row.id == row.id) {
            return;
        }
        self.replace(Some(Reading::new(row, None)));
        self.sync_thread(cx);
        cx.notify();
    }

    /// Тело открытого письма пришло (кэш, `BodyReady`): разобрать в воркере.
    pub(crate) fn show_body(&mut self, id: &str, body: postman_cache::BodyRow, cx: &mut Context<Self>) {
        let Some(r) = self.reading.as_mut().filter(|r| r.row.id == id) else { return };
        r.body = Some(body);
        r.doc = None;
        self.render_body(cx);
        self.ensure_invite(cx);
        self.ensure_scans(cx);
        self.ensure_smime(cx);
        cx.notify();
    }

    /// Событие синхронизации: блоб вложения, ответ на приглашение, мой адрес.
    pub(crate) fn on_sync(&mut self, ev: &Event, cx: &mut Context<Self>) {
        match ev {
            Event::BlobReady { blob_id, data } => self.blob_ready(blob_id, data.clone(), cx),
            Event::BlobFailed { blob_id, error } => self.blob_failed(blob_id, error.clone(), cx),
            Event::InviteAnswered { uid, status, delivery } => {
                let letter = self
                    .reading
                    .as_ref()
                    .filter(|r| r.invite.as_ref().is_some_and(|c| c.uid() == Some(uid.as_str())))
                    .map(|r| r.row.clone());
                // Ответ запоминается с версией встречи из карточки (S5-11), без карточки — только статус.
                let value = self.card_for(uid).map(|c| c.remember(*status)).unwrap_or_else(|| invite_card::encode(*status));
                let (cache, key) = (self.cache.clone(), invite_card::state_key(uid));
                cx.background_spawn(async move {
                    if let Err(e) = cache.set_state(&key, &value) {
                        tracing::warn!("invite answer is not stored: {e}");
                    }
                })
                .detach();
                let text = match delivery {
                    Delivery::Server => tr!("reading-invite-sent-server"),
                    Delivery::Mail { imported: true } => tr!("reading-invite-sent-imported"),
                    Delivery::Mail { imported: false } => tr!("reading-invite-sent-mail"),
                };
                cx.emit(ReadingEvent::Toast(Toast::Success, text));
                if let Some(row) = letter.filter(|_| crate::prefs::get(cx).calendar.delete_answered) {
                    cx.emit(ReadingEvent::TrashAnswered(row));
                }
                cx.notify();
            }
            Event::InviteFailed { uid, error } => {
                if let Some(card) = self.card_for(uid) {
                    card.sending = None;
                }
                cx.emit(ReadingEvent::Toast(Toast::Error, tr!("reading-invite-failed", error = error.as_str())));
                cx.notify();
            }
            Event::Me { address, .. } => {
                self.me = Some(address.email.clone());
                cx.notify();
            }
            Event::Senders(list) => self.mine = list.iter().map(|s| s.address.email.clone()).collect(),
            Event::ThreadReady { thread, .. } => self.thread_ready(thread, cx),
            Event::EventsLoaded { .. } | Event::CalendarFailed(_) => self.invite_sync(ev, cx),
            _ => {}
        }
    }

    /// Запрос не ушёл (сервера нет, поток синхронизации остановился): ничего не ждём.
    pub(crate) fn not_sent(&mut self, req: &SyncRequest, cx: &mut Context<Self>) {
        match req {
            SyncRequest::Blob(a) => {
                if self.smime_waits(&a.blob_id)
                    && let Some(id) = self.reading.as_ref().map(|r| r.row.id.clone())
                {
                    self.smime_failed(&id, tr!("reading-blob-unavailable"), cx);
                }
                self.in_flight.remove(&a.blob_id);
                self.pending_files.retain(|(p, _)| p.blob_id != a.blob_id);
                self.attachment_failed(&a.blob_id, &tr!("reading-blob-unavailable"), cx);
                if let Some(card) = self.reading.as_mut().and_then(|r| r.invite.as_mut()).filter(|c| c.waits_for(&a.blob_id)) {
                    card.state = State::Failed(tr!("reading-blob-unavailable"));
                }
            }
            SyncRequest::RespondInvite { invite, .. } => {
                if let Some(card) = self.card_for(&invite.event.uid) {
                    card.sending = None;
                }
            }
            SyncRequest::Thread(_) => self.thread_not_sent(),
            SyncRequest::Events { .. } => self.invite_events_failed(),
        }
        cx.notify();
    }

    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.replace(None);
        self.feed = None;
        cx.notify();
    }

    fn card_for(&mut self, uid: &str) -> Option<&mut Card> {
        self.reading.as_mut().and_then(|r| r.invite.as_mut()).filter(|c| c.uid() == Some(uid))
    }

    /// HTML → IR в воркере-песочнице, в фоновом исполнителе gpui; затем — картинки `cid:`.
    pub(crate) fn render_body(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &self.reading else { return };
        let Some(body) = r.body.clone() else { return };
        let id = r.row.id.clone();
        let worker = self.worker.clone();
        let task = cx.background_spawn(async move {
            // Отравленный мьютекс (паника в прошлом разборе) не мешает: клиент воркера сам перезапускает процесс.
            let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
            let req = match (body.html, body.text) {
                (Some(h), _) => Request::Html(h),
                (None, Some(t)) => Request::Text(t),
                _ => Request::Text(String::new()),
            };
            (w.render(&req), w.level())
        });
        cx.spawn(async move |this, cx| {
            let (result, level) = task.await;
            this.update(cx, |this, cx| {
                cx.emit(ReadingEvent::Sandbox(level));
                let doc = result.unwrap_or_else(|e| {
                    let text = tr!("reading-error", error = e.to_string());
                    cx.emit(ReadingEvent::Error(text.clone()));
                    postman_ir::from_plain(&text)
                });
                if let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) {
                    r.doc = Some(doc);
                }
                this.ensure_images(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Картинки `cid:` текущего письма, за которыми ещё не ходили.
    pub(crate) fn ensure_images_pub(&mut self, cx: &mut Context<Self>) {
        self.ensure_images(cx);
    }

    fn ensure_images(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &mut self.reading else { return };
        let (Some(doc), Some(body)) = (&r.doc, &r.body) else { return };
        let requested = &mut r.requested;
        let wanted: Vec<AttachmentRow> = ir_view::cids(doc)
            .into_iter()
            .filter(|cid| requested.insert(cid.clone()))
            .filter_map(|cid| body.attachments.iter().find(|a| a.cid.as_deref() == Some(cid.as_str())).cloned())
            .collect();
        for a in wanted {
            self.fetch(&a, cx);
        }
    }

    /// Приглашение во вложениях — на разбор в песочницу (сначала байты).
    fn ensure_invite(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &mut self.reading else { return };
        if r.invite.is_some() || r.task_mail.is_some() {
            return;
        }
        let Some(a) = r.body.as_ref().and_then(|b| b.attachments.iter().find(|a| attachments::is_invite(a))).cloned() else {
            return;
        };
        r.invite = Some(Card::loading(a.blob_id.clone()));
        self.fetch(&a, cx);
    }

    /// Байты вложения: демо — сразу; живой ящик — из хранилища (в фоне), иначе с сервера.
    pub(crate) fn fetch(&mut self, a: &AttachmentRow, cx: &mut Context<Self>) {
        // Вложение расшифрованного письма (S5-12 Ф2) — из воркера по ключу сеанса.
        if self.fetch_part(a, cx) {
            return;
        }
        if !self.live {
            match postman_demo::blob(&a.blob_id) {
                Some(data) => self.blob_ready(&a.blob_id, Arc::new(data), cx),
                None => self.blob_failed(&a.blob_id, tr!("reading-blob-unavailable"), cx),
            }
            return;
        }
        if !self.in_flight.insert(a.blob_id.clone()) {
            return;
        }
        let (cache, a) = (self.cache.clone(), a.clone());
        let task = cx.background_spawn(async move {
            let local = cache.blob(&a.blob_id).ok().flatten();
            (a, local)
        });
        cx.spawn(async move |this, cx| {
            let (a, local) = task.await;
            this.update(cx, |this, cx| match local {
                Some(data) => this.blob_ready(&a.blob_id, Arc::new(data), cx),
                None => cx.emit(ReadingEvent::Sync(SyncRequest::Blob(a))),
            })
            .ok();
        })
        .detach();
    }

    /// Байты пришли: приглашение — на разбор, картинки — на декодирование, ждущие действия — выполнить.
    pub(crate) fn blob_ready_pub(&mut self, blob_id: &str, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        self.blob_ready(blob_id, data, cx);
    }

    pub(crate) fn blob_failed_pub(&mut self, blob_id: &str, error: String, cx: &mut Context<Self>) {
        self.blob_failed(blob_id, error, cx);
    }

    fn blob_ready(&mut self, blob_id: &str, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        self.in_flight.remove(blob_id);
        if self.smime_waits(blob_id) {
            self.smime_verify(data.clone(), cx);
        }
        if let Some(r) = &self.reading {
            let id = r.row.id.clone();
            let invite = r.invite.as_ref().is_some_and(|c| c.waits_for(blob_id));
            let cids: Vec<String> = r
                .body
                .iter()
                .flat_map(|b| &b.attachments)
                .filter(|a| a.blob_id == blob_id)
                .filter_map(|a| a.cid.clone())
                .filter(|c| r.requested.contains(c) && !r.images.contains_key(c))
                .collect();
            if invite {
                self.parse_invite(id.clone(), data.clone(), cx);
            }
            for cid in cids {
                self.decode_image(id.clone(), cid, data.clone(), cx);
            }
        }
        self.attachment_ready(blob_id, &data, cx);
        let (ready, rest) = std::mem::take(&mut self.pending_files).into_iter().partition(|(a, _)| a.blob_id == blob_id);
        self.pending_files = rest;
        for (a, act) in ready {
            self.do_file(a, act, data.clone(), cx);
        }
        cx.notify();
    }

    fn blob_failed(&mut self, blob_id: &str, error: String, cx: &mut Context<Self>) {
        self.in_flight.remove(blob_id);
        if self.smime_waits(blob_id)
            && let Some(id) = self.reading.as_ref().map(|r| r.row.id.clone())
        {
            self.smime_failed(&id, error.clone(), cx);
            return;
        }
        let mut user = self.pending_files.iter().any(|(a, _)| a.blob_id == blob_id);
        self.pending_files.retain(|(a, _)| a.blob_id != blob_id);
        if let Some(card) = self.reading.as_mut().and_then(|r| r.invite.as_mut()).filter(|c| c.waits_for(blob_id)) {
            card.state = State::Failed(error.clone());
            user = true;
        }
        user |= self.attachment_failed(blob_id, &error, cx);
        // Фоновая проверка Office на макросы не скачалась — без тоста: щиток просто не появится.
        if user {
            cx.emit(ReadingEvent::Toast(Toast::Error, tr!("reading-blob-failed", error = error)));
        } else {
            tracing::warn!("attachment {blob_id} is not downloaded: {error}");
        }
        cx.notify();
    }

    fn decode_image(&mut self, id: String, cid: String, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let worker = self.worker.clone();
        let task = cx.background_spawn(async move {
            let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
            w.decode_image(&data, MAX_IMAGE_SIDE).and_then(render_image)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(image) => {
                    if let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) {
                        r.images.insert(cid, image);
                        cx.notify();
                    }
                }
                Err(e) => tracing::warn!("inline image {cid} is not shown: {e}"),
            })
            .ok();
        })
        .detach();
    }

    /// `.ics` → приглашение в песочнице; запомненный ответ — из хранилища.
    fn parse_invite(&mut self, id: String, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let (worker, cache) = (self.worker.clone(), self.cache.clone());
        let tz = crate::prefs::get(cx).calendar.zone();
        let task = cx.background_spawn(async move {
            let parsed = {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.parse_calendar(&data)
            };
            let v = parsed?;
            // VTODO — письмо-поручение (S6-10), не приглашение.
            if let Ok(task) = postman_calendar::assign::Incoming::from_worker(&v) {
                return Ok(Err(Box::new(task)));
            }
            let inv = Invitation::from_worker(&v, &tz)?;
            let stored = cache.state(&invite_card::state_key(&inv.event.uid)).ok().flatten();
            let answer = stored.as_deref().and_then(invite_card::decode);
            Ok::<_, String>(Ok((inv, answer)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return };
                if let Ok(Err(task)) = &result {
                    r.invite = None;
                    r.task_mail = Some(task.clone());
                    cx.emit(ReadingEvent::TaskMail(task.clone()));
                    cx.notify();
                    return;
                }
                let Some(card) = r.invite.as_mut() else { return };
                match result {
                    Ok(Ok((inv, answer))) => {
                        card.state = State::Ready(Box::new(inv));
                        card.answer = answer;
                    }
                    Ok(Err(_)) => {}
                    Err(e) => card.state = State::Failed(e),
                }
                this.invite_ready(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Кнопка карточки приглашения. Демо — «нужен сервер», статус не меняется.
    pub(crate) fn answer_invite(&mut self, status: Partstat, cx: &mut Context<Self>) {
        if !self.live {
            cx.emit(ReadingEvent::NeedsServer);
            return;
        }
        let Some(r) = &mut self.reading else { return };
        let in_reply_to = r.body.as_ref().and_then(|b| b.message_id.clone());
        let Some(card) = &mut r.invite else { return };
        let Some(inv) = card.invitation().cloned() else { return };
        card.sending = Some(status);
        cx.emit(ReadingEvent::Sync(SyncRequest::RespondInvite { invite: Box::new(inv), status, in_reply_to }));
        cx.notify();
    }

    /// «Открыть» / «Сохранить как»: сначала байты, потом действие.
    pub(crate) fn file_action(&mut self, a: AttachmentRow, act: FileAct, cx: &mut Context<Self>) {
        if self.pending_files.iter().any(|(p, x)| p.blob_id == a.blob_id && *x == act) {
            return;
        }
        self.pending_files.push((a.clone(), act));
        self.fetch(&a, cx);
        cx.notify();
    }

    fn do_file(&mut self, a: AttachmentRow, act: FileAct, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let name = files::safe_name(&a.name);
        match act {
            // Расшифрованное вложение (S5-12 Ф2): «Открыть» положило бы его на диск во временную папку — только
            // «Сохранить» по выбору пользователя (решение владельца №4).
            FileAct::Open if a.blob_id.starts_with(crate::smime_enc::PART) => {
                cx.emit(ReadingEvent::Toast(Toast::Info, tr!("smime-enc-part-open")));
            }
            FileAct::Open if self.risk_of(&a) != postman_core::attach::Risk::Safe => {
                // Макросы (по имени или найденные воркером) и исполняемые — только «Сохранить» (S5-07).
                let text = tr!("reading-file-failed", name = name, error = tr!("att-status-macros"));
                cx.emit(ReadingEvent::Toast(Toast::Warning, text));
            }
            FileAct::Open => {
                let dir = self.open_dir.clone();
                let task = cx.background_spawn(async move { files::open(&dir, &a.name, &data).map(|_| ()) });
                cx.spawn(async move |this, cx| {
                    if let Err(e) = task.await {
                        let text = tr!("reading-file-failed", name = name, error = e.to_string());
                        this.update(cx, |_, cx| cx.emit(ReadingEvent::Toast(Toast::Error, text))).ok();
                    }
                })
                .detach();
            }
            FileAct::SaveAs => {
                // Диалог ОС не держит поток окна: ответ приходит каналом.
                let path = cx.prompt_for_new_path(&downloads_dir(), Some(&name));
                cx.spawn(async move |this, cx| {
                    let path = match path.await {
                        Ok(Ok(Some(p))) => p,
                        Ok(Ok(None)) | Err(_) => return,
                        Ok(Err(e)) => {
                            let text = tr!("reading-file-failed", name = name, error = e.to_string());
                            this.update(cx, |_, cx| cx.emit(ReadingEvent::Toast(Toast::Error, text))).ok();
                            return;
                        }
                    };
                    let written = cx.background_executor().spawn(async move { files::write(&path, &data).map(|()| path) }).await;
                    let event = match written {
                        Ok(p) => ReadingEvent::Toast(Toast::Success, tr!("reading-saved", path = p.display().to_string())),
                        Err(e) => ReadingEvent::Toast(Toast::Error, tr!("reading-file-failed", name = name, error = e.to_string())),
                    };
                    this.update(cx, |_, cx| cx.emit(event)).ok();
                })
                .detach();
            }
        }
    }

    /// Опасность вложения с учётом проверки содержимого (S5-07).
    fn risk_of(&self, a: &AttachmentRow) -> postman_core::attach::Risk {
        let scanned = self.reading.as_ref().and_then(|r| r.scans.get(&a.blob_id)).map(|s| s.macros);
        postman_core::attach::risk_with(&a.name, scanned)
    }
}

/// Пиксели воркера (RGBA) → кадр gpui (BGRA).
pub(crate) fn render_image(rgba: Rgba) -> Result<Arc<RenderImage>, String> {
    let Rgba { width, height, mut pixels } = rgba;
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels).ok_or_else(|| format!("{width}x{height}: pixel buffer size mismatch"))?;
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// Каталог по умолчанию для «Сохранить как».
fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_else(|| PathBuf::from("."))
}

impl Render for ReadingPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        let reveal = std::mem::take(&mut self.reveal);
        let l = look(cx);
        let Some(r) = &self.reading else {
            return div()
                .flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(l.text2())
                .child(tr!("reading-empty"))
                .into_any_element();
        };
        let scheduled = self.scheduled_bar(cx);
        // Стопка беседы (S5-05б, макет «Беседа»): тема, сроки, «N писем · M участников», письма карточками — у каждого
        // своя шапка.
        if self.stacked(cx) {
            let title = div()
                .text_size(l.px(pets_ui::theme::Metric::TypeSubtitle))
                .line_height(px(26.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(r.row.subject.clone());
            let top = div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .pb(px(8.))
                .child(title)
                .children(crate::shared::strip(&r.row, &l, cx))
                .child(crate::lifecycle::header_row(&r.row, &l, cx))
                .children(self.stack_info(cx));
            return div()
                .id("reading")
                .flex()
                .flex_col()
                .size_full()
                .px(px(24.))
                .py(px(20.))
                .gap(px(4.))
                .overflow_y_scroll()
                .child(top)
                .children(scheduled)
                .children(self.stack_view(r, cx))
                .into_any_element();
        }
        let name = if r.row.from_name.is_empty() { r.row.from_addr.clone() } else { r.row.from_name.clone() };
        let to = r
            .body
            .as_ref()
            .map(|b| b.to.iter().map(|a| if a.name.is_empty() { a.email.clone() } else { a.name.clone() }).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        // Совпадения поиска — по порядку чтения: тема, затем текст; текущее (F3) — плотной подложкой (S5-14).
        let feed = self.feed_view(cx);
        let words: &[String] = self.marks.as_ref().map_or(&[], |(_, m)| m.as_slice());
        let plain = feed.is_none() && r.preview.is_none();
        let target = (reveal && plain).then(|| ir_view::Reveal { scroll: self.scroll.clone(), view: cx.entity_id() });
        let hits = ir_view::Hits::new(words, self.hit, 0).with_reveal(target);
        let subject = {
            let (styled, at) = ir_view::hit_text(r.row.subject.clone(), Vec::new(), &hits, &l);
            let layout = styled.layout().clone();
            match at {
                Some(at) => ir_view::reveal_at(styled.into_any_element(), layout, at, &hits),
                None => styled.into_any_element(),
            }
        };
        let neutral = l.cat(postman_core::palette::NEUTRAL);
        let avatar = div()
            .size(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(neutral.bg)
            .text_color(neutral.text)
            .child(crate::list::initials(&name));
        // Подпись S/MIME (S5-12): значок справа от отправителя и даты, карточка — под шапкой.
        let lock = self.lock_badge(&l, cx);
        let seal = self.smime_badge(&l, cx);
        let seal_card = self.smime_card(&l);
        let lock_line = self.lock_line(&l, cx);
        // Шапка (макет главного окна): тема, затем аватар, отправитель с адресом, «Кому: … · дата».
        let header = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .pb(px(12.))
            .border_b_1()
            .border_color(l.border())
            .child(
                div()
                    .text_size(l.px(pets_ui::theme::Metric::TypeSubtitle))
                    .line_height(px(26.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(subject),
            )
            .child(
                div().flex().items_center().gap(px(12.)).child(avatar).child(
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
                                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(name))
                                .child(div().min_w_0().truncate().text_color(l.text2()).child(format!("<{}>", r.row.from_addr))),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(l.px(pets_ui::theme::Metric::TypeCaption))
                                .text_color(l.text2())
                                .child(tr!("frame-reading-to", to = to, date = format_date(r.row.received_at))),
                        ),
                )
                .children(lock)
                .children(seal),
            )
            // Общий ящик (S5-28): кто взял письмо в работу и «Взять» / «Забрать себе» / «Вернуть в очередь».
            .children(crate::shared::strip(&r.row, &l, cx))
            // Сроки письма и действия «Срок», «Напомнить», «Ждать ответа», «Исчезнет» (S5-04).
            .child(crate::lifecycle::header_row(&r.row, &l, cx));
        // Вид «как Outlook» — строка «Беседа: N писем · ← · →» (S5-05б), лента — переключатель «Беседа (N)».
        let bar = match self.view(cx) {
            postman_core::prefs::Conversation::Outlook => self.outlook_bar(cx),
            _ => self.feed_bar(cx),
        };
        // Приглашение боковой панелью справа (S5-24, «Настройки → Чтение»): над письмом — строка, панель — рядом.
        let side = r.invite.as_ref().and_then(|c| invite_card::render_side(c, self.me.as_deref(), cx));
        // Лента беседы — на месте письма (вложения и приглашение — у письма в обычном виде).
        if let Some(feed) = feed {
            let main = div()
                .id("reading")
                .flex()
                .flex_col()
                .size_full()
                .px(px(24.))
                .py(px(20.))
                .gap(px(12.))
                .overflow_y_scroll()
                .child(header)
                .children(seal_card)
                .children(scheduled)
                .children(bar)
                .child(feed);
            return with_side(main, side);
        }
        let strip = self.ribbon(r, cx);
        // Открыт просмотр вложения — он на месте текста письма и карточки приглашения (DESIGN «Вложения»).
        if let Some(p) = &r.preview {
            let pane = self.preview_pane(p, window, cx);
            return div()
                .id("reading")
                .flex()
                .flex_col()
                .size_full()
                .px(px(24.))
                .pt(px(20.))
                .pb(px(16.))
                .gap(px(12.))
                .overflow_y_scroll()
                .child(header)
                .children(seal_card)
                .children(scheduled)
                .children(bar)
                .children(strip)
                .child(pane)
                .into_any_element();
        }
        let card = r.invite.as_ref().map(|c| invite_card::render(c, self.me.as_deref(), cx)).or_else(|| {
            // Письмо-поручение: карточка над текстом; своя копия задачи — из раздела «Задачи».
            let inc = r.task_mail.as_ref()?;
            let copy = self.tasks.as_ref().and_then(|t| t.upgrade()).and_then(|t| t.read(cx).tasks.iter().find(|x| x.uid == inc.task.uid).cloned());
            let me = self.me.clone().unwrap_or_default();
            let tz = crate::prefs::get(cx).calendar.zone();
            let weak = cx.entity().downgrade();
            let uid = inc.task.uid.clone();
            let on = move |action: Option<postman_calendar::assign::Action>, _: &mut Window, cx: &mut App| {
                let uid = uid.clone();
                let _ = weak.update(cx, |_, cx| cx.emit(ReadingEvent::TaskAction { uid, action }));
            };
            Some(crate::tasks::mail_card(inc, copy.as_ref(), &me, &tz, cx, on))
        });
        let body: AnyElement = match &r.doc {
            None => div().text_color(l.text2()).child("…").into_any_element(),
            Some(doc) => {
                let mut col = div().flex().flex_col().gap_2();
                if doc.remote_images_blocked > 0 {
                    col = col.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(l.warning_bg())
                            .child(tr!("reading-remote-blocked", n = doc.remote_images_blocked)),
                    );
                }
                col.children(ir_view::blocks_marked(&doc.blocks, "doc", &r.images, &hits, cx)).into_any_element()
            }
        };
        // Текст ещё не пришёл — совпадения только в теме.
        let n = hits.seen();
        self.hits.set(n);
        let matches = self.match_bar(n, &l);
        let main = div()
            .id("reading")
            .flex()
            .flex_col()
            .size_full()
            .px(px(24.))
            .py(px(20.))
            .gap(px(12.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(header)
            .children(seal_card)
            .children(scheduled)
            .children(bar)
            .children(strip)
            .children(card)
            .children(matches)
            .children(lock_line)
            .child(body);
        with_side(main, side)
    }
}

/// Письмо и боковая панель приглашения справа (S5-24); без панели — письмо как есть.
fn with_side(main: Stateful<Div>, side: Option<AnyElement>) -> AnyElement {
    match side {
        None => main.into_any_element(),
        Some(side) => div().flex().size_full().child(main.flex_1().min_w_0()).child(side).into_any_element(),
    }
}

/// Номер совпадения после шага `delta` по кругу из `n` (текущий за пределами — как первый).
fn step(current: usize, delta: isize, n: usize) -> usize {
    let n = isize::try_from(n).unwrap_or(isize::MAX).max(1);
    let current = isize::try_from(current).unwrap_or(0).min(n - 1);
    usize::try_from((current + delta).rem_euclid(n)).unwrap_or(0)
}

pub(crate) fn sandbox_label(level: Level) -> String {
    match level {
        Level::Full => tr!("sandbox-full"),
        Level::Partial => tr!("sandbox-partial"),
        Level::None => tr!("sandbox-none"),
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{SyncRequest, render_image, step};
    use postman_cache::AttachmentRow;
    use postman_core::sync::Command;
    use postman_worker::Rgba;

    /// F3 / Shift+F3 — по кругу; номер за пределами (совпадений стало меньше) — от последнего.
    #[test]
    fn hits_step_around() {
        assert_eq!(step(0, 1, 3), 1);
        assert_eq!(step(2, 1, 3), 0);
        assert_eq!(step(0, -1, 3), 2);
        assert_eq!(step(7, 1, 3), 0);
        assert_eq!(step(7, -1, 3), 1);
        assert_eq!(step(0, 1, 1), 0);
    }

    #[test]
    fn worker_pixels_become_bgra() {
        let image = render_image(Rgba { width: 2, height: 1, pixels: vec![1, 2, 3, 4, 5, 6, 7, 8] }).unwrap();
        assert_eq!(image.as_bytes(0), Some(&[3, 2, 1, 4, 7, 6, 5, 8][..]));
        assert!(render_image(Rgba { width: 2, height: 2, pixels: vec![0; 4] }).is_err());
    }

    #[test]
    fn requests_become_sync_commands() {
        let a = AttachmentRow { blob_id: "b1".into(), name: "a.pdf".into(), mime: "application/pdf".into(), size: 1, cid: None, inline: false };
        match SyncRequest::Blob(a).command() {
            Command::Blob { blob_id, name, mime } => assert_eq!((blob_id.as_str(), name.as_str(), mime.as_str()), ("b1", "a.pdf", "application/pdf")),
            _ => panic!("не Blob"),
        }
    }
}
