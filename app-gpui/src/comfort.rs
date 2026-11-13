//! Удобства из мессенджеров (`docs/decisions/2026-11-14-messenger-comforts.md`): «Беседа: заглушить / вернуть звук»
//! (CMF-01, `postman_core::mute`) и «Закрепить / открепить» (CMF-02, `postman_core::pin`). Обе команды — ключевыми
//! словами писем (синхронизируются), без клавиш: в Outlook таких нет, а в мессенджерах общих сочетаний нет. Команды —
//! в палитре и меню письма, тост — с «Отменить». Отмена отправки (CMF-03) — `send_later.rs`.
//!
//! Заглушённая беседа: значок «без звука» в строке, без уведомлений (`newmail.rs`), без подсветки в непрочитанных
//! (`live.rs`, `reload_folders`). Закреплённое — значок и группа «Закреплённые» вверху папки (`list.rs`).

use std::collections::HashSet;

use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::{Level as Toast, ToastAction};
use postman_cache::{EmailRow, Patch};
use postman_core::{mute, pin};

use crate::Mailer;

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "comfort", texts: TEXTS, commands: COMMANDS, handlers, scenes: scenes::SCENES, reset: Some(scenes::reset) };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/comfort.ftl")), ("en", include_str!("../i18n/en/comfort.ftl"))];

pub(crate) mod act {
    gpui_kit::actions!(mail, [Mute, Pin]);
}

const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{MAIL, cmd};
    // Без клавиш: в схеме Outlook таких команд нет, общих сочетаний у мессенджеров тоже (Ctrl+M, Ctrl+P заняты).
    &[cmd("mail.mute", &[], MAIL), cmd("mail.pin", &[], MAIL)]
};

fn handlers(root: Div, cx: &mut Context<Mailer>) -> Div {
    root.on_action(cx.listener(|this, _: &act::Mute, _, cx| this.toggle_mute(cx)))
        .on_action(cx.listener(|this, _: &act::Pin, _, cx| this.toggle_pin(cx)))
}

/// Обратные правки — для «Отменить» в тосте.
fn undo(patches: &[Patch]) -> Vec<Patch> {
    patches
        .iter()
        .map(|p| Patch { id: p.id.clone(), keywords: p.keywords.iter().map(|(k, on)| (k.clone(), !on)).collect(), ..Patch::default() })
        .collect()
}

/// Без писем с ключевыми словами только для чтения (общие ящики, S4-06).
fn writable(rows: Vec<EmailRow>, rights: &std::collections::HashMap<String, postman_cache::Rights>) -> Vec<EmailRow> {
    rows.into_iter().filter(|r| crate::list::may(rights, r, |x| x.keywords)).collect()
}

impl Mailer {
    /// `mail.mute`: беседы выделенных (или открытого) писем — заглушить; все уже заглушены — вернуть звук.
    pub(crate) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let ids = self.targets(cx);
        self.mute_ids(ids, cx);
    }

    /// Заглушить беседы писем `ids` или вернуть им звук (меню письма — над его строкой или выделением).
    pub(crate) fn mute_ids(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return self.toast(Toast::Info, tr!("actions-select-letter"), cx);
        }
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let mut seen = HashSet::new();
            let mut threads = Vec::new();
            for r in cache.rows_by_ids(&ids)? {
                if seen.insert(r.thread_id.clone()) {
                    let t = cache.thread(&r.thread_id)?;
                    threads.push(if t.is_empty() { vec![r] } else { t });
                }
            }
            Ok::<_, postman_cache::Error>(threads)
        });
        cx.spawn(async move |this, cx| {
            let threads = task.await;
            let _ = this.update(cx, |m, cx| match threads {
                Ok(threads) => m.mute_threads(threads, cx),
                Err(e) => tracing::error!("mute: conversation not read from the cache: {e}"),
            });
        })
        .detach();
    }

    fn mute_threads(&mut self, threads: Vec<Vec<EmailRow>>, cx: &mut Context<Self>) {
        let rights = self.list.read(cx).rights.clone();
        let threads: Vec<Vec<EmailRow>> = threads.into_iter().map(|t| writable(t, &rights)).filter(|t| !t.is_empty()).collect();
        if threads.is_empty() {
            return self.toast(Toast::Info, tr!("comfort-read-only"), cx);
        }
        let (on, patches) = mute::toggle(&threads);
        let text = if on { tr!("comfort-muted", n = threads.len()) } else { tr!("comfort-unmuted", n = threads.len()) };
        self.apply_with_undo(patches, text, cx);
    }

    /// `mail.pin`: выделенные (или открытое) письма — закрепить, все уже закреплены — открепить; в виде бесед —
    /// всю беседу в папке. В папке не больше `pin::LIMIT` закреплённых бесед.
    pub(crate) fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let ids = self.targets(cx);
        self.pin_ids(ids, cx);
    }

    /// Закрепить письма `ids` (в виде бесед — их беседы в папке) или открепить.
    pub(crate) fn pin_ids(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return self.toast(Toast::Info, tr!("actions-select-letter"), cx);
        }
        let list = self.list.read(cx);
        let threads = list.outlook && list.outline.is_some();
        let mailbox = list.mailbox.clone().filter(|_| list.search_ids.is_none());
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let mut rows = cache.rows_by_ids(&ids)?;
            let folder = mailbox.or_else(|| rows.first().and_then(|r| r.mailbox_ids.first().cloned())).unwrap_or_default();
            if threads {
                let mut seen: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
                let heads: HashSet<String> = rows.iter().map(|r| r.thread_id.clone()).collect();
                for t in heads {
                    let more = cache.thread(&t)?.into_iter().filter(|r| r.mailbox_ids.contains(&folder));
                    rows.extend(more.filter(|r| seen.insert(r.id.clone())).collect::<Vec<_>>());
                }
            }
            let pinned = pin::pinned_threads(&cache, &folder)?;
            Ok::<_, postman_cache::Error>((rows, pinned))
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |m, cx| match found {
                Ok((rows, pinned)) => m.pin_rows(rows, &pinned, cx),
                Err(e) => tracing::error!("pin: letters not read from the cache: {e}"),
            });
        })
        .detach();
    }

    fn pin_rows(&mut self, rows: Vec<EmailRow>, pinned: &HashSet<String>, cx: &mut Context<Self>) {
        let rights = self.list.read(cx).rights.clone();
        let rows = writable(rows, &rights);
        if rows.is_empty() {
            return self.toast(Toast::Info, tr!("comfort-read-only"), cx);
        }
        match pin::toggle(&rows, pinned) {
            Ok((on, patches)) => {
                let text = if on { tr!("comfort-pinned") } else { tr!("comfort-unpinned") };
                self.apply_with_undo(patches, text, cx);
            }
            Err(pin::Full) => self.toast(Toast::Info, tr!("comfort-pin-full", n = pin::LIMIT), cx),
        }
    }

    /// Правки писем и тост «… · Отменить».
    fn apply_with_undo(&mut self, patches: Vec<Patch>, text: String, cx: &mut Context<Self>) {
        if patches.is_empty() {
            return;
        }
        let back = undo(&patches);
        self.apply_patches(patches, cx);
        let mailer = cx.entity().downgrade();
        let action = ToastAction::new(tr!("bulk-undo"), move |_, cx| {
            let back = back.clone();
            let _ = mailer.update(cx, |m, cx| m.apply_patches(back, cx));
        });
        self.toasts.update(cx, |t, cx| t.push_action(Toast::Success, text, action, cx));
    }
}

/// Сцены `--screens`: закреплённые и заглушённая беседа в демо-«Входящих»; окно «Отменить» с отсчётом.
pub(crate) mod scenes {
    use std::sync::Mutex;

    use gpui_kit::*;
    use postman_cache::Patch;
    use postman_core::filters::Group;

    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // CMF-02, CMF-01: два письма закреплены — группа «Закреплённые» над датами, значок булавки; беседа первого
        // письма заглушена — значок «без звука».
        Scene::new("comfort-pinned", pinned, pinned_ready),
        // CMF-03: письмо ушло с задержкой 30 с — «Письмо отправлено · Отменить · 30 с» внизу экрана почты.
        Scene::new("comfort-undo-send", undo_send, undo_ready),
    ];

    /// Обратные правки сцен: вернуть демо-ящик другим сценам.
    static STAGED: Mutex<Vec<Patch>> = Mutex::new(Vec::new());

    fn stage(back: &[Patch]) {
        STAGED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(back);
    }

    pub(crate) fn reset(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        let back = std::mem::take(&mut *STAGED.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        if !back.is_empty() {
            m.apply_patches(back, cx);
        }
        m.send_later.screen_clear();
    }

    fn pinned(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        let list = m.list.read(cx);
        let (pin, quiet): (Vec<_>, Vec<_>) = list.rows.1.iter().take(6).enumerate().partition(|(i, _)| *i == 2 || *i == 4);
        let pin: Vec<_> = pin.into_iter().map(|(_, r)| r.clone()).collect();
        let quiet = quiet.into_iter().map(|(_, r)| r.clone()).next().ok_or("no rows")?;
        let back: Vec<Patch> = pin
            .iter()
            .map(|r| Patch { id: r.id.clone(), keywords: vec![(postman_core::pin::PINNED.into(), true)], ..Patch::default() })
            .chain([Patch { id: quiet.id.clone(), keywords: vec![(postman_core::mute::MUTED.into(), true)], ..Patch::default() }])
            .collect();
        stage(&super::undo(&back));
        m.apply_patches(back, cx);
        Ok(())
    }

    fn pinned_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        list.layout.group_of(0) == Some(&Group::Pinned)
            && screens::rows_loaded(list)
            && list.rows.1.iter().take(2).all(postman_core::pin::is_pinned)
            && list.rows.1.iter().any(postman_core::mute::is_muted)
    }

    fn undo_send(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.send_later.screen_offer(30);
        cx.notify();
        Ok(())
    }

    fn undo_ready(_: &Scene, m: &Mailer, _: &App) -> bool {
        m.undo_shown()
    }
}
