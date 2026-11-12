//! Уведомления о новых письмах в окне (T2-10, «Настройки → Общие»): после каждого обновления писем с сервера — новые
//! непрочитанные во «Входящих» (`postman_core::newmail`, в фоне: читает кэш). Найденное — в историю уведомлений
//! (🔔); окно не активно и не тихие часы — ещё и уведомление Windows, со звуком «Новое письмо» или молча. Отметка
//! «что уже видели» — в кэше: после перезапуска старые письма не всплывают; первый запуск — с этой минуты.

use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level;
use postman_core::newmail::{self, Fresh};

use crate::Mailer;

/// Ключ отметки в кэше.
const SINCE: &str = "newmail-since";
/// Писем в тексте уведомления — не больше.
const LINES: usize = 3;
/// Письмо из уведомления ищется среди стольких верхних писем «Входящих».
#[cfg_attr(not(windows), allow(dead_code))]
const LOOK: usize = 200;

/// Состояние проверки: отметка и идёт ли проверка.
#[derive(Default)]
pub(crate) struct NewMail {
    since: Option<i64>,
    running: bool,
    again: bool,
}

/// Заголовок и текст уведомления: одно письмо — отправитель и тема; несколько — сколько и первые строки.
pub(crate) fn text(list: &[Fresh]) -> (String, String) {
    match list {
        [one] => (one.from.clone(), one.subject.clone()),
        _ => {
            let lines: Vec<String> = list.iter().take(LINES).map(|f| format!("{}: {}", f.from, f.subject)).collect();
            let more = list.len().saturating_sub(LINES);
            let mut body = lines.join("\n");
            if more > 0 {
                body.push('\n');
                body.push_str(&tr!("newmail-more", n = more));
            }
            (tr!("newmail-title", n = list.len()), body)
        }
    }
}

impl Mailer {
    /// Письма обновились: проверить новые (одна проверка за раз; пришло ещё — проверить после неё).
    pub(crate) fn check_new_mail(&mut self, cx: &mut Context<Self>) {
        if !self.backend.is_live() {
            return;
        }
        if self.newmail.running {
            self.newmail.again = true;
            return;
        }
        self.newmail.running = true;
        let cache = self.cache.clone();
        let me = self.list.read(cx).me.clone();
        let known = self.newmail.since;
        let task = cx.background_spawn(async move {
            let now = jiff::Timestamp::now().as_second();
            let stored = cache.state(SINCE).ok().flatten().and_then(|s| s.parse::<i64>().ok());
            let Some(since) = known.or(stored) else {
                // Первый запуск: всё, что уже лежит, — не новое.
                let _ = cache.set_state(SINCE, &now.to_string());
                return Ok::<_, postman_cache::Error>((Vec::new(), now));
            };
            let found = newmail::scan(&cache, since, &me)?;
            let next = found.iter().map(|f| f.received_at).max().unwrap_or(since).max(since);
            if next != since {
                cache.set_state(SINCE, &next.to_string())?;
            }
            Ok((found, next))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |m, cx| {
                m.newmail.running = false;
                match result {
                    Ok((found, since)) => {
                        m.newmail.since = Some(since);
                        m.show_new_mail(found, cx);
                    }
                    Err(e) => tracing::warn!("newmail: new mail check failed, retry later: {e}"),
                }
                if std::mem::take(&mut m.newmail.again) {
                    m.check_new_mail(cx);
                }
            });
        })
        .detach();
    }

    /// Щелчок по уведомлению Windows (T3-13): «Почта», «Входящие» и письмо `id`, если оно ещё там.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn open_new_mail(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let inboxes: Vec<String> = cache.mailboxes()?.into_iter().filter(|m| m.role.as_deref() == Some("inbox")).map(|m| m.id).collect();
            let mut found = None;
            if let Some(id) = &id {
                for inbox in &inboxes {
                    if let Some(row) = cache.page(inbox, 0, LOOK)?.into_iter().find(|r| r.id == *id) {
                        found = Some((inbox.clone(), row));
                        break;
                    }
                }
            }
            Ok::<_, postman_cache::Error>((inboxes.into_iter().next(), found))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |m, cx| match result {
                Ok((first, found)) => {
                    m.set_section(crate::Section::Mail, cx);
                    match found {
                        Some((inbox, row)) => {
                            m.open_mailbox(inbox, cx);
                            m.open_letter(row, cx);
                        }
                        None => {
                            if let Some(inbox) = first {
                                m.open_mailbox(inbox, cx);
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("newmail: new letter not opened: {e}"),
            });
        })
        .detach();
    }

    fn show_new_mail(&mut self, found: Vec<Fresh>, cx: &mut Context<Self>) {
        let g = crate::prefs::get(cx).general;
        let list = newmail::shown(found, g.notify);
        if list.is_empty() {
            return;
        }
        let (title, body) = text(&list);
        self.toasts.update(cx, |t, cx| t.note(Level::Info, format!("{title}\n{body}"), cx));
        let hour = u8::try_from(jiff::Zoned::now().hour()).unwrap_or(0);
        let quiet = g.quiet && newmail::quiet(hour, g.quiet_from, g.quiet_to);
        let active = self.window.update(cx, |_, window, _| crate::screens::window_active(window)).unwrap_or(true);
        if active || quiet {
            return;
        }
        let sound = g.sound;
        let letter = crate::tray::target(&list);
        cx.background_spawn(async move {
            if let Err(e) = crate::screens::notify_new_mail(&title, &body, sound, letter) {
                tracing::error!("newmail: new mail notification not shown: {e}");
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{Fresh, text};

    fn f(n: usize) -> Fresh {
        Fresh { id: n.to_string(), from: format!("От {n}"), subject: format!("Тема {n}"), received_at: n as i64, important: false }
    }

    /// Одно письмо — отправитель и тема; пять — счётчик и три строки с «ещё 2».
    #[test]
    fn notification_text() {
        assert_eq!(text(&[f(1)]), ("От 1".to_owned(), "Тема 1".to_owned()));
        let (_, body) = text(&(1..=5).map(f).collect::<Vec<_>>());
        assert_eq!(body.lines().count(), 4);
        assert!(body.starts_with("От 1: Тема 1"));
    }
}
