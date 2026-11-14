//! Вход (ADR 0012 «Проверка договора», вход `Flow`): `Phone` → `auth.sendCode` → `Code` → `auth.signIn` →
//! `Password { hint }` (SRP двухэтапной проверки) → `Done`; на вопрос `Phone` ответ `Confirm(true)` — вход с другого
//! устройства: `Qr { url: tg://login?token=… }`, дальше `Confirm(true)` — ждать (новый `Qr`, когда срок прошёл),
//! `Confirm(false)` — назад к телефону. Переезд ДЦ (`PHONE_MIGRATE_N`, `loginTokenMigrateTo`) — внутри.
//!
//! Неверный ввод — `ChannelError::Denied` с кодом ([`crate::BAD_PHONE`], [`crate::BAD_CODE`], …), шаг остаётся тем
//! же: интерфейс показывает причину и спрашивает снова. Телефон, код и пароль нигде не сохраняются и не пишутся в журнал.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use grammers_crypto::two_factor_auth::calculate_2fa;
use grammers_tl_types as tl;
use postman_core::backend::BoxFuture;
use postman_core::channel::{AccountId, Answer, ChannelError, CodeVia, Flow, Prompt};

use crate::convert::user_name;
use crate::wire::{Dial, Wire, WireError, call, call_in};
use crate::{BAD_CODE, BAD_PASSWORD, BAD_PHONE, CODE_EXPIRED, NO_API_KEY, SIGN_UP, Shared};

enum Step {
    Phone,
    Code { phone: String, hash: String },
    Password,
    Qr { token: Vec<u8>, expires: i64 },
    Done,
}

pub(crate) struct SignIn {
    dial: Arc<dyn Dial>,
    shared: Arc<Shared>,
    api: (i32, String),
    wire: Option<Rc<dyn Wire>>,
    step: Step,
}

impl SignIn {
    pub(crate) fn new(dial: Arc<dyn Dial>, shared: Arc<Shared>) -> Result<Self, ChannelError> {
        let api = dial.api().ok_or_else(|| ChannelError::Denied(NO_API_KEY.into()))?;
        Ok(Self { dial, shared, api, wire: None, step: Step::Phone })
    }

    async fn wire(&mut self) -> Result<Rc<dyn Wire>, ChannelError> {
        if let Some(w) = &self.wire {
            return Ok(Rc::clone(w));
        }
        let w = self.dial.dial(None).await.map_err(WireError::channel)?;
        self.wire = Some(Rc::clone(&w));
        Ok(w)
    }

    async fn go(&mut self, answer: Option<Answer>) -> Result<Prompt, ChannelError> {
        let Some(answer) = answer else {
            self.step = Step::Phone;
            return Ok(Prompt::Phone);
        };
        match (&self.step, answer) {
            (Step::Phone, Answer::Text(phone)) => self.send_code(&phone).await,
            (Step::Phone, Answer::Confirm(true)) => self.qr().await,
            (Step::Code { phone, hash }, Answer::Text(code)) => {
                let (phone, hash) = (phone.clone(), hash.clone());
                self.sign_in(&phone, &hash, code.trim()).await
            }
            (Step::Code { phone, hash }, Answer::Resend) => {
                let (phone, hash) = (phone.clone(), hash.clone());
                let w = self.wire().await?;
                let req = tl::functions::auth::ResendCode { phone_number: phone.clone(), phone_code_hash: hash, reason: None };
                let sent = call(&*w, &req).await.map_err(code_error)?;
                self.after_code(phone, sent).await
            }
            (Step::Password, Answer::Secret(s)) => {
                let pw = String::from_utf8_lossy(s.expose()).into_owned();
                self.password(pw.as_bytes()).await
            }
            (Step::Password, Answer::Text(pw)) => self.password(pw.as_bytes()).await,
            (Step::Qr { .. }, Answer::Confirm(true)) => self.qr_wait().await,
            (Step::Qr { .. }, Answer::Confirm(false)) => {
                self.step = Step::Phone;
                Ok(Prompt::Phone)
            }
            (Step::Done, _) => Ok(Prompt::Finished),
            _ => Err(ChannelError::Protocol("unexpected answer".into())),
        }
    }

    /// Вызов с переездом домашнего ДЦ (`*_MIGRATE_N`, код 303).
    async fn migrating<R: tl::RemoteCall>(&mut self, req: &R) -> Result<R::Return, WireError> {
        let w = self.wire().await.map_err(|e| WireError::Network(e.to_string()))?;
        match call(&*w, req).await {
            Err(WireError::Rpc { code: 303, value: Some(dc), .. }) => {
                let dc = i32::try_from(dc).unwrap_or(2);
                tracing::info!(dc, "telegram: sign-in moves to another data center");
                w.set_home_dc(dc).await;
                call(&*w, req).await
            }
            other => other,
        }
    }

    async fn send_code(&mut self, phone: &str) -> Result<Prompt, ChannelError> {
        let phone = normalize_phone(phone).ok_or_else(|| ChannelError::Denied(BAD_PHONE.into()))?;
        let req = tl::functions::auth::SendCode {
            phone_number: phone.clone(),
            api_id: self.api.0,
            api_hash: self.api.1.clone(),
            settings: tl::types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                unknown_number: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
            }
            .into(),
        };
        let sent = self.migrating(&req).await.map_err(phone_error)?;
        tracing::info!("telegram: sign-in code requested");
        self.after_code(phone, sent).await
    }

    async fn after_code(&mut self, phone: String, sent: tl::enums::auth::SentCode) -> Result<Prompt, ChannelError> {
        match sent {
            tl::enums::auth::SentCode::Code(c) => {
                let (via, length) = code_via(&c.r#type);
                self.step = Step::Code { phone, hash: c.phone_code_hash };
                Ok(Prompt::Code { via, length })
            }
            tl::enums::auth::SentCode::Success(s) => self.finish(s.authorization).await,
            tl::enums::auth::SentCode::PaymentRequired(_) => Err(ChannelError::Denied("payment-required".into())),
        }
    }

    async fn sign_in(&mut self, phone: &str, hash: &str, code: &str) -> Result<Prompt, ChannelError> {
        let w = self.wire().await?;
        let req = tl::functions::auth::SignIn {
            phone_number: phone.to_owned(),
            phone_code_hash: hash.to_owned(),
            phone_code: Some(code.to_owned()),
            email_verification: None,
        };
        match call(&*w, &req).await {
            Ok(a) => self.finish(a).await,
            Err(e) if e.is("SESSION_PASSWORD_NEEDED") => self.password_prompt().await,
            Err(e) => Err(code_error(e)),
        }
    }

    async fn password_prompt(&mut self) -> Result<Prompt, ChannelError> {
        let w = self.wire().await?;
        let tl::enums::account::Password::Password(p) = call(&*w, &tl::functions::account::GetPassword {}).await?;
        self.step = Step::Password;
        Ok(Prompt::Password { hint: p.hint.filter(|h| !h.is_empty()) })
    }

    async fn password(&mut self, password: &[u8]) -> Result<Prompt, ChannelError> {
        let w = self.wire().await?;
        let tl::enums::account::Password::Password(p) = call(&*w, &tl::functions::account::GetPassword {}).await?;
        let Some(tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(algo)) = &p.current_algo else {
            return Err(ChannelError::Protocol("unknown password algorithm".into()));
        };
        let (Some(srp_b), Some(srp_id)) = (p.srp_b.clone(), p.srp_id) else { return Err(ChannelError::Protocol("no srp parameters".into())) };
        if !self.dial.srp_ok(&algo.p, algo.g) {
            return Err(ChannelError::Protocol("unsafe srp parameters".into()));
        }
        let (m1, g_a) = calculate_2fa(&algo.salt1, &algo.salt2, &algo.p, &algo.g, srp_b, p.secure_random.clone(), password);
        let req =
            tl::functions::auth::CheckPassword { password: tl::types::InputCheckPasswordSrp { srp_id, a: g_a.to_vec(), m1: m1.to_vec() }.into() };
        match call(&*w, &req).await {
            Ok(a) => self.finish(a).await,
            Err(e) if e.is("PASSWORD_HASH_INVALID") => Err(ChannelError::Denied(BAD_PASSWORD.into())),
            Err(e) => Err(e.channel()),
        }
    }

    async fn qr(&mut self) -> Result<Prompt, ChannelError> {
        let req = tl::functions::auth::ExportLoginToken { api_id: self.api.0, api_hash: self.api.1.clone(), except_ids: Vec::new() };
        let token = self.migrating(&req).await;
        self.on_token(token).await
    }

    async fn on_token(&mut self, token: Result<tl::enums::auth::LoginToken, WireError>) -> Result<Prompt, ChannelError> {
        match token {
            Ok(tl::enums::auth::LoginToken::Token(t)) => {
                let expires = i64::from(t.expires) * 1000;
                let url = format!("tg://login?token={}", base64url(&t.token));
                self.step = Step::Qr { token: t.token, expires };
                Ok(Prompt::Qr { url, expires })
            }
            Ok(tl::enums::auth::LoginToken::MigrateTo(m)) => {
                let w = self.wire().await?;
                w.set_home_dc(m.dc_id).await;
                let imported = call_in(&*w, Some(m.dc_id), &tl::functions::auth::ImportLoginToken { token: m.token }).await;
                Box::pin(self.on_token(imported)).await
            }
            Ok(tl::enums::auth::LoginToken::Success(s)) => self.finish(s.authorization).await,
            Err(e) if e.is("SESSION_PASSWORD_NEEDED") => self.password_prompt().await,
            Err(e) => Err(e.channel()),
        }
    }

    /// Ждать, пока QR отсканируют, или срока; затем спросить сервер снова.
    async fn qr_wait(&mut self) -> Result<Prompt, ChannelError> {
        let Step::Qr { token, expires } = &self.step else { return Err(ChannelError::Protocol("no qr".into())) };
        let (old, expires) = (token.clone(), *expires);
        let left = u64::try_from(expires - now_ms()).unwrap_or(0).clamp(1_000, 30_000);
        let w = self.wire().await?;
        let scanned = w.login_token(Duration::from_millis(left)).await;
        let req = tl::functions::auth::ExportLoginToken { api_id: self.api.0, api_hash: self.api.1.clone(), except_ids: Vec::new() };
        let token = call(&*w, &req).await;
        if let Ok(tl::enums::auth::LoginToken::Token(t)) = &token
            && t.token == old
            && !scanned
            && i64::from(t.expires) * 1000 > now_ms()
        {
            return Ok(Prompt::Qr { url: format!("tg://login?token={}", base64url(&t.token)), expires });
        }
        self.on_token(token).await
    }

    async fn finish(&mut self, a: tl::enums::auth::Authorization) -> Result<Prompt, ChannelError> {
        let tl::enums::auth::Authorization::Authorization(a) = a else { return Err(ChannelError::Denied(SIGN_UP.into())) };
        let tl::enums::User::User(u) = &a.user else { return Err(ChannelError::Protocol("empty user".into())) };
        let w = self.wire().await?;
        let account = AccountId(u.id.to_string());
        self.shared.add_account(&account);
        let secret = w.blob(u.id).to_secret();
        self.step = Step::Done;
        tracing::info!("telegram: signed in");
        Ok(Prompt::Done { account, name: user_name(u), secret })
    }
}

impl Drop for SignIn {
    fn drop(&mut self) {
        if let Some(w) = &self.wire {
            w.close();
        }
    }
}

impl Flow for SignIn {
    fn step<'a>(&'a mut self, answer: Option<Answer>) -> BoxFuture<'a, Result<Prompt, ChannelError>> {
        Box::pin(self.go(answer))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// Номер — цифры с `+` в начале (пробелы, скобки, дефисы убираются); меньше 5 цифр — не номер.
pub fn normalize_phone(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let bad = s.chars().any(|c| !(c.is_ascii_digit() || " +-().".contains(c)));
    (!bad && (5..=15).contains(&digits.len())).then(|| format!("+{digits}"))
}

fn code_via(t: &tl::enums::auth::SentCodeType) -> (CodeVia, Option<u8>) {
    use tl::enums::auth::SentCodeType as T;
    let len = |n: i32| u8::try_from(n).ok();
    match t {
        T::App(c) => (CodeVia::App, len(c.length)),
        T::Sms(c) => (CodeVia::Sms, len(c.length)),
        T::Call(c) => (CodeVia::Call, len(c.length)),
        T::MissedCall(c) => (CodeVia::Call, len(c.length)),
        T::FlashCall(_) => (CodeVia::Call, None),
        T::EmailCode(c) => (CodeVia::Email, len(c.length)),
        T::SetUpEmailRequired(_) => (CodeVia::Email, None),
        T::FragmentSms(c) => (CodeVia::Sms, len(c.length)),
        T::FirebaseSms(c) => (CodeVia::Sms, len(c.length)),
        T::SmsWord(_) | T::SmsPhrase(_) => (CodeVia::Sms, None),
    }
}

fn phone_error(e: WireError) -> ChannelError {
    if e.is("PHONE_NUMBER_*") { ChannelError::Denied(BAD_PHONE.into()) } else { e.channel() }
}

fn code_error(e: WireError) -> ChannelError {
    if e.is("PHONE_CODE_EXPIRED") {
        ChannelError::Denied(CODE_EXPIRED.into())
    } else if e.is("PHONE_CODE_*") {
        ChannelError::Denied(BAD_CODE.into())
    } else {
        e.channel()
    }
}

/// base64url без `=` (RFC 4648 §5) — как ждут официальные клиенты в `tg://login?token=`.
pub fn base64url(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        let take = chunk.len() + 1;
        for i in 0..take {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_and_tokens() {
        assert_eq!(normalize_phone("+999 (12) 345-67-89").as_deref(), Some("+9991234567 89".replace(' ', "").as_str()));
        assert_eq!(normalize_phone("12"), None);
        assert_eq!(normalize_phone("+999 12a"), None);
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }
}
