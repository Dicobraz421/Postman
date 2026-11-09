//! Подписки на календари `.ics` (S8-44): календарь по ссылке, только для чтения. Скачанный файл лежит рядом с данными
//! Postman — без сети показываются прежние события; разбирает его только воркер, под показанный период. В модели
//! подписка — как календарь коллеги: `Owner` с ключом `ics:…` даёт цвет, скрытие и запрет ответа; в список коллег
//! слева не попадает, у неё своя группа.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui_kit::*;
use postman_calendar::{Event, EventStatus, FreeBusy, Owner, Span};
use postman_core::prefs::CalendarSubscription;
use postman_worker::{Client, IcsCalendar, IcsEvent, MAX_ICS};

use super::CalendarView;

/// Ключ подписки в модели календаря.
const PREFIX: &str = "ics:";
/// Как часто проверять, не пора ли обновить.
const TICK: Duration = Duration::from_secs(300);

/// Подписка на экране.
pub(super) struct Sub {
    pub(super) id: String,
    pub(super) url: String,
    pub(super) name: String,
    events: Vec<Event>,
    /// Последнее обновление не удалось: показаны прежние события.
    pub(super) failed: bool,
    fetching: bool,
    checked: Option<Instant>,
}

impl Sub {
    fn new(s: CalendarSubscription) -> Self {
        Self { id: id_of(&s.url), url: s.url, name: s.name, events: Vec::new(), failed: false, fetching: false, checked: None }
    }

    pub(super) fn key(&self) -> String {
        format!("{PREFIX}{}", self.id)
    }

    fn owner(&self) -> Owner {
        Owner { email: self.key(), name: self.shown_name() }
    }

    pub(super) fn shown_name(&self) -> String {
        if self.name.is_empty() { pets_i18n::tr!("cal-sub-new") } else { self.name.clone() }
    }
}

/// Событие подписки (`Owner` с ключом `ics:…`).
pub(super) fn is_subscription(e: &Event) -> bool {
    e.owner.as_ref().is_some_and(is_sub_owner)
}

fn is_sub_owner(o: &Owner) -> bool {
    o.email.starts_with(PREFIX)
}

/// Имя файла подписки: по ссылке.
fn id_of(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.trim().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Имя подписки с ключом `key` (`ics:…`) для строк настроек: ключ пользователю ничего не говорит.
pub(crate) fn name_of_key(cal: &postman_core::prefs::Calendar, key: &str) -> Option<String> {
    let id = key.strip_prefix(PREFIX)?;
    let sub = cal.subscriptions.iter().find(|s| id_of(&s.url) == id);
    Some(sub.map(|s| s.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| pets_i18n::tr!("cal-sub-new")))
}

/// Каталог файлов подписок; демо и снимки — временный: данные Postman они не трогают.
fn dir(live: bool) -> PathBuf {
    if live { crate::data_dir().join("subscriptions") } else { std::env::temp_dir().join("postman-demo-subscriptions") }
}

fn event_of(sub: &Sub, e: IcsEvent) -> Event {
    Event {
        recurrence: None,
        alert_secs: None,
        id: format!("{}-{}-{}", sub.key(), e.uid, e.start),
        uid: format!("{}-{}", sub.id, e.uid),
        calendar_ids: Vec::new(),
        title: e.title,
        description: e.description,
        location: e.location,
        span: Span::new(e.start, e.end),
        all_day: e.all_day,
        status: if e.tentative { EventStatus::Tentative } else { EventStatus::Confirmed },
        free_busy: if e.transparent { FreeBusy::Free } else { FreeBusy::Busy },
        participants: Vec::new(),
        recurring: e.recurring,
        keywords: Vec::new(),
        sequence: 0,
        owner: Some(sub.owner()),
    }
}

/// Записать файл подписки и его ETag: сначала во временный — сбой не портит прежний.
fn store(live: bool, id: &str, body: &[u8], etag: Option<&str>) -> Result<(), String> {
    let dir = dir(live);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (file, tmp) = (dir.join(format!("{id}.ics")), dir.join(format!("{id}.tmp")));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
    let tag = dir.join(format!("{id}.etag"));
    match etag {
        Some(t) => std::fs::write(tag, t).map_err(|e| e.to_string()),
        None => {
            let _ = std::fs::remove_file(tag);
            Ok(())
        }
    }
}

/// Подписки из настроек: первая загрузка — по файлам, затем обновление по времени.
impl CalendarView {
    /// Воркер — у области чтения, она создаётся раньше; подписки и таймер обновления — отсюда.
    pub(crate) fn start_subscriptions(&mut self, worker: Arc<Mutex<Client>>, cx: &mut Context<Self>) {
        self.worker = Some(worker);
        self.subs = crate::prefs::get(cx).calendar.subscriptions.into_iter().map(Sub::new).collect();
        self.load_opened_prefs(cx);
        self.start_reminders(cx);
        // Первый проход — после создания окна: живой ящик включается уже потом (`go_live`), а демо-проход писал бы своё и
        // считал подписку проверенной.
        cx.spawn(async move |this, cx| {
            loop {
                let pass = this.update(cx, |v, cx| {
                    v.load_subs(cx);
                    v.fetch_subs(false, cx);
                });
                if pass.is_err() {
                    return;
                }
                cx.background_executor().timer(TICK).await;
            }
        })
        .detach();
    }

    /// Живой ящик включён: подписки — заново, из своего каталога и с сервера; демо-проход (если успел) не считается.
    pub(super) fn subs_go_live(&mut self, cx: &mut Context<Self>) {
        for sub in &mut self.subs {
            sub.events.clear();
            sub.fetching = false;
            sub.checked = None;
            sub.failed = false;
        }
        self.load_subs(cx);
        self.fetch_subs(false, cx);
    }

    /// События подписок — за показанный период, из файлов.
    pub(super) fn load_subs(&mut self, cx: &mut Context<Self>) {
        let Some(worker) = self.worker.clone() else { return };
        let (from, to) = self.range();
        let tz = self.tz.iana_name().unwrap_or("UTC").to_owned();
        let live = self.live;
        for id in self.subs.iter().map(|s| s.id.clone()).collect::<Vec<_>>() {
            let (worker, tz, name) = (worker.clone(), tz.clone(), id.clone());
            let task = cx.background_spawn(async move {
                let Ok(bytes) = std::fs::read(dir(live).join(format!("{name}.ics"))) else { return None };
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                let parsed = w.ics(&bytes, from, to, &tz);
                if let Some(cut) = w.take_cut() {
                    tracing::warn!("calendar subscription cut: {cut}");
                }
                Some(parsed)
            });
            cx.spawn(async move |this, cx| {
                let parsed = task.await;
                this.update(cx, |v, cx| v.sub_loaded(&id, (from, to), parsed, cx)).ok();
            })
            .detach();
        }
    }

    fn sub_loaded(&mut self, id: &str, range: (i64, i64), parsed: Option<Result<IcsCalendar, String>>, cx: &mut Context<Self>) {
        if self.range() != range {
            return;
        }
        let Some(sub) = self.subs.iter_mut().find(|s| s.id == id) else { return };
        match parsed {
            Some(Ok(cal)) => {
                if sub.name.is_empty()
                    && let Some(name) = cal.name
                {
                    sub.name = name.clone();
                    let url = sub.url.clone();
                    crate::prefs::update(cx, move |p| {
                        if let Some(s) = p.calendar.subscriptions.iter_mut().find(|s| s.url == url) {
                            s.name = name;
                        }
                    });
                }
                let events = cal.events;
                let list: Vec<Event> = events.into_iter().map(|e| event_of(sub, e)).collect();
                sub.events = list;
            }
            Some(Err(e)) => {
                tracing::warn!("subscription {id} is not parsed: {e}");
                sub.failed = true;
            }
            None => {}
        }
        self.apply_subs(cx);
    }

    /// События и владельцы подписок — поверх событий серверов; открытое событие остаётся открытым.
    pub(super) fn apply_subs(&mut self, cx: &mut Context<Self>) {
        let open = self.selected.and_then(|i| self.events.get(i)).map(|e| (e.uid.clone(), e.span.start));
        self.events.retain(|e| !is_subscription(e));
        self.events.extend(self.subs.iter().flat_map(|s| s.events.iter().cloned()));
        self.colleagues.retain(|o| !is_sub_owner(o));
        self.colleagues.extend(self.subs.iter().map(Sub::owner));
        self.mix_opened();
        self.selected = open.and_then(|(uid, start)| self.events.iter().position(|e| e.uid == uid && e.span.start == start));
        self.remember_colors(cx);
        cx.notify();
    }

    /// Обновить подписки: `force` — все сразу (добавлена, «Обновить»), иначе — чей срок вышел.
    pub(super) fn fetch_subs(&mut self, force: bool, cx: &mut Context<Self>) {
        let every = Duration::from_secs(u64::from(crate::prefs::get(cx).calendar.subscription_minutes) * 60);
        let (live, from) = (self.live, self.range().0);
        for sub in self.subs.iter_mut().filter(|s| !s.fetching && (force || s.checked.is_none_or(|t| t.elapsed() >= every))) {
            sub.fetching = true;
            let (id, url, name) = (sub.id.clone(), sub.url.clone(), sub.id.clone());
            let task = cx.background_spawn(async move {
                if !live {
                    return store(live, &name, &postman_demo::calendar::holidays(from), None);
                }
                let tag = dir(live).join(format!("{name}.etag"));
                let etag = std::fs::read_to_string(tag).ok();
                match postman_backend_dav::fetch_changed(&url, MAX_ICS, etag.as_deref())? {
                    Some((body, etag)) => store(live, &name, &body, etag.as_deref()),
                    None => Ok(()),
                }
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |v, cx| v.sub_fetched(&id, result, cx)).ok();
            })
            .detach();
        }
    }

    fn sub_fetched(&mut self, id: &str, result: Result<(), String>, cx: &mut Context<Self>) {
        if let Some(sub) = self.subs.iter_mut().find(|s| s.id == id) {
            sub.fetching = false;
            sub.checked = Some(Instant::now());
            sub.failed = result.is_err();
            if let Err(e) = result {
                tracing::info!("subscription {id}: {e}");
            }
        }
        self.load_subs(cx);
        cx.notify();
    }

    /// Добавить календарь по ссылке; ссылка не годится — `false`.
    pub(crate) fn add_subscription(&mut self, url: &str, cx: &mut Context<Self>) -> bool {
        let url = url.trim().to_owned();
        if !postman_backend_dav::public_url_ok(&url) {
            return false;
        }
        if self.subs.iter().all(|s| s.url != url) {
            let sub = CalendarSubscription { url, name: String::new() };
            let saved = sub.clone();
            crate::prefs::update(cx, move |p| p.calendar.subscriptions.push(saved));
            self.subs.push(Sub::new(sub));
            self.fetch_subs(true, cx);
        }
        true
    }

    /// Удалить подписку: события уходят, файл — тоже.
    pub(super) fn remove_subscription(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(i) = self.subs.iter().position(|s| s.id == id) else { return };
        let sub = self.subs.remove(i);
        let (url, key) = (sub.url.clone(), sub.key());
        crate::prefs::update(cx, move |p| {
            p.calendar.subscriptions.retain(|s| s.url != url);
            p.calendar.calendar_colors.remove(&key);
        });
        self.color_menu = None;
        for ext in ["ics", "etag"] {
            let _ = std::fs::remove_file(dir(self.live).join(format!("{}.{ext}", sub.id)));
        }
        self.apply_subs(cx);
    }

    /// В подписке есть события (снимок).
    pub(crate) fn has_subscription_events(&self) -> bool {
        self.subs.iter().any(|s| !s.events.is_empty())
    }

    /// Подписки на экране: имя и «не обновился» (сцены).
    pub(crate) fn subs_state(&self) -> Vec<(String, bool)> {
        self.subs.iter().map(|s| (s.shown_name(), s.failed)).collect()
    }

    /// Идёт обновление подписки (сцены).
    pub(crate) fn subs_fetching(&self) -> bool {
        self.subs.iter().any(|s| s.fetching)
    }

    /// Все подписки проверены после `since` (сцены).
    pub(crate) fn subs_checked_since(&self, since: Instant) -> bool {
        self.subs.iter().all(|s| !s.fetching && s.checked.is_some_and(|t| t >= since))
    }

    /// События подписок показанного периода (сцены).
    pub(crate) fn sub_events(&self) -> Vec<&Event> {
        self.events.iter().filter(|e| is_subscription(e)).collect()
    }

    /// Номера событий подписок на весь день (сцены).
    pub(crate) fn sub_events_all_day(&self) -> Vec<usize> {
        self.events.iter().enumerate().filter(|(_, e)| is_subscription(e) && e.all_day).map(|(i, _)| i).collect()
    }

    /// Названия событий подписок без скрытых флажком, без повторов (сцены).
    pub(crate) fn visible_sub_titles(&self) -> Vec<String> {
        let mut titles: Vec<String> = self.events.iter().filter(|e| is_subscription(e) && !self.hidden(e)).map(|e| e.title.clone()).collect();
        titles.sort();
        titles.dedup();
        titles
    }

    /// Показать меню образца цвета подписки `n` (сцены).
    pub(crate) fn open_sub_menu(&mut self, n: usize, cx: &mut Context<Self>) {
        self.color_menu = self.subs.get(n).map(Sub::key);
        cx.notify();
    }

    /// С прошлой проверки подписок прошло `age`: тот же шаг таймера, что и в работе (сцены).
    pub(crate) fn expire_subs(&mut self, age: Duration, cx: &mut Context<Self>) {
        for sub in &mut self.subs {
            sub.checked = Instant::now().checked_sub(age);
        }
        self.fetch_subs(false, cx);
    }
}

#[cfg(test)]
mod tests {
    // Не `super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{CalendarSubscription, EventStatus, FreeBusy, IcsEvent, Sub, event_of, id_of, is_subscription, name_of_key};

    /// Экземпляры повтора отличаются временем, событие — только для чтения и подписано календарём.
    #[test]
    fn events_belong_to_the_subscription() {
        let sub = Sub::new(CalendarSubscription { url: "https://cal.example.test/a.ics".into(), name: "A".into() });
        let e = IcsEvent {
            uid: "u".into(),
            title: "T".into(),
            location: String::new(),
            description: String::new(),
            start: 10,
            end: 20,
            all_day: false,
            tentative: true,
            transparent: true,
            recurring: true,
        };
        let event = event_of(&sub, e);
        assert!(is_subscription(&event));
        assert_eq!(event.owner.as_ref().map(|o| o.name.as_str()), Some("A"));
        assert_eq!((event.status, event.free_busy), (EventStatus::Tentative, FreeBusy::Free));
        assert!(event.participants.is_empty());
        assert_eq!(id_of(" https://cal.example.test/a.ics "), sub.id);
    }

    /// В строках настроек — имя подписки, ключи коллег не трогаются.
    #[test]
    fn settings_show_the_name() {
        let url = "https://cal.example.test/a.ics".to_owned();
        let sub = Sub::new(CalendarSubscription { url: url.clone(), name: "A".into() });
        let cal = postman_core::prefs::Calendar { subscriptions: vec![CalendarSubscription { url, name: "A".into() }], ..Default::default() };
        assert_eq!(name_of_key(&cal, &sub.key()).as_deref(), Some("A"));
        assert_eq!(name_of_key(&cal, "colleague@example.test"), None);
    }
}
