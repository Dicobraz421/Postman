//! Подпись S/MIME в области чтения (S5-12 Ф1, `docs/SMIME.md` §7): значок в шапке рядом с датой и карточка
//! подробностей под шапкой (как карточка безопасности вложений), без модальных окон.
//!
//! Цвет — роли: красный (`Pm::Threat*`) — только безопасность (подпись недействительна, адрес не совпал, сертификат
//! отозван или не годится), успешные и «не проверено» — нейтральные серые. Байты письма проверяет воркер, отзыв —
//! ядро (`postman_core::smime`); поток интерфейса не ждёт.

use std::sync::{Arc, OnceLock, PoisonError};

use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::theme::Metric;
use postman_cache::{AttachmentRow, BodyRow};
use postman_core::smime::{Confirm, Context as Env, GostEngine, Revocation, Verdict};
use postman_smime::{Hash, KeyKind, Kind, Scheme, Status};
use postman_worker::SmimeFailure;

use crate::chrome::icon;
use crate::reading::ReadingPane;
use crate::roles::{Look, Pm};

/// Значки Segoe Fluent Icons.
mod glyph {
    pub(super) const CERT: char = '\u{EB95}';
    pub(super) const CHECK: char = '\u{E73E}';
    pub(super) const WARNING: char = '\u{E7BA}';
    pub(super) const SHIELD: char = '\u{EA18}';
    pub(super) const SYNC: char = '\u{E895}';
    pub(super) const CHEVRON_DOWN: char = '\u{E70D}';
    pub(super) const CHEVRON_UP: char = '\u{E70E}';
}

/// Что известно о подписи открытого письма.
pub(crate) enum Seal {
    /// Ждём кэш, байты письма или воркер.
    Checking,
    Done(Box<Verdict>),
    TooLarge,
    Failed(String),
    /// Шифрованное письмо без подписи внутри (или ещё не открытое): значка подписи нет (S5-12 Ф2).
    Unsigned,
}

/// Подпись открытого письма: итог, раскрыта ли карточка, блоб письма целиком, за которым пошли.
pub(crate) struct State {
    pub(crate) seal: Seal,
    pub(crate) open: bool,
    pub(crate) raw: Option<String>,
    /// Шифрованное письмо (Ф2, `smime_enc.rs`): расшифровка и её итог.
    pub(crate) lock: Option<crate::smime_enc::Lock>,
    /// Байты письма целиком (шифрованные) — для вложений и повтора с PIN; только в памяти.
    pub(crate) data: Option<Arc<Vec<u8>>>,
    /// ГОСТ: внутреннее письмо, расшифрованное СКЗИ (ключ из СКЗИ не выходит) — для вложений; только в памяти.
    pub(crate) inner: Option<Arc<zeroize::Zeroizing<Vec<u8>>>>,
}

/// Письмо подписано или зашифровано S/MIME (по вложениям JMAP) — есть что проверять.
pub(crate) fn wants(body: &BodyRow) -> bool {
    body.attachments.iter().any(|a| postman_smime::is_smime_part(&a.mime, &a.name))
}

/// Вложение не показывается в ленте: `smime.p7s` — всегда, `smime.p7m` — когда это подпись внутри и тело уже
/// показано из него.
pub(crate) fn hidden(a: &AttachmentRow, opaque_shown: bool) -> bool {
    let mime = a.mime.to_ascii_lowercase();
    let name = a.name.to_ascii_lowercase();
    let signature = matches!(mime.as_str(), "application/pkcs7-signature" | "application/x-pkcs7-signature") || name.ends_with(".p7s");
    let p7m = matches!(mime.as_str(), "application/pkcs7-mime" | "application/x-pkcs7-mime") || name.ends_with(".p7m");
    signature || (opaque_shown && p7m)
}

/// Окружение демо-ящика (для поиска сертификатов адресатов, Ф4).
pub(crate) fn demo_context() -> Env {
    demo_env()
}

/// Якоря демо-ящика (корни демо-УЦ): без Windows и без СКЗИ.
fn demo_env() -> Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(|| {
        let roots = postman_demo::smime::anchors().into_iter().map(|(der, corporate)| postman_smime::Anchor { der, corporate }).collect();
        Env::fixed(postman_smime::Trust { roots, ..Default::default() })
    })
    .clone()
}

/// Тон значка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Neutral,
    Danger,
}

/// Значок: текст, тон, глиф.
pub(crate) fn badge(seal: &Seal) -> (String, Tone, char) {
    let v = match seal {
        Seal::Checking => return (tr!("smime-checking"), Tone::Neutral, glyph::SYNC),
        Seal::TooLarge => return (tr!("smime-too-large"), Tone::Neutral, glyph::WARNING),
        Seal::Failed(_) => return (tr!("smime-unverified"), Tone::Neutral, glyph::WARNING),
        Seal::Unsigned => return (tr!("smime-encrypted"), Tone::Neutral, crate::smime_enc::LOCK),
        Seal::Done(v) => v,
    };
    let r = &v.report;
    match (&r.status, &v.revocation) {
        (_, Revocation::Revoked) => (tr!("smime-revoked"), Tone::Danger, glyph::SHIELD),
        (Status::Invalid, _) => (tr!("smime-invalid"), Tone::Danger, glyph::SHIELD),
        (Status::Expired | Status::WrongUsage | Status::Distrusted, _) | (_, Revocation::Rejected(_)) => {
            (tr!("smime-cert-bad"), Tone::Danger, glyph::SHIELD)
        }
        (Status::Valid | Status::Untrusted(_), _) if !r.address_ok => (tr!("smime-mismatch"), Tone::Danger, glyph::SHIELD),
        (Status::Untrusted(_), _) => (tr!("smime-untrusted"), Tone::Neutral, glyph::WARNING),
        (Status::Valid, _) if r.weak => (tr!("smime-signed-weak"), Tone::Neutral, glyph::CHECK),
        (Status::Valid, _) => (tr!("smime-signed"), Tone::Neutral, glyph::CHECK),
        _ => (tr!("smime-unverified"), Tone::Neutral, glyph::WARNING),
    }
}

/// Первая строка карточки — итог словами.
fn verdict_text(seal: &Seal) -> String {
    let v = match seal {
        Seal::Checking => return tr!("smime-checking"),
        Seal::TooLarge => return tr!("smime-verdict-too-large"),
        Seal::Failed(e) => return tr!("smime-verdict-failed", error = e.as_str()),
        Seal::Unsigned => return tr!("smime-verdict-encrypted"),
        Seal::Done(v) => v,
    };
    let r = &v.report;
    let email = r.certs.first().and_then(|c| c.emails.first()).cloned().unwrap_or_default();
    if v.revocation == Revocation::Revoked {
        return tr!("smime-verdict-revoked");
    }
    if let Revocation::Rejected(why) = v.revocation {
        return tr!("smime-verdict-rejected", reason = reason(why));
    }
    match &r.status {
        Status::Valid | Status::Untrusted(_) if !r.address_ok => tr!("smime-verdict-mismatch", cert = email, from = r.from.as_str()),
        Status::Valid => tr!("smime-verdict-valid"),
        Status::Untrusted(_) => tr!("smime-verdict-untrusted"),
        Status::Expired => tr!("smime-verdict-expired"),
        Status::WrongUsage => tr!("smime-verdict-usage"),
        Status::Distrusted => tr!("smime-verdict-distrusted"),
        Status::Invalid => tr!("smime-verdict-invalid"),
        Status::NoCertificate => tr!("smime-verdict-no-cert"),
        Status::Unsupported(alg) => tr!("smime-verdict-unsupported", alg = alg.as_str()),
        Status::Malformed(e) => tr!("smime-verdict-malformed", error = e.as_str()),
        Status::NotSigned => tr!("smime-verdict-not-signed"),
    }
}

fn reason(c: Confirm) -> String {
    match c {
        Confirm::Untrusted => tr!("smime-reason-untrusted"),
        Confirm::Distrusted => tr!("smime-reason-distrusted"),
        Confirm::Expired => tr!("smime-reason-expired"),
        Confirm::WrongUsage => tr!("smime-reason-usage"),
        Confirm::BadSignature => tr!("smime-reason-signature"),
        Confirm::Failed => tr!("smime-reason-failed"),
    }
}

fn hash_name(h: Hash) -> String {
    match h {
        Hash::Streebog256 => tr!("smime-hash-streebog", bits = 256),
        Hash::Streebog512 => tr!("smime-hash-streebog", bits = 512),
        other => other.name().to_string(),
    }
}

/// «RSA 2048 · SHA-256», «ГОСТ Р 34.10-2012 (256 бит, TC26-256-B) · Стрибог-256».
pub(crate) fn algorithm(key: Option<&KeyKind>, scheme: Option<Scheme>) -> String {
    let key = match key {
        Some(KeyKind::Rsa { bits }) => tr!("smime-key-rsa", bits = *bits),
        Some(KeyKind::Ec { curve }) => tr!("smime-key-ec", curve = curve.as_str()),
        Some(KeyKind::Gost { bits, params }) => tr!("smime-key-gost", bits = *bits, params = params.as_str()),
        Some(KeyKind::Other(o)) => o.clone(),
        None => String::new(),
    };
    let pss = matches!(scheme, Some(Scheme::RsaPss { .. })).then(|| "PSS".to_string());
    [Some(key), pss, scheme.map(|s| hash_name(s.hash()))].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn date(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%d.%m.%Y").to_string()).unwrap_or_default()
}

fn date_time(ts: i64) -> String {
    jiff::Timestamp::from_second(ts).map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%d.%m.%Y %H:%M").to_string()).unwrap_or_default()
}

/// Отпечаток группами по 4 знака.
fn fingerprint(hex: &str) -> String {
    hex.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

fn revocation_text(v: &Verdict, demo: bool) -> String {
    match &v.revocation {
        Revocation::Good => tr!("smime-rev-good", at = date_time(v.checked_at)),
        Revocation::Revoked => tr!("smime-rev-revoked"),
        Revocation::Unknown => tr!("smime-rev-unknown"),
        Revocation::Rejected(c) => reason(*c),
        Revocation::NotCheckedGost => tr!("smime-rev-gost"),
        Revocation::NotChecked if demo && v.report.trusted() => tr!("smime-rev-demo"),
        Revocation::NotChecked => tr!("smime-rev-not-checked"),
    }
}

impl ReadingPane {
    /// Окружение проверки: живой ящик — Windows, демо — корни демо-УЦ.
    pub(crate) fn smime_context(&self) -> Env {
        self.smime_env()
    }

    fn smime_env(&self) -> Env {
        if self.live { Env::system() } else { demo_env() }
    }

    /// Тело пришло: подписанное письмо — итог из кэша, иначе байты письма целиком и проверка в воркере.
    pub(crate) fn ensure_smime(&mut self, cx: &mut Context<Self>) {
        let env = self.smime_env();
        let cache = self.cache.clone();
        let Some(r) = &mut self.reading else { return };
        if r.smime.is_some() || !r.body.as_ref().is_some_and(wants) {
            return;
        }
        let raw = r.body.as_ref().and_then(|b| b.blob_id.clone());
        // Шифрованное (метка строки, Ф2): сразу «Расшифровка…»; итога подписи в кэше у него нет (не храним).
        let encrypted = crate::smime_enc::is_encrypted(&r.row);
        let lock = encrypted.then_some(crate::smime_enc::Lock::Opening);
        r.smime = Some(State { seal: Seal::Checking, open: false, raw: raw.clone(), lock, data: None, inner: None });
        let id = r.row.id.clone();
        let now = jiff::Timestamp::now().as_second();
        let key = id.clone();
        let task = cx.background_spawn(async move { postman_core::smime::cached(&cache, &key, &env, now) });
        cx.spawn(async move |this, cx| {
            let hit = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return };
                // Подпись внутри: тело есть только в подписанном содержимом — нужен воркер.
                let encrypted = r.smime.as_ref().is_some_and(|s| s.lock.is_some());
                match hit.filter(|v| v.report.kind != Kind::Opaque && !encrypted) {
                    Some(v) => {
                        if let Some(s) = &mut r.smime {
                            s.seal = Seal::Done(Box::new(v));
                        }
                        cx.notify();
                    }
                    None => match raw {
                        Some(blob_id) => {
                            let a = AttachmentRow { blob_id, name: "message.eml".into(), mime: "message/rfc822".into(), ..Default::default() };
                            this.fetch(&a, cx);
                        }
                        None => this.smime_failed(&id, tr!("smime-no-raw"), cx),
                    },
                }
            })
            .ok();
        })
        .detach();
    }

    /// Это блоб письма целиком, который ждёт проверка подписи.
    pub(crate) fn smime_waits(&self, blob_id: &str) -> bool {
        self.reading.as_ref().and_then(|r| r.smime.as_ref()).is_some_and(|s| matches!(s.seal, Seal::Checking) && s.raw.as_deref() == Some(blob_id))
    }

    pub(crate) fn smime_failed(&mut self, id: &str, error: String, cx: &mut Context<Self>) {
        if let Some(s) = self.reading.as_mut().filter(|r| r.row.id == id).and_then(|r| r.smime.as_mut()) {
            s.seal = Seal::Failed(error);
            cx.notify();
        }
    }

    /// Байты письма пришли: проверка в воркере (фон), отзыв и кэш — ядро; поток интерфейса не ждёт.
    pub(crate) fn smime_verify(&mut self, data: Arc<Vec<u8>>, cx: &mut Context<Self>) {
        let Some(r) = &self.reading else { return };
        // Шифрованное — сразу расшифровка (подпись внутри проверит воркер там же).
        if r.smime.as_ref().is_some_and(|s| s.lock.is_some()) {
            return self.smime_decrypt(data, postman_core::smime::keys::Prompt::Silent, cx);
        }
        let again = data.clone();
        let (id, cache, env, worker) = (r.row.id.clone(), self.cache.clone(), self.smime_env(), self.worker.clone());
        let task = cx.background_spawn(async move {
            let now = jiff::Timestamp::now().as_second();
            let trust = env.anchors.trust();
            let checked = {
                let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
                w.smime_verify(&data, &trust, u64::try_from(now).unwrap_or(0))
            };
            checked.map(|(report, doc)| (postman_core::smime::finish(&cache, &id, report, &env, now), doc))
        });
        let id = r.row.id.clone();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return };
                if r.smime.is_none() {
                    return;
                }
                // Зашифровано (метки строки не было — демо, старый кэш): расшифровка.
                if let Ok((v, _)) = &result
                    && v.report.kind == Kind::Encrypted
                {
                    this.smime_decrypt(again, postman_core::smime::keys::Prompt::Silent, cx);
                    return;
                }
                // Подписи нет: значка нет.
                if let Ok((v, _)) = &result
                    && (v.report.status == Status::NotSigned || v.report.kind == Kind::None)
                {
                    r.smime = None;
                    cx.notify();
                    return;
                }
                let Some(s) = r.smime.as_mut() else { return };
                match result {
                    Ok((v, doc)) => {
                        if v.report.kind == Kind::Opaque
                            && let Some(doc) = doc
                        {
                            r.doc = Some(doc);
                        }
                        s.seal = Seal::Done(Box::new(v));
                    }
                    Err(SmimeFailure::TooLarge) => s.seal = Seal::TooLarge,
                    Err(SmimeFailure::Worker(e)) => {
                        tracing::warn!("S/MIME check failed: {e}");
                        s.seal = Seal::Failed(e);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Раскрыть или свернуть карточку подписи (щелчок по значку, команда `mail.signature`).
    pub(crate) fn toggle_smime(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.reading.as_mut().and_then(|r| r.smime.as_mut()) {
            s.open = !s.open;
            cx.notify();
        }
    }

    /// Значок в шапке письма.
    pub(crate) fn smime_badge(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.reading.as_ref()?.smime.as_ref()?;
        // Шифрованное без подписи (или пока не открыто): только «Зашифровано» (`lock_badge`).
        if matches!(s.seal, Seal::Unsigned) || (s.lock.is_some() && matches!(s.seal, Seal::Checking)) {
            return None;
        }
        let (text, tone, g) = badge(&s.seal);
        let danger = tone == Tone::Danger;
        let fg = if danger { l.pm(Pm::ThreatFg) } else { l.text2() };
        let el = div()
            .id("smime-badge")
            .flex_none()
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(l.px(Metric::RadiusControl))
            .bg(if danger { l.pm(Pm::ThreatBg) } else { l.fill_passive() })
            .text_size(l.px(Metric::TypeCaption))
            .text_color(fg)
            .cursor_pointer()
            .hover(|d| d.bg(l.hover()))
            .child(icon(g, 12.))
            .child(text)
            .child(icon(if s.open { glyph::CHEVRON_UP } else { glyph::CHEVRON_DOWN }, 9.))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("smime-details")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_smime(cx)));
        Some(el.into_any_element())
    }

    /// Карточка подробностей под шапкой (раскрыта значком).
    pub(crate) fn smime_card(&self, l: &Look) -> Option<AnyElement> {
        let s = self.reading.as_ref()?.smime.as_ref().filter(|s| s.open)?;
        let (_, tone, g) = badge(&s.seal);
        let danger = tone == Tone::Danger;
        let head = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .size(px(32.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(if danger { l.pm(Pm::ThreatBg) } else { l.fill() })
                    .text_color(if danger { l.pm(Pm::ThreatFg) } else { l.text2() })
                    .child(icon(if danger { g } else { glyph::CERT }, 16.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(tr!("smime-card-title")))
                    .child(
                        div()
                            .text_size(l.px(Metric::TypeCaption))
                            .text_color(if danger { l.pm(Pm::ThreatFg) } else { l.text2() })
                            .child(verdict_text(&s.seal)),
                    ),
            );
        let label = |t: String| div().w(px(150.)).flex_none().text_color(l.text2()).child(t);
        let row = |k: String, v: AnyElement| div().flex().gap(px(12.)).child(label(k)).child(div().flex_1().min_w_0().child(v)).into_any_element();
        let text = |t: String| div().child(t).into_any_element();
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(cipher) = self.lock_cipher() {
            rows.push(row(tr!("smime-row-encryption"), text(cipher)));
        }
        if let Seal::Done(v) = &s.seal {
            let r = &v.report;
            if let Some(c) = r.certs.first() {
                let who = match c.emails.first() {
                    Some(e) => format!("{} · {e}", c.subject),
                    None => c.subject.clone(),
                };
                rows.push(row(tr!("smime-row-signer"), text(who)));
                let issuer = match &c.issuer_organization {
                    Some(o) if *o != c.issuer => format!("{} · {o}", c.issuer),
                    _ => c.issuer.clone(),
                };
                let corporate = r.corporate.then(|| {
                    div()
                        .px(px(6.))
                        .rounded(px(4.))
                        .bg(l.fill())
                        .text_size(l.px(Metric::TypeCaption))
                        .text_color(l.text2())
                        .child(tr!("smime-corporate"))
                });
                rows.push(row(
                    tr!("smime-row-issuer"),
                    div().flex().flex_wrap().items_center().gap(px(6.)).child(issuer).children(corporate).into_any_element(),
                ));
                rows.push(row(tr!("smime-row-valid"), text(tr!("smime-valid-range", from = date(c.not_before), to = date(c.not_after)))));
                rows.push(row(
                    tr!("smime-row-fingerprint"),
                    div().text_size(l.px(Metric::TypeCaption)).font_family("Consolas").child(fingerprint(&c.fingerprint)).into_any_element(),
                ));
            }
            rows.push(row(tr!("smime-row-algorithm"), text(algorithm(r.certs.first().map(|c| &c.key), r.scheme))));
            if let Some(engine) = &v.gost {
                let t = match engine {
                    GostEngine::Postman => tr!("smime-engine-postman"),
                    GostEngine::Skzi(name) => tr!("smime-engine-skzi", name = name.as_str()),
                };
                rows.push(row(tr!("smime-row-engine"), text(t)));
            }
            if let Some(t) = r.signing_time {
                rows.push(row(tr!("smime-row-time"), text(date_time(t))));
            }
            if r.status.signature_ok() {
                rows.push(row(tr!("smime-row-revocation"), text(revocation_text(v, !self.live))));
            }
            if r.weak {
                rows.push(div().text_color(l.text2()).child(tr!("smime-weak-note")).into_any_element());
            }
            if r.trusted() && r.address_ok && !matches!(v.revocation, Revocation::Revoked | Revocation::Rejected(_)) {
                rows.push(div().text_size(l.px(Metric::TypeCaption)).text_color(l.text2()).child(tr!("smime-saved")).into_any_element());
            }
        }
        let card = div()
            .id("smime-card")
            .flex_none()
            .relative()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(18.))
            .py(px(14.))
            .rounded(px(8.))
            .border_1()
            .border_color(l.stroke())
            .bg(l.panel())
            .overflow_hidden()
            .child(head)
            .child(div().flex().flex_col().gap(px(8.)).children(rows))
            .when(danger, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(l.pm(Pm::ThreatDot))));
        Some(card.into_any_element())
    }
}

/// Сцены снимков (`--screens smime…`): итог по виду образца демо.
/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part = crate::part::Part { name: "smime", texts: TEXTS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/smime.ftl")), ("en", include_str!("../i18n/en/smime.ftl"))];

/// Сцены `--screens` S/MIME (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::Mailer;
    use crate::screens::{self, Scene};
    use crate::smime_enc::{Lock, Problem};

    pub(crate) const SCENES: &[Scene] = &[
        // Подписанные письма демо (S5-12 Ф1), карточка подписи раскрыта: подпись RSA верна (корпоративный УЦ), ГОСТ Р
        // 34.10-2012 (проверено Postman, без СКЗИ), текст изменён после подписи, издатель не доверен.
        Scene::new("smime", letter, signed_ready).wait(open_card),
        Scene::new("smime-gost", letter, signed_ready).wait(open_card),
        Scene::new("smime-bad", letter, signed_ready).wait(open_card),
        Scene::new("smime-untrusted", letter, signed_ready).wait(open_card),
        // Шифрованное и подписанное письмо демо (S5-12 Ф2): расшифровано демо-ключом, «Зашифровано» и «Подписано»,
        // карточка раскрыта (шифр и подпись), вложение — из расшифрованного письма.
        Scene::new("encrypted", letter, sealed_ready).wait(open_card),
        // Шифровано не для меня: строка «не для вас» с издателем и номером сертификата.
        Scene::new("encrypted-other", letter, sealed_ready),
        // Тег GCM испорчен: красная строка «повреждено».
        Scene::new("encrypted-bad", letter, sealed_ready),
        // Ключ на смарт-карте: строка «Нужен PIN · Ввести» (состояние подставлено — демо без карты).
        Scene::new("encrypted-pin", letter, sealed_ready).wait(need_pin),
    ];

    /// Образец демо (`postman_demo::smime`) письма сцены.
    fn kind(s: &Scene) -> &'static str {
        match s.name {
            "smime" => "rsa",
            "smime-gost" => "gost",
            "smime-bad" => "bad",
            "smime-untrusted" => "untrusted",
            "encrypted" => "enc",
            "encrypted-bad" => "enc-bad",
            _ => "enc-other",
        }
    }

    fn letter(s: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let kind = kind(s);
        screens::select_demo(m, |b| screens::signed(b, kind), cx)
    }

    fn target<'a>(s: &Scene, m: &'a Mailer, cx: &'a App) -> Option<&'a str> {
        let kind = kind(s);
        screens::demo_id(m.list.read(cx), |b| screens::signed(b, kind))
    }

    fn signed_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        // Подпись проверена воркером, карточка раскрыта, тело разобрано.
        let target = target(s, m, cx);
        screens::reading(m, cx).is_some_and(|r| {
            let done = r.smime.as_ref().is_some_and(|s| s.open && matches!(s.seal, crate::smime::Seal::Done(_)));
            Some(r.row.id.as_str()) == target && r.doc.is_some() && done
        })
    }

    fn sealed_ready(s: &Scene, m: &Mailer, cx: &App) -> bool {
        let target = target(s, m, cx);
        screens::reading(m, cx).is_some_and(|r| {
            let smime = r.smime.as_ref();
            let lock = smime.and_then(|s| s.lock.as_ref());
            let ok = match s.name {
                "encrypted" => matches!(lock, Some(Lock::Open(_))) && smime.is_some_and(|s| s.open && matches!(s.seal, crate::smime::Seal::Done(_))),
                "encrypted-other" => matches!(lock, Some(Lock::Failed(Problem::NotForYou(_)))),
                "encrypted-bad" => matches!(lock, Some(Lock::Failed(Problem::Damaged))),
                _ => matches!(lock, Some(Lock::Failed(Problem::NeedPin))),
            };
            Some(r.row.id.as_str()) == target && ok
        })
    }

    /// Подпись проверена — раскрыть карточку, как щелчок по значку.
    fn open_card(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.reading.update(cx, |r, cx| {
            let closed = r.reading.as_ref().and_then(|r| r.smime.as_ref()).is_some_and(|s| !s.open && matches!(s.seal, crate::smime::Seal::Done(_)));
            if closed {
                r.toggle_smime(cx);
            }
        });
    }

    /// Демо без смарт-карты: «не для вас» подменяется строкой PIN — так её видит владелец карты.
    fn need_pin(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        m.reading.update(cx, |r, cx| {
            let smime = r.reading.as_mut().and_then(|r| r.smime.as_mut());
            if let Some(s) = smime.filter(|s| matches!(s.lock, Some(Lock::Failed(Problem::NotForYou(_))))) {
                s.lock = Some(Lock::Failed(Problem::NeedPin));
                cx.notify();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{Seal, Tone, badge, hidden};
    use postman_cache::AttachmentRow;

    fn att(name: &str, mime: &str) -> AttachmentRow {
        AttachmentRow { blob_id: "b".into(), name: name.into(), mime: mime.into(), ..Default::default() }
    }

    #[test]
    fn signature_parts_are_hidden() {
        assert!(hidden(&att("smime.p7s", "application/pkcs7-signature"), false));
        assert!(hidden(&att("x", "application/x-pkcs7-signature"), false));
        assert!(!hidden(&att("smime.p7m", "application/pkcs7-mime"), false), "шифрованное — вложением, пока не показано");
        assert!(hidden(&att("smime.p7m", "application/pkcs7-mime"), true));
        assert!(!hidden(&att("report.pdf", "application/pdf"), true));
    }

    /// Итоги образцов демо → тон значка: красный — только изменённое письмо; ГОСТ — алгоритм словами.
    #[test]
    fn demo_samples_map_to_badges() {
        let _ = pets_i18n::init(&crate::parts::texts(), "ru");
        use postman_core::smime::{Revocation, Verdict};
        let trust = postman_smime::Trust {
            roots: postman_demo::smime::anchors().into_iter().map(|(der, corporate)| postman_smime::Anchor { der, corporate }).collect(),
            ..Default::default()
        };
        let seal = |kind: &str, revocation: Revocation| {
            let report = postman_smime::verify(postman_demo::smime::raw(kind).unwrap(), &trust, 1_792_600_000);
            Seal::Done(Box::new(Verdict { chain: report.chain.clone(), report, revocation, checked_at: 0, gost: None }))
        };
        assert_eq!(badge(&seal("rsa", Revocation::NotChecked)).1, Tone::Neutral);
        assert_eq!(badge(&seal("rsa", Revocation::Revoked)).1, Tone::Danger, "отозван — красный");
        assert_eq!(badge(&seal("bad", Revocation::NotChecked)).1, Tone::Danger);
        assert_eq!(badge(&seal("untrusted", Revocation::NotChecked)).1, Tone::Neutral);
        let Seal::Done(g) = seal("gost", Revocation::NotCheckedGost) else { unreachable!() };
        let alg = super::algorithm(g.report.certs.first().map(|c| &c.key), g.report.scheme);
        assert!(alg.contains("TC26-256-B"), "{alg}");
    }

    #[test]
    fn checking_and_failure_are_neutral() {
        assert_eq!(badge(&Seal::Checking).1, Tone::Neutral);
        assert_eq!(badge(&Seal::TooLarge).1, Tone::Neutral);
        assert_eq!(badge(&Seal::Failed("x".into())).1, Tone::Neutral);
    }
}
