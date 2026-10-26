//! Офлайн (S5-08, DESIGN «Действия и состояния», макет «Состояния»): сервер не отвечает — строка состояния
//! «Нет связи · повтор через N с», метка «нет связи» у аккаунта в дереве папок, тонкая полоса над списком с
//! «Повторить сейчас». Правки писем без связи копятся в очереди синхронизации (`postman_core::link`) и уходят, когда
//! связь вернётся, — так и сказано в полосе. Связь вернулась — всё снимается. Вместо прежнего тоста «Нет связи…».
//!
//! Состояние — [`Offline`] (без gpui, тесты); обратный отсчёт — один таймер к следующей смене числа, только пока связи
//! нет и до попытки: последние 10 с — по секунде, раньше — шагом 5 с (S5-14: без связи окно перерисовывается реже).

use std::time::{Duration, Instant};

use postman_core::link::Connection;

/// До стольких секунд до повтора счёт идёт по секунде, дальше — шагом [`STEP`] (вверх: «через 60 с», «55 с»…).
const EXACT: u64 = 10;
const STEP: u64 = 5;

/// Что изменилось с событием связи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// Связь только что пропала.
    Lost,
    /// Связи по-прежнему нет: новый срок повтора или число правок в очереди.
    Still,
    /// Связь вернулась.
    Back,
    /// Связь была и есть.
    None,
}

/// Связи нет: когда повтор и сколько правок ждёт отправки.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Offline {
    down: bool,
    retry_at: Option<Instant>,
    queued: usize,
}

impl Offline {
    pub(crate) fn is_down(&self) -> bool {
        self.down
    }

    pub(crate) fn queued(&self) -> usize {
        self.queued
    }

    /// Событие синхронизации о связи, полученное в `now`.
    pub(crate) fn on(&mut self, c: Connection, now: Instant) -> Change {
        match c {
            Connection::Offline { retry_in, queued } => {
                let was = self.down;
                self.down = true;
                self.retry_at = Some(now + Duration::from_secs(retry_in));
                self.queued = queued;
                if was { Change::Still } else { Change::Lost }
            }
            Connection::Online if self.down => {
                *self = Offline::default();
                Change::Back
            }
            Connection::Online => Change::None,
        }
    }

    /// Секунд до повтора; `Some(0)` — попытка идёт (срок прошёл, ответа ещё нет); связь есть — `None`.
    pub(crate) fn left(&self, now: Instant) -> Option<u64> {
        if !self.down {
            return None;
        }
        Some(self.retry_at.map_or(0, |t| t.saturating_duration_since(now).as_secs_f32().ceil() as u64))
    }

    /// Секунды для строки состояния: последние [`EXACT`] — точно, раньше — вверх до кратного [`STEP`].
    pub(crate) fn shown(&self, now: Instant) -> Option<u64> {
        let left = self.left(now)?;
        Some(if left > EXACT { left.div_ceil(STEP) * STEP } else { left })
    }

    /// Через сколько сменится [`Offline::shown`] — таймер строки состояния. Связь есть или попытка уже идёт
    /// («подключение…» до ответа синхронизации) — `None`: менять нечего.
    pub(crate) fn next_change(&self, now: Instant) -> Option<Duration> {
        if !self.down {
            return None;
        }
        let rest = self.retry_at?.checked_duration_since(now).filter(|d| !d.is_zero())?;
        let shown = self.shown(now)?;
        let below = if shown > EXACT { shown - STEP } else { shown.saturating_sub(1) };
        Some(rest.saturating_sub(Duration::from_secs(below)).max(Duration::from_millis(20)))
    }

    /// «Повторить сейчас»: попытка уже идёт (до ответа синхронизации).
    pub(crate) fn retry_now(&mut self, now: Instant) {
        if self.down {
            self.retry_at = Some(now);
        }
    }
}

/// Текст строки состояния без связи: «Нет связи · повтор через 25 с», попытка — «Нет связи · подключение…»; в
/// очереди правки — «· изменений в очереди: 3».
pub(crate) fn status_text(o: &Offline, now: Instant) -> Option<String> {
    let left = o.shown(now)?;
    let head = if left > 0 { pets_i18n::tr!("states-offline-retry", n = left as i64) } else { pets_i18n::tr!("states-offline-trying") };
    Some(match o.queued() {
        0 => head,
        n => format!("{head} · {}", pets_i18n::tr!("states-offline-queued", n = n as i64)),
    })
}

/// Сервер из адреса учётной записи — для полосы «Нет подключения к mail.example.ru».
pub(crate) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    host.to_owned()
}

impl crate::Mailer {
    /// Событие связи от синхронизации: строка состояния, метка у аккаунта, полоса над списком; пропала и вернулась —
    /// записью в истории уведомлений (без тоста).
    pub(crate) fn on_connection(&mut self, c: Connection, cx: &mut gpui_kit::Context<Self>) {
        use pets_ui::Level;
        let change = self.offline.on(c, Instant::now());
        let host = self.server_host();
        match change {
            Change::Lost => {
                let text = pets_i18n::tr!("states-lost", host = host.clone());
                self.toasts.update(cx, |t, cx| t.note(Level::Warning, text, cx));
            }
            Change::Back => {
                let text = pets_i18n::tr!("states-back", host = host.clone());
                self.toasts.update(cx, |t, cx| t.note(Level::Success, text, cx));
            }
            Change::Still | Change::None => {}
        }
        self.show_offline(host, change == Change::Lost, cx);
        self.offline_tick(cx);
        cx.notify();
    }

    /// Метка у аккаунта и полоса над списком — по состоянию связи (`fresh` — связь только что пропала: полосу
    /// показать снова, даже если её скрывали).
    pub(crate) fn show_offline(&mut self, host: String, fresh: bool, cx: &mut gpui_kit::Context<Self>) {
        let down = self.offline.is_down();
        let queued = self.offline.queued();
        self.folders.update(cx, |f, cx| {
            if f.offline != down {
                f.offline = down;
                cx.notify();
            }
        });
        self.list.update(cx, |l, cx| {
            l.offline = down.then_some((host, queued));
            if fresh || !down {
                l.offline_hidden = false;
            }
            cx.notify();
        });
    }

    /// Пока связи нет: «повтор через N с» — к следующей смене числа ([`Offline::next_change`]). Таймер один.
    fn offline_tick(&mut self, cx: &mut gpui_kit::Context<Self>) {
        if self.offline_ticking {
            return;
        }
        let Some(wait) = self.offline.next_change(Instant::now()) else { return };
        self.offline_ticking = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |m, cx| {
                m.offline_ticking = false;
                m.offline_tick(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Сервер учётной записи (демо — сервер демо-ящика, для снимков).
    pub(crate) fn server_host(&self) -> String {
        match self.live_account() {
            Some(a) => host_of(&a.url),
            None => format!("mail.{}", postman_demo::ME.rsplit('@').next().unwrap_or_default()),
        }
    }

    /// «Повторить сейчас»: синхронизация пробует сразу.
    pub(crate) fn retry_now(&mut self, cx: &mut gpui_kit::Context<Self>) {
        self.offline.retry_now(Instant::now());
        if self.backend.is_live() && !self.backend.send(postman_core::sync::Command::Reconnect) {
            self.sync_stopped(cx);
        }
        cx.notify();
    }
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "states", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/states.ftl")),
    ("en", include_str!("../i18n/en/states.ftl")),
];

/// Сцены `--screens` состояний ящика (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::settings::Usage;

    use crate::screens::Scene;
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Ящик заполнен на 94 % (S5-29): индикатор в строке состояния и тост «Очистить вложения…».
        Scene::new("quota", quota, quota_ready),
        // Нет связи: строка состояния с обратным отсчётом, метка у аккаунта, полоса над списком.
        Scene::new("offline", offline, offline_ready),
    ];

    fn quota(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Окно почты, как на макете: полоса «Автоответ включён» (образец сцены `vacation`) и строка состояния.
        m.close_settings(window, cx);
        m.set_section(Section::Mail, cx);
        m.close_menu(cx);
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        // 9,4 из 10 ГБ, как на макете; тост — заново (прежняя заполненность — ниже порога).
        let limit: u64 = 10 << 30;
        m.set_quota(None, cx);
        m.set_quota(Some(Usage { used: (limit * 94).div_ceil(100), limit }), cx);
        Ok(())
    }

    fn quota_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.toasts.read(cx).on_screen() > 0 && m.settings.quota.is_some() && !m.tabs.is_active(crate::tabs::TabKind::Settings)
    }

    fn offline(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.close_menu(cx);
        m.toasts.update(cx, |t, cx| t.hide_all(cx));
        // После `quota` — снова 12 % из 2 ГБ.
        if m.settings.quota.is_some_and(|u| u.percent() > postman_core::settings::WARN_PERCENT) {
            let limit: u64 = 2 << 30;
            m.set_quota(Some(Usage { used: limit / 8, limit }), cx);
        }
        let own = postman_demo::ME.to_owned();
        m.folders.update(cx, |f, cx| {
            f.own = Some(own);
            cx.notify();
        });
        // Как событие синхронизации: связи нет, повтор через 25 с, две правки в очереди.
        m.on_connection(postman_core::link::Connection::Offline { retry_in: 25, queued: 2 }, cx);
        Ok(())
    }

    fn offline_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        m.offline.is_down() && m.list.read(cx).offline.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, Offline, host_of};
    use postman_core::link::Connection;
    use std::time::{Duration, Instant};

    /// Связь пропала → по-прежнему нет (новый срок, очередь растёт) → вернулась: всё снято.
    #[test]
    fn offline_state_transitions() {
        let mut o = Offline::default();
        let now = Instant::now();
        assert_eq!(o.on(Connection::Online, now), Change::None);
        assert_eq!(o.left(now), None);
        assert_eq!(o.on(Connection::Offline { retry_in: 5, queued: 0 }, now), Change::Lost);
        assert!(o.is_down());
        assert_eq!(o.left(now), Some(5));
        assert_eq!(o.left(now + Duration::from_millis(2500)), Some(3));
        // Срок прошёл, ответа нет — «подключение…».
        assert_eq!(o.left(now + Duration::from_secs(6)), Some(0));
        let later = now + Duration::from_secs(6);
        assert_eq!(o.on(Connection::Offline { retry_in: 10, queued: 2 }, later), Change::Still);
        assert_eq!((o.left(later), o.queued()), (Some(10), 2));
        o.retry_now(later + Duration::from_secs(1));
        assert_eq!(o.left(later + Duration::from_secs(1)), Some(0));
        assert_eq!(o.on(Connection::Online, later), Change::Back);
        assert_eq!((o.is_down(), o.left(later), o.queued()), (false, None, 0));
        assert_eq!(o.on(Connection::Online, later), Change::None);
    }

    /// Отсчёт без связи (S5-14): дальше 10 с — шагом 5 с вверх, последние 10 с — по секунде; таймер — к смене числа;
    /// попытка идёт — таймера нет.
    #[test]
    fn countdown_steps_and_wakes_only_on_change() {
        let mut o = Offline::default();
        let now = Instant::now();
        assert_eq!(o.next_change(now), None, "связь есть");
        o.on(Connection::Offline { retry_in: 60, queued: 0 }, now);
        assert_eq!(o.shown(now), Some(60));
        let t = now + Duration::from_millis(2_700);
        assert_eq!(o.shown(t), Some(60));
        assert_eq!(o.next_change(t), Some(Duration::from_millis(2_300)), "до 55 с");
        assert_eq!(o.shown(now + Duration::from_secs(5)), Some(55));
        let t = now + Duration::from_millis(49_500);
        assert_eq!(o.shown(t), Some(15));
        assert_eq!(o.next_change(t), Some(Duration::from_millis(500)), "до 10 с");
        let t = now + Duration::from_millis(53_250);
        assert_eq!(o.shown(t), Some(7));
        assert_eq!(o.next_change(t), Some(Duration::from_millis(750)));
        // Срок прошёл — «подключение…», будить нечего до нового события связи.
        assert_eq!(o.shown(now + Duration::from_secs(61)), Some(0));
        assert_eq!(o.next_change(now + Duration::from_secs(61)), None);
        // Дальше 10 с — всегда кратно 5, таймер никогда не нулевой.
        for ms in (0..60_000).step_by(37) {
            let t = now + Duration::from_millis(ms);
            let shown = o.shown(t).unwrap_or(0);
            assert!(shown <= 10 || shown % 5 == 0, "{ms}: {shown}");
            assert!(o.next_change(t).is_some_and(|d| d >= Duration::from_millis(20)), "{ms}");
        }
    }

    #[test]
    fn host_is_taken_from_the_account_url() {
        assert_eq!(host_of("https://mail.corp.example/jmap/session"), "mail.corp.example");
        assert_eq!(host_of("http://10.0.0.5:8080"), "10.0.0.5:8080");
        assert_eq!(host_of("mail.corp.example"), "mail.corp.example");
    }

    #[test]
    fn status_texts() {
        let (ru, problems) = pets_i18n::I18n::new(&crate::parts::texts(), "ru");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(ru.fmt("states-offline-retry", &{
            let mut a = pets_i18n::FluentArgs::new();
            a.set("n", 25);
            a
        }), "Нет связи · повтор через 25 с");
    }
}
