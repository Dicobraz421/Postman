//! Шифрованное письмо S/MIME в области чтения (S5-12 Ф2, `docs/SMIME.md` §6.2, §7): «Зашифровано» в шапке, строка
//! с действием вместо тела при ошибке, вложения расшифрованного письма — только в памяти.
//!
//! Ход: байты письма → воркер (получатели и шифр) → ядро снимает ключ (CNG; демо — ключ в памяти) → воркер
//! (расшифровка, тело, вложения, подпись внутри). Ключ содержимого — на сеанс в памяти (`keys::session`), текст —
//! только в памяти области чтения; на диск — ничего (решение владельца №4). Поток интерфейса не ждёт.

use std::sync::{Arc, OnceLock, PoisonError};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_cache::{AttachmentRow, EmailRow};
use postman_core::smime::keys::{self, KeyError, KeyStore, Prompt, SoftKeys, WindowsKeys};
use postman_smime::envelope::{Cipher, OpenError};
use postman_worker::SmimeFailure;

use crate::chrome::icon;
use crate::reading::ReadingPane;
use crate::roles::{Look, Pm};
use crate::smime::Seal;

/// Ключ вложения расшифрованного письма: `smime-part:<id письма>:<номер>` (не блоб сервера).
pub(crate) const PART: &str = "smime-part:";

/// Значок замка (Segoe Fluent Icons).
pub(crate) const LOCK: char = '\u{E72E}';

/// Расшифровка открытого письма.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lock {
    Opening,
    Open(Cipher),
    Failed(Problem),
}

/// Почему не открылось — строка вместо тела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Problem {
    NeedPin,
    NoCard,
    Cancelled,
    /// Письмо ГОСТ, а сертифицированного СКЗИ нет (решение владельца 2026-10-22).
    NeedSkzi,
    NotForYou(Vec<String>),
    Damaged,
    Unsupported(String),
    Nested,
    TooLarge,
    Failed(String),
}

impl Problem {
    fn of_key(e: KeyError) -> Problem {
        match e {
            KeyError::NeedPin => Problem::NeedPin,
            KeyError::NoCard => Problem::NoCard,
            KeyError::Cancelled => Problem::Cancelled,
            KeyError::NeedSkzi => Problem::NeedSkzi,
            KeyError::NotForYou(l) => Problem::NotForYou(l),
            KeyError::Unsupported(a) => Problem::Unsupported(a),
            KeyError::Failed(e) => Problem::Failed(e),
        }
    }

    fn of_open(e: OpenError) -> Problem {
        match e {
            OpenError::Damaged => Problem::Damaged,
            OpenError::Unsupported(a) => Problem::Unsupported(a),
            OpenError::Nested => Problem::Nested,
            OpenError::TooLarge => Problem::TooLarge,
            OpenError::NotEncrypted | OpenError::Malformed(_) => Problem::Damaged,
        }
    }

    fn of_worker(e: SmimeFailure) -> Problem {
        match e {
            SmimeFailure::TooLarge => Problem::TooLarge,
            SmimeFailure::Worker(e) => Problem::Failed(e),
        }
    }

    /// Текст строки.
    pub(crate) fn text(&self) -> String {
        match self {
            Problem::NeedPin => tr!("smime-enc-need-pin"),
            Problem::NoCard => tr!("smime-enc-no-card"),
            Problem::Cancelled => tr!("smime-enc-cancelled"),
            Problem::NeedSkzi => tr!("smime-enc-need-skzi"),
            Problem::NotForYou(who) => tr!("smime-enc-not-for-you", who = who.first().cloned().unwrap_or_default()),
            Problem::Damaged => tr!("smime-enc-damaged"),
            Problem::Unsupported(alg) => tr!("smime-enc-unsupported", alg = alg.clone()),
            Problem::Nested => tr!("smime-enc-nested"),
            Problem::TooLarge => tr!("smime-enc-too-large"),
            Problem::Failed(e) => tr!("smime-enc-failed", error = e.clone()),
        }
    }

    /// Действие строки: «Ввести» (окно PIN ОС) или «Повторить».
    fn action(&self) -> Option<(&'static str, Prompt)> {
        match self {
            Problem::NeedPin | Problem::Cancelled => Some(("smime-enc-enter", Prompt::Window(0))),
            Problem::NoCard | Problem::Failed(_) => Some(("smime-enc-retry", Prompt::Silent)),
            _ => None,
        }
    }

    /// Красная строка — только безопасность (DESIGN «Цвет»): письмо изменено.
    fn danger(&self) -> bool {
        matches!(self, Problem::Damaged)
    }
}

/// Письмо зашифровано (метка строки, ставит синхронизация по `Content-Type`).
pub(crate) fn is_encrypted(row: &EmailRow) -> bool {
    row.has_keyword(postman_cache::ENCRYPTED)
}

/// Ключи демо-ящика: владелец демо-ящика (тестовый ключ, `postman_demo::smime::my_key`).
fn demo_keys() -> Arc<dyn KeyStore> {
    static KEYS: OnceLock<Arc<SoftKeys>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let (cert, key) = postman_demo::smime::my_key();
        Arc::new(SoftKeys(postman_smime::soft::SoftKey::new(cert, key).into_iter().collect()))
    })
    .clone()
}

/// Вложения расшифрованного письма — строками ленты (ключ — `smime-part:<id>:<номер>`).
fn part_rows(key: &str, parts: &[postman_smime::envelope::Part]) -> Vec<AttachmentRow> {
    parts
        .iter()
        .map(|p| AttachmentRow {
            blob_id: format!("{PART}{key}:{}", p.index),
            name: p.name.clone(),
            mime: p.mime.clone(),
            size: p.size,
            cid: p.cid.clone(),
            inline: p.inline,
        })
        .collect()
}

/// Итог фоновой расшифровки.
struct Done {
    /// ГОСТ: внутреннее письмо от СКЗИ (для вложений).
    inner: Option<Arc<zeroize::Zeroizing<Vec<u8>>>>,
    cipher: Cipher,
    doc: Option<postman_ir::Document>,
    parts: Vec<AttachmentRow>,
    verdict: Option<postman_core::smime::Verdict>,
}

impl ReadingPane {
    fn key_store(&self) -> Arc<dyn KeyStore> {
        if self.live { Arc::new(WindowsKeys) } else { demo_keys() }
    }

    /// Байты письма пришли — расшифровать (`prompt` — молча при открытии, с окном PIN по «Ввести»).
    pub(crate) fn smime_decrypt(&mut self, data: Arc<Vec<u8>>, prompt: Prompt, cx: &mut Context<Self>) {
        let (env, store, worker, cache) = (self.smime_context(), self.key_store(), self.worker.clone(), self.cache.clone());
        let Some(r) = self.reading.as_mut() else { return };
        let Some(s) = r.smime.as_mut() else { return };
        s.lock = Some(Lock::Opening);
        s.data = Some(data.clone());
        let id = r.row.id.clone();
        let key = id.clone();
        let from = r.row.from_addr.to_lowercase();
        let gost_inner = s.inner.clone();
        cx.notify();
        let task = cx.background_spawn(async move {
            let now = jiff::Timestamp::now().as_second();
            let trust = env.anchors.trust();
            // ГОСТ: СКЗИ отдаёт внутреннее письмо (ключ из него не выходит), воркер его разбирает.
            let gost = match gost_inner {
                Some(i) => Some(i),
                None if keys::remembered(&key).is_none() => {
                    let envelope = {
                        let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                        w.smime_envelope(&data)
                    };
                    let envelope = envelope.map_err(Problem::of_worker)?.map_err(Problem::of_open)?;
                    if envelope.cipher == Ok(Cipher::Gost28147) {
                        Some(Arc::new(store.gost(&envelope, prompt).map_err(Problem::of_key)?))
                    } else {
                        let cek = store.cek(&envelope, prompt).map_err(Problem::of_key)?;
                        keys::remember(&key, cek);
                        None
                    }
                }
                None => None,
            };
            if let Some(inner) = gost {
                let r = {
                    let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                    w.smime_inner(&inner, &from, &trust, u64::try_from(now).unwrap_or(0), None)
                };
                let (opened, doc, _) = r.map_err(Problem::of_worker)?.map_err(Problem::of_open)?;
                let opened = opened.ok_or_else(|| Problem::Failed("worker".into()))?;
                let parts = part_rows(&key, &opened.parts);
                let signed = opened.inner.kind != postman_smime::Kind::None;
                let verdict = signed.then(|| postman_core::smime::assess(&cache, opened.inner, &env, now));
                return Ok::<Done, Problem>(Done { inner: Some(inner), cipher: opened.cipher, doc, parts, verdict });
            }
            let cek = match keys::remembered(&key) {
                Some(c) => c,
                None => {
                    let envelope = {
                        let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                        w.smime_envelope(&data)
                    };
                    let envelope = envelope.map_err(Problem::of_worker)?.map_err(Problem::of_open)?;
                    store.cek(&envelope, prompt).map_err(Problem::of_key)?
                }
            };
            let opened = {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.smime_open(&data, &cek, &trust, u64::try_from(now).unwrap_or(0))
            };
            let (opened, doc) = opened.map_err(Problem::of_worker)?.map_err(Problem::of_open)?;
            keys::remember(&key, cek);
            let parts = part_rows(&key, &opened.parts);
            let signed = opened.inner.kind != postman_smime::Kind::None;
            let verdict = signed.then(|| postman_core::smime::assess(&cache, opened.inner, &env, now));
            Ok::<Done, Problem>(Done { inner: None, cipher: opened.cipher, doc, parts, verdict })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return };
                let Some(s) = r.smime.as_mut() else { return };
                match result {
                    Ok(d) => {
                        s.lock = Some(Lock::Open(d.cipher));
                        s.inner = d.inner;
                        s.seal = match d.verdict {
                            Some(v) => Seal::Done(Box::new(v)),
                            None => Seal::Unsigned,
                        };
                        if let Some(doc) = d.doc {
                            r.doc = Some(doc);
                        }
                        if let Some(b) = r.body.as_mut() {
                            b.attachments = d.parts;
                        }
                        this.ensure_images_pub(cx);
                    }
                    Err(p) => {
                        tracing::info!("S/MIME message is not opened: {p:?}");
                        s.lock = Some(Lock::Failed(p));
                        s.seal = Seal::Unsigned;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// «Ввести» / «Повторить» в строке ошибки.
    fn retry_decrypt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        let data = self.reading.as_ref().and_then(|r| r.smime.as_ref()).and_then(|s| s.data.clone());
        let Some(data) = data else { return };
        let prompt = match prompt {
            Prompt::Window(_) => Prompt::Window(crate::sys::window_id(window).unwrap_or(0)),
            p => p,
        };
        self.smime_decrypt(data, prompt, cx);
    }

    /// Вложение расшифрованного письма: байты — из воркера по ключу сеанса (не с сервера, не с диска). `false` —
    /// не наше вложение.
    pub(crate) fn fetch_part(&mut self, a: &AttachmentRow, cx: &mut Context<Self>) -> bool {
        let Some(rest) = a.blob_id.strip_prefix(PART) else { return false };
        let Some((id, index)) = rest.rsplit_once(':').and_then(|(id, i)| Some((id.to_owned(), i.parse::<u32>().ok()?))) else { return true };
        let state = self.reading.as_ref().filter(|r| r.row.id == id).and_then(|r| r.smime.as_ref());
        // ГОСТ: вложение — из внутреннего письма от СКЗИ.
        if let Some(inner) = state.and_then(|s| s.inner.clone()) {
            if !self.in_flight.insert(a.blob_id.clone()) {
                return true;
            }
            let (worker, blob) = (self.worker.clone(), a.blob_id.clone());
            let task = cx.background_spawn(async move {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.smime_inner(&inner, "", &Default::default(), 0, Some(index)).map(|r| r.map(|(_, _, bytes)| bytes))
            });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                this.update(cx, |this, cx| match r {
                    Ok(Ok(bytes)) => this.blob_ready_pub(&blob, Arc::new(bytes.to_vec()), cx),
                    Ok(Err(e)) => this.blob_failed_pub(&blob, Problem::of_open(e).text(), cx),
                    Err(e) => this.blob_failed_pub(&blob, Problem::of_worker(e).text(), cx),
                })
                .ok();
            })
            .detach();
            return true;
        }
        let data = state.and_then(|s| s.data.clone());
        let (Some(data), Some(cek)) = (data, keys::remembered(&id)) else {
            self.blob_failed_pub(&a.blob_id, tr!("smime-enc-part-gone"), cx);
            return true;
        };
        if !self.in_flight.insert(a.blob_id.clone()) {
            return true;
        }
        let (worker, blob) = (self.worker.clone(), a.blob_id.clone());
        let task = cx.background_spawn(async move {
            let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
            w.smime_part(&data, &cek, index)
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |this, cx| match r {
                Ok(Ok(bytes)) => this.blob_ready_pub(&blob, Arc::new(bytes.to_vec()), cx),
                Ok(Err(e)) => this.blob_failed_pub(&blob, Problem::of_open(e).text(), cx),
                Err(e) => this.blob_failed_pub(&blob, Problem::of_worker(e).text(), cx),
            })
            .ok();
        })
        .detach();
        true
    }

    /// Значок «Зашифровано» (серый) в шапке рядом с подписью.
    pub(crate) fn lock_badge(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.reading.as_ref()?.smime.as_ref()?;
        let text = match s.lock.as_ref()? {
            Lock::Opening => tr!("smime-enc-opening"),
            Lock::Open(_) => tr!("smime-encrypted"),
            Lock::Failed(_) => tr!("smime-encrypted"),
        };
        let el = div()
            .id("smime-lock")
            .flex_none()
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(l.fill_passive())
            .text_size(l.px(Metric::TypeCaption))
            .text_color(l.text2())
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(icon(LOCK, 12.))
            .child(text)
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("smime-details")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_smime(cx)));
        Some(el.into_any_element())
    }

    /// Строка вместо тела: идёт расшифровка или ошибка с действием.
    pub(crate) fn lock_line(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.reading.as_ref()?.smime.as_ref()?;
        let (text, danger, action) = match s.lock.as_ref()? {
            Lock::Opening => (tr!("smime-enc-opening"), false, None),
            Lock::Open(_) => return None,
            Lock::Failed(p) => (p.text(), p.danger(), p.action()),
        };
        let fg = if danger { l.pm(Pm::ThreatFg) } else { l.text() };
        let button = action.map(|(key, prompt)| {
            div()
                .id("smime-lock-action")
                .flex_none()
                .px(px(10.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(l.px(Metric::RadiusControl))
                .text_color(l.accent_text())
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .hover(|d| d.bg(l.hover()))
                .child(tr!(key))
                .on_click(cx.listener(move |this, _, window, cx| this.retry_decrypt(prompt, window, cx)))
        });
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(if danger { l.pm(Pm::ThreatBg) } else { l.hover() })
                .text_color(fg)
                .child(icon(LOCK, 14.))
                .child(div().flex_1().min_w_0().child(text))
                .children(button)
                .into_any_element(),
        )
    }

    /// Строка карточки: шифр письма.
    pub(crate) fn lock_cipher(&self) -> Option<String> {
        match self.reading.as_ref()?.smime.as_ref()?.lock.as_ref()? {
            Lock::Open(c) => Some(c.name()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Problem;

    /// Красным — только повреждённое письмо; «Ввести» — у PIN, «Повторить» — у карты.
    #[test]
    fn problems_map_to_lines() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        assert!(Problem::Damaged.danger());
        assert!(!Problem::NotForYou(vec!["УЦ, № 51".into()]).danger());
        assert_eq!(Problem::NeedPin.action().map(|a| a.0), Some("smime-enc-enter"));
        assert_eq!(Problem::NoCard.action().map(|a| a.0), Some("smime-enc-retry"));
        assert!(Problem::NotForYou(vec!["УЦ, № 51".into()]).text().contains("УЦ, № 51"));
        assert!(Problem::Unsupported("RC2".into()).action().is_none());
    }
}
