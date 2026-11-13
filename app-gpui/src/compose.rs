//! Новое письмо и ответ (S3-01, S5-06б, DESIGN «Главное окно», «Новое письмо»): новое письмо — вкладка в заголовке
//! окна, ответ — в области чтения над письмом («Развернуть» переносит его во вкладку с тем же черновиком). Окон и
//! модальных диалогов нет: подтверждения — строкой в самом письме.
//!
//! Модель ([`Model`]) — без gpui: поля, вложения, метки письма, грязное/чистое, автосохранение, закрытие, замечания
//! «Перед отправкой» (`postman_core::checks`); её проверяют тесты. View — [`crate::composer::Composer`]. Сервер —
//! только через шов `Backend::send` с тикетом; итог (`Sent`, `DraftSaved`, `ComposeFailed`) приходит событием
//! синхронизации через `Mailer::on_sync`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;
use pets_i18n::tr;
use postman_compose::{Address, Attachment, Draft, Share, parse_addresses};
use postman_core::checks::{self, Check};
use postman_core::contacts::AddressBook;
use postman_core::labels::{self, Keyword};
use postman_core::marks;
use postman_core::send_later;
use postman_core::sync::{Command, Event, Release, Sender};

use crate::Mailer;
use crate::composer::{Composer, ComposerEvent, Open, Place};
use crate::tabs::TabKind;

/// Автосохранение изменённого черновика — не чаще этого.
pub(crate) const AUTOSAVE: Duration = Duration::from_secs(60);
/// Предупреждение о размере вложений: типичный предел серверов — 25–35 МБ.
const LARGE_TOTAL: u64 = 25 * 1024 * 1024;

// --- модель письма (без gpui) ---

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

impl Field {
    #[cfg(test)]
    pub(crate) fn is_address(self) -> bool {
        matches!(self, Field::To | Field::Cc | Field::Bcc)
    }
}

/// Зачем черновик уходит на сервер.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Purpose {
    /// Отправить с окном «Отменить» (сервер держит письмо задержку настройки и запас, `send_later::undo_release`).
    Send,
    /// «Отправить позже»: сервер выпустит письмо в этот момент (Unix-секунды).
    SendLater(i64),
    Save,
    Autosave,
}

impl Purpose {
    pub(crate) fn sends(self) -> bool {
        matches!(self, Purpose::Send | Purpose::SendLater(_))
    }
}

/// Строка состояния письма; текст — в view (Fluent).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Status {
    None,
    Busy(Purpose),
    Saved {
        at: String,
    },
    BadAddresses(Vec<String>),
    NoRecipients,
    /// Отправка остановлена замечанием «Перед отправкой» (нет сертификата шифрования).
    Blocked,
    /// Сервера нет (демо) или поток синхронизации остановился: текст остаётся в письме.
    NoServer(Purpose),
    Failed(String),
    Attach {
        name: String,
        error: String,
    },
    Large(u64),
}

/// Ответ на строку «Сохранить черновик? Да / Нет / Отмена».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloseAnswer {
    Save,
    Discard,
    Cancel,
}

/// Что делает письмо после ответа.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloseStep {
    Close,
    Save,
    Stay,
}

#[derive(Debug)]
struct Pending {
    ticket: u64,
    snapshot: u64,
    purpose: Purpose,
}

pub(crate) struct Model {
    /// Отправитель, ответ (In-Reply-To, References), вложения, чужие ключевые слова.
    base: Draft,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    /// Метка «Конфиденциально» (`marks::CONFIDENTIAL`; шифрования нет — S5-12).
    pub(crate) confidential: bool,
    /// «Подписать» S/MIME (S5-12 Ф3): по умолчанию — «Подписывать все письма» в «Безопасности».
    pub(crate) sign: bool,
    /// «Ждать ответа · N дней»: общее слово `pm.wait` и слово со сроком (`marks::wait_keyword`, S5-04).
    pub(crate) wait: Option<u8>,
    /// Категория письма — её ключевое слово (`pm.cat.<id>`).
    pub(crate) category: Option<String>,
    /// Подпись, стоящая в тексте (смена подписи заменяет её).
    pub(crate) signature: String,
    /// id сохранённой на сервере версии черновика (её заменит следующее сохранение или отправка).
    server_id: Option<String>,
    /// Слепок содержимого, совпадающий с сохранённой (или исходной) версией.
    saved: u64,
    pending: Option<Pending>,
    /// Слепок, сохранить который не удалось, — не повторяем автосохранение, пока текст тот же.
    autosave_failed: Option<u64>,
    last_save: Instant,
    pub(crate) status: Status,
    /// Показана строка «Сохранить черновик?».
    pub(crate) confirm_close: bool,
    /// Показана строка «Удалить черновик?».
    pub(crate) confirm_delete: bool,
    /// Закрыть письмо, когда сервер подтвердит сохранение.
    close_after_save: bool,
    /// Замечания «Перед отправкой» по тексту и по адресатам; снятые — ключами `Check::key`.
    text_checks: Vec<Check>,
    recipient_checks: Vec<Check>,
    /// «Конфиденциально» (S5-12 Ф4): замечания по сертификатам и получатели для шифрования (ставит окно письма).
    cert_checks: Vec<Check>,
    pub(crate) encrypt_to: Vec<postman_smime::seal::To>,
    dismissed: HashSet<String>,
    /// Личные группы письма (T2-47): чип группы в поле — `"Имя" <group:N>`, N — номер здесь ([`Model::add_group`]).
    groups: Vec<Group>,
    /// «Отмена отправки» (CMF-03): сколько секунд письмо ждёт после «Отправить»; 0 — сразу. Ставит окно письма из
    /// настройки перед отправкой (`Prefs::replies`).
    pub(crate) undo_secs: u8,
    /// Ответ на фрагменты (`fragments.rs`): реплики «цитата → мой ответ» между своим текстом и подписью.
    pub(crate) fragments: crate::fragments::Pairs,
}

/// Личная группа в письме (T2-47): чип «Имя · N»; при отправке и сохранении черновика — адреса участников, адреса
/// у группы нет (получатели видят участников, как в Outlook).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Group {
    pub(crate) name: String,
    pub(crate) members: Vec<Address>,
}

impl Group {
    /// Группа книги подсказок → группа письма: имена участников — из книги (нет — пусто).
    pub(crate) fn of_book(g: &postman_core::contacts::Group, book: &AddressBook) -> Self {
        let members = g.members.iter().map(|e| Address::new(book.get(e).map(|x| x.name.clone()).unwrap_or_default(), e)).collect();
        Group { name: g.name.clone(), members }
    }
}

/// Чип группы в поле адресов: `"Имя" <group:N>`.
const GROUP_REF: &str = "group:";

/// Номер группы письма в готовом адресе поля (`"Имя" <group:N>`); обычный адрес — `None`.
pub(crate) fn group_ref(segment: &str) -> Option<usize> {
    let inner = segment.trim().strip_suffix('>')?.rsplit_once('<')?.1;
    inner.strip_prefix(GROUP_REF)?.parse().ok()
}

/// Адрес для поля ввода: имя с запятыми — в кавычках (как [`join`]).
fn shown(a: &Address) -> String {
    if a.name.contains([',', ';', '"']) { format!("\"{}\" <{}>", a.name.replace('"', ""), a.email) } else { a.display() }
}

/// Адреса для поля ввода; имена с запятыми — в кавычках, чтобы разбор вернул то же. Непустое поле кончается
/// разделителем: все адреса — готовые (чипы), набирать — следующий.
fn join(list: &[Address]) -> String {
    let mut s = list
        .iter()
        .map(|a| if a.name.contains([',', ';', '"']) { format!("\"{}\" <{}>", a.name.replace('"', ""), a.email) } else { a.display() })
        .collect::<Vec<_>>()
        .join("; ");
    if !s.is_empty() {
        s.push_str("; ");
    }
    s
}

/// Подпись нового письма: под пустыми строками для текста, как в Outlook.
fn signature_block(signature: &str) -> String {
    if signature.trim().is_empty() { String::new() } else { format!("\n\n{}", signature.trim_end()) }
}

impl Model {
    pub(crate) fn new(draft: Draft, from: Option<Address>, signature: &str, now: Instant) -> Self {
        let mut base = draft;
        if let Some(from) = from.filter(|_| base.from.email.is_empty()) {
            base.from = from;
        }
        // Метки письма (вернулось после отмены отправки или открыт черновик): свои — в поля, чужие — как были.
        let today = jiff::Zoned::now().date();
        let (mut confidential, mut wait, mut category) = (false, None, None);
        base.keywords.retain(|k| {
            if k.eq_ignore_ascii_case(marks::CONFIDENTIAL) {
                confidential = true;
            } else if let Some(due) = marks::parse_wait(k) {
                let days = today.until(due).ok().map_or(1, |s| s.get_days()).clamp(1, 99);
                wait = u8::try_from(days).ok();
            } else if k.eq_ignore_ascii_case(marks::WAIT) {
                // Общее слово «Ждём ответа» (S5-04) — ставится заново вместе со сроком.
            } else if matches!(labels::classify(k), Keyword::Category(_) | Keyword::Thunderbird(_)) {
                category.get_or_insert_with(|| labels::normalize(k));
            } else {
                return true;
            }
            false
        });
        let body = if base.markdown.is_empty() { signature_block(signature) } else { base.markdown.clone() };
        let fragments = std::mem::take(&mut base.fragments);
        let mut m = Self {
            to: join(&base.to),
            cc: join(&base.cc),
            bcc: join(&base.bcc),
            subject: base.subject.clone(),
            body,
            confidential,
            sign: false,
            wait,
            category,
            signature: signature.trim_end().to_owned(),
            base,
            server_id: None,
            saved: 0,
            pending: None,
            autosave_failed: None,
            last_save: now,
            status: Status::None,
            confirm_close: false,
            confirm_delete: false,
            close_after_save: false,
            text_checks: Vec::new(),
            recipient_checks: Vec::new(),
            cert_checks: Vec::new(),
            encrypt_to: Vec::new(),
            undo_secs: send_later::UNDO_DEFAULT,
            dismissed: HashSet::new(),
            groups: Vec::new(),
            fragments: Default::default(),
        };
        // Нетронутое новое письмо — не «изменения»: закрывается без вопроса. Ответ на фрагмент — уже изменение.
        m.saved = m.snapshot();
        for f in fragments {
            m.add_fragment(f);
        }
        m.refresh_text_checks();
        m
    }

    /// Учётная запись с сервера (Identity): отправитель; подпись — в нетронутое пустое письмо.
    /// `true` — текст изменился, его надо показать в поле.
    pub(crate) fn set_identity(&mut self, from: Address, signature: &str) -> bool {
        self.base.from = from;
        let block = signature_block(signature);
        if self.is_dirty() || !self.body.is_empty() || block.is_empty() {
            return false;
        }
        self.body = block;
        self.signature = signature.trim_end().to_owned();
        self.saved = self.snapshot();
        true
    }

    /// Отправитель («От»).
    pub(crate) fn from(&self) -> &Address {
        &self.base.from
    }

    /// Другой отправитель из списка «От» (свой адрес или общий ящик, S4-06). Текст письма не меняется.
    pub(crate) fn set_from(&mut self, from: Address) {
        self.base.from = from;
    }

    /// Окно ответа на письмо `in_reply_to`, которому ещё не пришли вложения беседы (`Mailer::share_for`).
    pub(crate) fn wants_share(&self, in_reply_to: &str) -> bool {
        self.base.share.is_none() && self.base.in_reply_to.as_deref() == Some(in_reply_to)
    }

    /// Вложения беседы новым участникам (решение владельца 2026-11-13): участники и файлы беседы.
    pub(crate) fn set_share(&mut self, share: Share) {
        self.base.share = Some(share);
    }

    /// Отправлять ли вложения беседы новым участникам этого письма (строка над текстом): переключить.
    pub(crate) fn toggle_share(&mut self) {
        if let Some(s) = &mut self.base.share {
            s.off = !s.off;
        }
    }

    /// Строка над текстом: новые участники (имя или адрес) — адресаты полей, которых нет ни в одном письме беседы, —
    /// сколько файлов беседы им уйдёт и не отключено ли это. Новых нет или файлов нет — `None`.
    pub(crate) fn share_line(&self) -> Option<(Vec<String>, usize, bool)> {
        let share = self.base.share.as_ref().filter(|s| !s.attachments.is_empty())?;
        let mut seen: Vec<String> = Vec::new();
        let mut names = Vec::new();
        for text in [&self.to, &self.cc, &self.bcc] {
            for a in parse_addresses(&self.expanded(text)).0 {
                let email = a.email.to_lowercase();
                if email == self.base.from.email.to_lowercase() || share.knows(&email) || seen.contains(&email) {
                    continue;
                }
                seen.push(email);
                names.push(if a.name.is_empty() { a.email } else { a.name });
            }
        }
        (!names.is_empty()).then_some((names, share.attachments.len(), share.off))
    }

    /// Письмо — ответ или пересылка (есть In-Reply-To): в области чтения тема не показывается.
    pub(crate) fn is_reply(&self) -> bool {
        self.base.in_reply_to.is_some()
    }

    pub(crate) fn field(&self, f: Field) -> &str {
        match f {
            Field::To => &self.to,
            Field::Cc => &self.cc,
            Field::Bcc => &self.bcc,
            Field::Subject => &self.subject,
            Field::Body => &self.body,
        }
    }

    pub(crate) fn set(&mut self, f: Field, value: String) {
        match f {
            Field::To => self.to = value,
            Field::Cc => self.cc = value,
            Field::Bcc => self.bcc = value,
            Field::Subject => self.subject = value,
            Field::Body => self.body = value,
        }
        if f == Field::Body {
            self.refresh_text_checks();
        }
    }

    fn snapshot(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&self.to, &self.cc, &self.bcc, &self.subject, &self.body).hash(&mut h);
        (self.confidential, self.wait, &self.category).hash(&mut h);
        self.fragments.hash(&mut h);
        // Картинки текста (подпись, T3-07) идут за текстом: их ссылки `cid:` уже в нём.
        for a in self.base.attachments.iter().filter(|a| a.inline_cid.is_none()) {
            (&a.name, a.data.len(), &a.blob_id).hash(&mut h);
        }
        h.finish()
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.snapshot() != self.saved
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Нераспознанные адреса поля (в письме — красные чипы).
    #[cfg(test)]
    pub(crate) fn bad(&self, f: Field) -> Vec<String> {
        if f.is_address() { parse_addresses(self.field(f)).1 } else { Vec::new() }
    }

    /// Получатели (Кому, Копия, Скрытая), которые разобрались; группы — участниками (T2-47).
    pub(crate) fn recipients(&self) -> Vec<Address> {
        [&self.to, &self.cc, &self.bcc].into_iter().flat_map(|t| parse_addresses(&self.expanded(t)).0).collect()
    }

    // --- личные группы (T2-47) ---

    /// Группа письма → текст её чипа (`"Имя" <group:N>`); та же группа второй раз — тот же номер.
    pub(crate) fn add_group(&mut self, g: Group) -> String {
        let n = match self.groups.iter().position(|x| *x == g) {
            Some(n) => n,
            None => {
                self.groups.push(g);
                self.groups.len() - 1
            }
        };
        let name = self.groups.get(n).map(|g| g.name.replace(['"', '<', '>'], "")).unwrap_or_default();
        format!("\"{name}\" <{GROUP_REF}{n}>")
    }

    /// Группа готового адреса поля; обычный адрес или чужой номер — `None`.
    pub(crate) fn group(&self, segment: &str) -> Option<&Group> {
        self.groups.get(group_ref(segment)?)
    }

    /// Группа — чипом в поле `f` (письмо из «Контактов»: «Написать» группе). Нетронутое письмо остаётся нетронутым.
    pub(crate) fn add_group_to(&mut self, f: Field, g: Group) {
        let clean = !self.is_dirty();
        let chip = self.add_group(g);
        let (mut closed, open) = segments(self.field(f));
        closed.push(chip);
        self.set(f, compose_field(&closed, &open));
        if clean {
            self.saved = self.snapshot();
        }
    }

    /// «+» у чипа группы (как в Outlook): готовый адрес `i` поля `f` — участниками группы (кого в поле ещё нет).
    /// `false` — там не группа.
    pub(crate) fn expand_group(&mut self, f: Field, i: usize) -> bool {
        let (mut closed, open) = segments(self.field(f));
        let Some(g) = closed.get(i).and_then(|c| self.group(c)).cloned() else { return false };
        let has = |closed: &[String], email: &str| closed.iter().any(|c| parse_addresses(c).0.iter().any(|a| a.email.eq_ignore_ascii_case(email)));
        let mut members = Vec::new();
        for a in &g.members {
            if !has(&closed, &a.email) && !members.iter().any(|m: &Address| m.email.eq_ignore_ascii_case(&a.email)) {
                members.push(a.clone());
            }
        }
        closed.splice(i..=i, members.iter().map(shown));
        self.set(f, compose_field(&closed, &open));
        true
    }

    /// Текст поля адресов с группами участниками (их адресами); повторы адреса — один раз. Без групп — как есть.
    fn expanded(&self, text: &str) -> String {
        let (closed, open) = segments(text);
        if !closed.iter().any(|c| group_ref(c).is_some()) {
            return text.to_owned();
        }
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for c in closed {
            let list: Vec<String> = match self.group(&c) {
                Some(g) => g.members.iter().filter(|a| seen.insert(a.email.to_lowercase())).map(shown).collect(),
                None => match parse_addresses(&c).0.first() {
                    Some(a) if !seen.insert(a.email.to_lowercase()) => Vec::new(),
                    _ => vec![c],
                },
            };
            out.extend(list);
        }
        compose_field(&out, &open)
    }

    /// Ключевые слова письма на сервере: чужие, категория, «Конфиденциально», «Ждать ответа» (срок от `today`).
    pub(crate) fn keywords(&self, today: jiff::civil::Date) -> Vec<String> {
        let mut out = self.base.keywords.clone();
        out.extend(self.category.clone());
        if self.confidential {
            out.push(marks::CONFIDENTIAL.to_owned());
        }
        if let Some(due) = self.wait.and_then(|d| marks::wait_due(today, d)) {
            out.push(marks::WAIT.to_owned());
            out.push(marks::wait_keyword(due));
        }
        out
    }

    /// Черновик из полей; ошибки адресов — до отправки, а не от сервера.
    pub(crate) fn draft(&self, for_send: bool) -> Result<Draft, Status> {
        let mut d = self.base.clone();
        let mut bad = Vec::new();
        for (text, out) in [(&self.to, &mut d.to), (&self.cc, &mut d.cc), (&self.bcc, &mut d.bcc)] {
            // Личные группы (T2-47) — адресами участников: у группы своего адреса нет.
            let (ok, b) = parse_addresses(&self.expanded(text));
            *out = ok;
            bad.extend(b);
        }
        if !bad.is_empty() {
            return Err(Status::BadAddresses(bad));
        }
        if for_send && d.recipients().next().is_none() {
            return Err(Status::NoRecipients);
        }
        d.subject = self.subject.clone();
        d.markdown = self.markdown();
        // Картинка текста, на которую текст больше не ссылается (сменили подпись), — не уходит.
        d.attachments.retain(|a| a.inline_cid.as_ref().is_none_or(|cid| d.markdown.contains(&format!("cid:{cid}"))));
        d.keywords = self.keywords(jiff::Zoned::now().date());
        d.sign = for_send && self.sign;
        // «Конфиденциально» = шифрование (S5-12 Ф4); черновик — только себе (решение владельца №5).
        d.encrypt_to = match (self.confidential, for_send) {
            (false, _) => Vec::new(),
            (true, true) => self.encrypt_to.clone(),
            (true, false) => {
                let me = d.from.email.to_lowercase();
                self.encrypt_to.iter().filter(|t| t.prepared.emails.contains(&me) && !t.prepared.key.is_gost()).take(1).cloned().collect()
            }
        };
        Ok(d)
    }

    /// Начать отправку или сохранение: команда для `Backend::send` или `None` (ошибка — в `status`).
    pub(crate) fn begin(&mut self, purpose: Purpose, ticket: u64, now: Instant) -> Option<Command> {
        if !purpose.sends() {
            self.last_save = now;
        }
        if purpose.sends() && self.blocked() {
            self.status = Status::Blocked;
            return None;
        }
        match self.draft(purpose.sends()) {
            Ok(d) => {
                self.pending = Some(Pending { ticket, snapshot: self.snapshot(), purpose });
                self.status = Status::Busy(purpose);
                let (draft, replace) = (Box::new(d), self.server_id.clone());
                Some(match purpose {
                    Purpose::Send => Command::Send { draft, replace, ticket, release: send_later::undo_release(self.undo_secs) },
                    Purpose::SendLater(at) => Command::Send { draft, replace, ticket, release: Release::At(at) },
                    Purpose::Save | Purpose::Autosave => Command::SaveDraft { draft, replace, ticket },
                })
            }
            Err(status) => {
                if purpose == Purpose::Autosave {
                    self.autosave_failed = Some(self.snapshot());
                }
                self.close_after_save = false;
                self.status = status;
                None
            }
        }
    }

    /// Шов не принял команду (демо, синхронизация остановилась): черновик остаётся в письме.
    pub(crate) fn refused(&mut self) {
        if let Some(p) = self.pending.take() {
            // Без сервера тот же текст сохранять снова бессмысленно — ждём изменений.
            self.autosave_failed = Some(p.snapshot);
            self.status = Status::NoServer(p.purpose);
        }
        self.close_after_save = false;
    }

    /// Черновик сохранён на сервере. `true` — закрыть письмо (просили «Да» при закрытии).
    pub(crate) fn on_saved(&mut self, ticket: u64, id: String, at: String) -> bool {
        let Some(p) = self.pending.take_if(|p| p.ticket == ticket) else { return false };
        self.server_id = Some(id);
        self.saved = p.snapshot;
        self.status = Status::Saved { at };
        std::mem::take(&mut self.close_after_save)
    }

    /// Версия на сервере (письмо вернулось в «Черновики» после отмены отправки): её заменит следующее сохранение.
    pub(crate) fn set_server_id(&mut self, id: String) {
        self.server_id = Some(id);
    }

    /// Версия черновика на сервере — удалить вместе с письмом («Удалить черновик»).
    pub(crate) fn server_id(&self) -> Option<&str> {
        self.server_id.as_deref()
    }

    /// Идёт «Отправить позже» (а не обычная отправка с окном «Отменить»).
    pub(crate) fn sending_later(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| matches!(p.purpose, Purpose::SendLater(_)))
    }

    /// Письмо ушло. `true` — это наша отправка, письмо закрывается.
    pub(crate) fn on_sent(&mut self, ticket: u64) -> bool {
        let Some(p) = self.pending.take_if(|p| p.ticket == ticket) else { return false };
        self.saved = p.snapshot;
        self.status = Status::None;
        true
    }

    /// Отправка или сохранение не удались: строка в письме, письмо не закрывается.
    pub(crate) fn on_failed(&mut self, ticket: u64, error: String) -> bool {
        let Some(p) = self.pending.take_if(|p| p.ticket == ticket) else { return false };
        if p.purpose == Purpose::Autosave {
            self.autosave_failed = Some(p.snapshot);
        }
        self.close_after_save = false;
        self.status = Status::Failed(error);
        true
    }

    /// Через сколько автосохранение; `None` — не нужно (нет изменений, идёт отправка, этот текст уже не сохранился).
    pub(crate) fn autosave_wait(&self, now: Instant) -> Option<Duration> {
        let snapshot = self.snapshot();
        if snapshot == self.saved || self.pending.is_some() || self.confirm_close || self.confirm_delete || self.autosave_failed == Some(snapshot) {
            return None;
        }
        Some(AUTOSAVE.saturating_sub(now.saturating_duration_since(self.last_save)))
    }

    pub(crate) fn should_autosave(&self, now: Instant) -> bool {
        self.autosave_wait(now).is_some_and(|w| w.is_zero())
    }

    /// Закрытие: без изменений — сразу (`true`), иначе строка «Сохранить черновик?».
    pub(crate) fn request_close(&mut self) -> bool {
        if !self.is_dirty() {
            return true;
        }
        self.confirm_delete = false;
        self.confirm_close = true;
        false
    }

    pub(crate) fn answer_close(&mut self, answer: CloseAnswer) -> CloseStep {
        self.confirm_close = false;
        match answer {
            CloseAnswer::Save => {
                self.close_after_save = true;
                CloseStep::Save
            }
            CloseAnswer::Discard => CloseStep::Close,
            CloseAnswer::Cancel => CloseStep::Stay,
        }
    }

    /// Вложения в списке письма (картинки внутри текста — `cid:` — не показываем).
    pub(crate) fn attachments(&self) -> impl Iterator<Item = (usize, &Attachment)> {
        self.base.attachments.iter().enumerate().filter(|(_, a)| a.inline_cid.is_none())
    }

    pub(crate) fn add_attachment(&mut self, a: Attachment) {
        self.base.attachments.push(a);
        let total: u64 = self.base.attachments.iter().map(|a| a.data.len() as u64).sum();
        if total > LARGE_TOTAL {
            self.status = Status::Large(total);
        }
        self.refresh_text_checks();
    }

    /// Картинки подписи (T3-07): те, на которые ссылается текст (`cid:…`), — вложениями inline; уже вложенные — не
    /// повторяются. `true` — что-то добавилось.
    pub(crate) fn attach_inline<'a>(&mut self, images: impl Iterator<Item = &'a postman_core::prefs::SignImage>) -> bool {
        let mut added = false;
        for img in images {
            let used = self.body.contains(&format!("cid:{}", img.cid));
            let have = self.base.attachments.iter().any(|a| a.inline_cid.as_deref() == Some(img.cid.as_str()));
            if used && !have {
                self.base.attachments.push(Attachment {
                    name: img.name.clone(),
                    mime: img.mime.clone(),
                    data: img.data.as_ref().clone(),
                    inline_cid: Some(img.cid.clone()),
                    blob_id: None,
                });
                added = true;
            }
        }
        added
    }

    /// Картинки текста, на которые он ссылается: `cid` и байты (полоса под текстом письма).
    pub(crate) fn inline_images(&self) -> Vec<(String, &Attachment)> {
        self.base
            .attachments
            .iter()
            .filter_map(|a| a.inline_cid.clone().filter(|cid| self.body.contains(&format!("cid:{cid}"))).map(|cid| (cid, a)))
            .collect()
    }

    pub(crate) fn remove_attachment(&mut self, i: usize) {
        if i < self.base.attachments.len() {
            self.base.attachments.remove(i);
        }
        self.refresh_text_checks();
    }

    // --- «Перед отправкой» ---

    fn refresh_text_checks(&mut self) {
        self.text_checks = checks::text(&self.body, self.attachments().count());
    }

    /// Замечания по адресатам: при смене адресатов и метки. `own` — свои домены, `book` — адресная книга.
    pub(crate) fn refresh_recipient_checks(&mut self, own: &[String], book: &AddressBook) {
        self.recipient_checks = checks::recipients(&self.recipients(), self.confidential, own, book);
    }

    /// «Конфиденциально» (S5-12 Ф4): замечания по сертификатам и получатели шифрования; без метки — пусто.
    pub(crate) fn set_encryption(&mut self, checks: Vec<Check>, to: Vec<postman_smime::seal::To>) {
        self.cert_checks = checks;
        self.encrypt_to = to;
    }

    /// «Убрать их»: адресаты без сертификата — из полей адресов. `true` — поля изменились.
    pub(crate) fn remove_addresses(&mut self, addrs: &[String]) -> bool {
        let mut changed = false;
        for f in [Field::To, Field::Cc, Field::Bcc] {
            let (closed, open) = segments(self.field(f));
            let kept: Vec<String> = closed
                .iter()
                .filter(|s| !parse_addresses(s).0.first().is_some_and(|a| addrs.iter().any(|x| x.eq_ignore_ascii_case(&a.email))))
                .cloned()
                .collect();
            if kept.len() != closed.len() {
                self.set(f, compose_field(&kept, &open));
                changed = true;
            }
        }
        changed
    }

    /// Замечания, которые не сняли.
    pub(crate) fn checks(&self) -> impl Iterator<Item = &Check> {
        self.text_checks.iter().chain(&self.recipient_checks).chain(&self.cert_checks).filter(|c| !self.dismissed.contains(&c.key()))
    }

    /// Отправку держит замечание (нет сертификата шифрования).
    pub(crate) fn blocked(&self) -> bool {
        self.checks().any(Check::blocks_send)
    }

    /// «Игнорировать», «ОК», «Оставить».
    pub(crate) fn dismiss(&mut self, check: &Check) {
        self.dismissed.insert(check.key());
        if self.status == Status::Blocked && !self.blocked() {
            self.status = Status::None;
        }
    }

    /// «Заменить адрес»: `from` → `to` в полях адресов (имя остаётся). `true` — поля изменились.
    pub(crate) fn replace_address(&mut self, from: &str, to: &str) -> bool {
        let mut changed = false;
        for f in [Field::To, Field::Cc, Field::Bcc] {
            let (closed, open) = segments(self.field(f));
            let mut hit = false;
            let closed: Vec<String> = closed
                .into_iter()
                .map(|s| match parse_addresses(&s).0.first() {
                    Some(a) if a.email.eq_ignore_ascii_case(from) => {
                        hit = true;
                        Address::new(a.name.clone(), to).display()
                    }
                    _ => s,
                })
                .collect();
            if hit {
                self.set(f, compose_field(&closed, &open));
                changed = true;
            }
        }
        changed
    }

    /// Сменить подпись в тексте: прежняя заменяется, нет её — новая встаёт в конец своего текста.
    pub(crate) fn set_signature(&mut self, signature: &str) {
        // Реплики фрагментов (`fragments.rs`): подпись — под ними, в хвосте письма.
        if self.swap_tail_signature(signature) {
            return;
        }
        let next = swap_signature(&self.body, &self.signature, signature);
        self.signature = signature.trim_end().to_owned();
        self.set(Field::Body, next);
    }

    /// Подпись по умолчанию из настроек: нетронутое письмо остаётся нетронутым — закрытие без вопроса, без черновика.
    pub(crate) fn set_default_signature(&mut self, signature: &str) {
        let clean = !self.is_dirty();
        self.set_signature(signature);
        if clean {
            self.saved = self.snapshot();
        }
    }
}

// --- текст письма: подпись, разметка, адреса ---

/// Где кончается свой текст: строка перед цитатой (`… пишет:`) или пересланное сообщение; нет их — конец текста.
pub(crate) fn own_end(body: &str) -> usize {
    let mut offset = 0;
    let mut prev: Option<usize> = None;
    for line in body.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("-------- ") {
            return offset;
        }
        if t.starts_with('>') {
            return prev.unwrap_or(offset);
        }
        prev = (!line.trim().is_empty()).then_some(offset);
        offset += line.len();
    }
    body.len()
}

/// Текст с другой подписью: прежняя (последняя в своём тексте) заменяется; без прежней — новая в конец своего
/// текста (перед цитатой ответа). Пустая новая — прежняя убирается вместе с пустыми строками перед ней.
pub(crate) fn swap_signature(body: &str, old: &str, new: &str) -> String {
    let (old, new) = (old.trim_end(), new.trim_end());
    let end = own_end(body);
    let own = &body[..end];
    if !old.is_empty()
        && let Some(at) = own.rfind(old)
    {
        let mut head = own[..at].to_owned();
        if new.is_empty() {
            head.truncate(head.trim_end_matches('\n').len());
        }
        return format!("{head}{new}{}{}", &own[at + old.len()..], &body[end..]);
    }
    if new.is_empty() {
        return body.to_owned();
    }
    let head = own.trim_end_matches('\n');
    let tail = &body[end..];
    let sep = if tail.is_empty() { "" } else { "\n\n" };
    format!("{head}\n\n{new}{sep}{tail}")
}

/// Кнопка панели форматирования: разметка Markdown (подчёркивание — `<u>`, его в Markdown нет).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Format {
    Bold,
    Italic,
    Underline,
    Bullets,
    Numbers,
    Link,
    Quote,
}

/// Граница символа не дальше `i`.
fn floor_char(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Разметка вокруг выделения (байты `sel`) или в начале его строк: новый текст и что выделить после.
/// Пустое выделение — пустая пара разметки с курсором внутри; строки с разметкой — разметку снять.
pub(crate) fn apply_format(text: &str, sel: Range<usize>, f: Format) -> (String, Range<usize>) {
    let (a, b) = (floor_char(text, sel.start.min(sel.end)), floor_char(text, sel.start.max(sel.end)));
    let wrap = |open: &str, close: &str| {
        let out = format!("{}{open}{}{close}{}", &text[..a], &text[a..b], &text[b..]);
        (out, a + open.len()..b + open.len())
    };
    match f {
        Format::Bold => wrap("**", "**"),
        Format::Italic => wrap("*", "*"),
        Format::Underline => wrap("<u>", "</u>"),
        Format::Link => {
            const URL: &str = "https://";
            if a == b {
                let out = format!("{}<{URL}>{}", &text[..a], &text[a..]);
                (out, a + 1..a + 1 + URL.len())
            } else {
                let out = format!("{}[{}]({URL}){}", &text[..a], &text[a..b], &text[b..]);
                let start = b + 3;
                (out, start..start + URL.len())
            }
        }
        Format::Bullets | Format::Numbers | Format::Quote => {
            let start = text[..a].rfind('\n').map_or(0, |i| i + 1);
            let end = text[b..].find('\n').map_or(text.len(), |i| b + i);
            let lines: Vec<&str> = text[start..end].split('\n').collect();
            let prefix = |i: usize| match f {
                Format::Bullets => "- ".to_owned(),
                Format::Quote => "> ".to_owned(),
                _ => format!("{}. ", i + 1),
            };
            let marked = |i: usize, l: &str| match f {
                Format::Numbers => l.strip_prefix(&prefix(i)).is_some(),
                _ => l.starts_with(&prefix(i)),
            };
            let all = lines.iter().enumerate().all(|(i, l)| marked(i, l));
            let block: Vec<String> =
                lines.iter().enumerate().map(|(i, l)| if all { l[prefix(i).len()..].to_owned() } else { format!("{}{l}", prefix(i)) }).collect();
            let block = block.join("\n");
            let out = format!("{}{block}{}", &text[..start], &text[end..]);
            let range = if a == b && lines.len() == 1 && lines[0].is_empty() {
                let at = start + block.len();
                at..at
            } else {
                start..start + block.len()
            };
            (out, range)
        }
    }
}

// --- вставка из Word и браузера (T3-06) ---

/// Больше HTML из буфера обмена в воркер не шлём: вставится простой текст.
pub(crate) const PASTE_HTML_MAX: usize = 8 << 20;

/// HTML из буфера обмена, разобранный воркером в IR, — Markdown письма: таблицы (GFM), списки, жирный, курсив,
/// ссылки, заголовки; остальное — текстом. Картинок у вставки нет: `cid:` чужого документа письму не принадлежит.
pub(crate) fn paste_markdown(mut doc: postman_ir::Document) -> String {
    use postman_ir::{Block, Inline};
    fn inlines(v: &mut Vec<Inline>) {
        v.retain(|i| !matches!(i, Inline::Image { .. }));
        for i in v.iter_mut() {
            if let Inline::Link { content, .. } = i {
                inlines(content);
            }
        }
    }
    fn blocks(v: &mut Vec<Block>) {
        v.retain(|b| !matches!(b, Block::Figure));
        for b in v.iter_mut() {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => inlines(content),
                Block::Quote(inner) => blocks(inner),
                Block::List { items, .. } => items.iter_mut().for_each(blocks),
                Block::Table(rows) => {
                    rows.iter_mut().flatten().for_each(inlines);
                    // Заголовок таблицы и так жирный: `**…**` Word в нём — лишние знаки.
                    for cell in rows.iter_mut().take(1).flatten() {
                        for i in cell.iter_mut() {
                            if let Inline::Text { marks, .. } = i {
                                marks.bold = false;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    blocks(&mut doc.blocks);
    align_tables(postman_ir::to_markdown(&doc).trim_matches('\n'))
}

/// Таблица в тексте письма (поле рисует её сеткой, T3-06): байты, строки текста `[первая, после последней)` и ячейки
/// по строкам (заголовок — первой, без строки выравнивания).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableAt {
    pub(crate) bytes: std::ops::Range<usize>,
    pub(crate) lines: std::ops::Range<usize>,
    pub(crate) rows: Vec<Vec<String>>,
}

/// Таблицы GFM текста по порядку.
pub(crate) fn tables(text: &str) -> Vec<TableAt> {
    let mut out = Vec::new();
    let (mut at, mut line) = (0, 0);
    while at < text.len() {
        if let Some(r) = table_around(text, at).filter(|r| r.start == at) {
            let src = &text[r.clone()];
            let n = src.matches('\n').count() + 1;
            let rows = src.split('\n').enumerate().filter(|(i, _)| *i != 1).map(|(_, l)| table_cells(l)).collect();
            out.push(TableAt { bytes: r.clone(), lines: line..line + n, rows });
            line += n;
            at = r.end + 1;
            continue;
        }
        match text[at..].find('\n') {
            Some(i) => {
                at += i + 1;
                line += 1;
            }
            None => break,
        }
    }
    out
}

/// Ссылка на картинку текста `![…](cid:…)` (логотип подписи, T3-07): байты, строка, `cid` и одна ли она в строке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageAt {
    pub(crate) bytes: std::ops::Range<usize>,
    pub(crate) line: usize,
    pub(crate) cid: String,
    pub(crate) alone: bool,
}

/// Ссылки `![…](cid:…)` текста по порядку.
pub(crate) fn image_refs(text: &str) -> Vec<ImageAt> {
    let mut out = Vec::new();
    let mut start = 0;
    for (line, l) in text.split('\n').enumerate() {
        let mut from = 0;
        let mut found: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        while let Some(i) = l[from..].find("![") {
            let a = from + i;
            let Some(m) = l[a..].find("](cid:").map(|m| a + m) else { break };
            let Some(close) = l[m..].find(')').map(|c| m + c) else { break };
            found.push((start + a..start + close + 1, l[m + "](cid:".len()..close].to_owned()));
            from = close + 1;
        }
        let rest: String = {
            let mut s = l.to_owned();
            for (r, _) in found.iter().rev() {
                s.replace_range(r.start - start..r.end - start, "");
            }
            s
        };
        // Одна в строке — и в цитате ответа (`> ![…](cid:…)`): строку займёт картинка.
        let alone = rest.trim_matches(|c: char| c == '>' || c.is_whitespace()).is_empty();
        out.extend(found.into_iter().map(|(bytes, cid)| ImageAt { bytes, line, cid, alone }));
        start += l.len() + 1;
    }
    out
}

/// Таблицы текста — с выровненными `|` (вставка из Word): читается в поле, отправляется так же.
pub(crate) fn align_tables(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        match table_around(text, at).filter(|r| r.start == at) {
            Some(r) => {
                let lines: Vec<&str> = text[r.clone()].split('\n').collect();
                let rows: Vec<Vec<String>> = lines.iter().enumerate().filter(|(n, _)| *n != 1).map(|(_, l)| table_cells(l)).collect();
                out.push_str(&format_table(&rows).0);
                at = r.end;
            }
            None => {
                let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
                out.push_str(&text[at..end]);
                at = end;
                continue;
            }
        }
        if at < text.len() {
            // `\n` после таблицы.
            out.push('\n');
            at += 1;
        }
    }
    out
}

// --- таблицы GFM (T3-06, решение владельца 2026-11-11) ---

/// Таблица кнопки «Таблица»: столбцов и строк данных (под строкой заголовка).
pub(crate) const TABLE_COLS: usize = 3;
pub(crate) const TABLE_ROWS: usize = 2;
/// Ширина пустой ячейки в знаках: столбцы видны и до ввода.
const CELL_MIN: usize = 3;

/// Ячейки строки таблицы: между `|`, без крайних пробелов; `\|` — знак внутри ячейки.
fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = if t.ends_with('|') && !t.ends_with("\\|") { &t[..t.len() - 1] } else { t };
    let (mut out, mut cur, mut esc) = (Vec::new(), String::new(), false);
    for c in t.chars() {
        if c == '|' && !esc {
            out.push(cur.trim().to_owned());
            cur.clear();
        } else {
            cur.push(c);
        }
        esc = c == '\\' && !esc;
    }
    out.push(cur.trim().to_owned());
    out
}

/// Строка выравнивания `| --- | :-: |`.
fn is_delimiter(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty() && cells.iter().all(|c| !c.is_empty() && c.trim_matches(':').chars().all(|ch| ch == '-') && c.contains('-'))
}

/// Строки таблицы вокруг байта `at`: подряд идущие строки, начинающиеся с `|`, вторая — строка выравнивания.
/// Итог — байты `[начало, конец)` (без последнего `\n`).
fn table_around(text: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let at = floor_char(text, at);
    let is_row = |l: &str| l.trim_start().starts_with('|');
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    if !is_row(&text[line_start..line_end]) {
        return None;
    }
    let mut start = line_start;
    while start > 0 {
        let prev = text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        if !is_row(&text[prev..start - 1]) {
            break;
        }
        start = prev;
    }
    let mut end = line_end;
    while end < text.len() {
        let next = text[end + 1..].find('\n').map_or(text.len(), |i| end + 1 + i);
        if !is_row(&text[end + 1..next]) {
            break;
        }
        end = next;
    }
    let lines: Vec<&str> = text[start..end].split('\n').collect();
    (lines.len() >= 2 && is_delimiter(lines[1])).then_some(start..end)
}

/// Таблица с выровненными `|`: ширина столбца — по самой длинной ячейке (в знаках). Итог — текст и байты содержимого
/// каждой ячейки (с дополнением пробелами) по строкам; строка выравнивания — без ячеек.
fn format_table(rows: &[Vec<String>]) -> (String, Vec<Vec<std::ops::Range<usize>>>) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let width = |c: usize| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0).max(CELL_MIN);
    let widths: Vec<usize> = (0..cols).map(width).collect();
    let (mut out, mut cells) = (String::new(), Vec::new());
    for (n, row) in rows.iter().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut ranges = Vec::new();
        out.push('|');
        for (c, w) in widths.iter().enumerate() {
            let cell = row.get(c).map_or("", String::as_str);
            out.push(' ');
            let start = out.len();
            out.push_str(cell);
            out.push_str(&" ".repeat(w - cell.chars().count()));
            ranges.push(start..out.len());
            out.push_str(" |");
        }
        cells.push(ranges);
        if n == 0 {
            out.push('\n');
            out.push('|');
            for w in &widths {
                out.push(' ');
                out.push_str(&"-".repeat(*w));
                out.push_str(" |");
            }
        }
    }
    (out, cells)
}

/// «Таблица»: пустая таблица `cols`×`rows` (и строка заголовка) с новой строки у курсора; выделена первая ячейка
/// заголовка — набранное встанет в неё.
pub(crate) fn insert_table(text: &str, at: usize, cols: usize, rows: usize) -> (String, std::ops::Range<usize>) {
    let at = floor_char(text, at);
    let empty: Vec<Vec<String>> = vec![vec![String::new(); cols.max(1)]; rows + 1];
    let (table, cells) = format_table(&empty);
    let before = &text[..at];
    let lead = if before.is_empty() || before.ends_with("\n\n") {
        ""
    } else if before.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let after = &text[at..];
    let trail = if after.is_empty() {
        "\n"
    } else if after.starts_with("\n\n") {
        ""
    } else if after.starts_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let start = at + lead.len();
    let first = cells.first().and_then(|r| r.first()).cloned().unwrap_or(0..0);
    (format!("{before}{lead}{table}{trail}{after}"), start + first.start..start + first.end)
}

/// Tab (`back` — Shift+Tab) в таблице: к следующей (предыдущей) ячейке, после последней — новая строка. Таблица
/// выравнивается заново; выделено содержимое ячейки. Курсор не в таблице или Shift+Tab в первой ячейке — `None`.
pub(crate) fn table_tab(text: &str, at: usize, back: bool) -> Option<(String, std::ops::Range<usize>)> {
    let range = table_around(text, at)?;
    let src = &text[range.clone()];
    let lines: Vec<&str> = src.split('\n').collect();
    let mut rows: Vec<Vec<String>> = lines.iter().enumerate().filter(|(n, _)| *n != 1).map(|(_, l)| table_cells(l)).collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1);
    for r in &mut rows {
        r.resize(cols, String::new());
    }
    // Где курсор: строка (без строки выравнивания) и ячейка — по числу `|` до него.
    let rel = at.min(range.end) - range.start;
    let line_no = src[..rel].matches('\n').count();
    let line_start = src[..rel].rfind('\n').map_or(0, |i| i + 1);
    let before = &src[line_start..rel];
    let pipes = before.match_indices('|').filter(|(i, _)| *i == 0 || before.as_bytes()[i - 1] != b'\\').count();
    let row = if line_no == 0 { 0 } else { line_no - 1 };
    let col = pipes.saturating_sub(1).min(cols - 1);
    let (next_row, next_col) = if line_no == 1 {
        // На строке выравнивания: Tab — в первую ячейку данных, Shift+Tab — в последнюю заголовка.
        if back { (0, cols - 1) } else { (1.min(rows.len() - 1), 0) }
    } else if back {
        match (row, col) {
            (0, 0) => return None,
            (r, 0) => (r - 1, cols - 1),
            (r, c) => (r, c - 1),
        }
    } else if col + 1 < cols {
        (row, col + 1)
    } else {
        if row + 1 >= rows.len() {
            rows.push(vec![String::new(); cols]);
        }
        (row + 1, 0)
    };
    let (table, cells) = format_table(&rows);
    let cell = cells.get(next_row).and_then(|r| r.get(next_col)).cloned()?;
    // Выделить набранное в ячейке (без дополнения пробелами).
    let typed = rows[next_row][next_col].len();
    let sel = if typed == 0 { cell.clone() } else { cell.start..cell.start + typed };
    let out = format!("{}{table}{}", &text[..range.start], &text[range.end..]);
    Some((out, range.start + sel.start..range.start + sel.end))
}

/// Поле адресов: готовые адреса (до последнего разделителя `;` или `,`, вне кавычек и `<…>`) и набираемый.
pub(crate) fn segments(text: &str) -> (Vec<String>, String) {
    let (mut closed, mut cur) = (Vec::new(), String::new());
    let (mut quoted, mut angle) = (false, false);
    for c in text.chars() {
        match c {
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' | '\n' if !quoted && !angle => {
                let t = cur.trim();
                if !t.is_empty() {
                    closed.push(t.to_owned());
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    (closed, cur.trim_start().to_owned())
}

/// Поле из готовых адресов и набираемого.
pub(crate) fn compose_field(closed: &[String], open: &str) -> String {
    let mut s: String = closed.iter().map(|c| format!("{c}; ")).collect();
    s.push_str(open);
    s
}

/// Набираемый адрес — после последнего `,` или `;`.
pub(crate) fn current_token(text: &str) -> &str {
    let start = text.rfind([',', ';']).map_or(0, |i| i + 1);
    text[start..].trim()
}

/// Вставить подсказку вместо набираемого адреса; дальше — разделитель для следующего.
pub(crate) fn accept_suggestion(text: &str, suggestion: &str) -> String {
    let start = text.rfind([',', ';']).map_or(0, |i| i + 1);
    let prefix = text[..start].trim_end();
    let sep = if prefix.is_empty() { "" } else { " " };
    format!("{prefix}{sep}{suggestion}; ")
}

static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

/// Тикет команды письма: итог синхронизации находит своё письмо по нему.
pub(crate) fn next_ticket() -> u64 {
    NEXT_TICKET.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn size_text(n: u64) -> String {
    match n {
        n if n < 1024 => tr!("compose-size-b", n = n.to_string()),
        n if n < 1024 * 1024 => tr!("compose-size-kb", n = format!("{:.0}", n as f64 / 1024.0)),
        n => tr!("compose-size-mb", n = format!("{:.1}", n as f64 / 1024.0 / 1024.0)),
    }
}

// --- письма у экрана ---

/// Адресная книга для подсказок: строится в фоне при первом письме, пополняется контактами с сервера.
pub(crate) type Book = Rc<RefCell<AddressBook>>;

/// Сколько адресатов в «Часто пишу» (макет «Контакты»: два десятка).
const FREQUENT_MAX: usize = 24;

/// Открытое письмо: во вкладке или ответом в области чтения.
pub(crate) struct Opened {
    pub(crate) id: u64,
    pub(crate) view: Entity<Composer>,
    _events: Subscription,
}

/// Ответ в области чтения: какое письмо и на какое письмо списка.
pub(crate) struct Inline {
    pub(crate) id: u64,
    pub(crate) letter: String,
}

/// Открытые письма и то, что им нужно от экрана: отправитель и подпись (Identity), адресная книга.
#[derive(Default)]
pub(crate) struct Composers {
    /// Сертификаты шифрования адресатов (S5-12 Ф4): «Конфиденциально».
    pub(crate) certs: crate::smime_seal::Certs,
    /// Свои адреса с сертификатом подписи (S5-12 Ф3): «Подписать» по умолчанию — только для них.
    pub(crate) signable: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    pub(crate) open: Vec<Opened>,
    pub(crate) inline: Option<Inline>,
    me: Option<(Address, String)>,
    /// Адреса, от которых можно отправить (S4-06): свои и общих ящиков.
    senders: Vec<Sender>,
    /// Общая с окном встречи (S3-05).
    pub(crate) book: Book,
    book_started: bool,
    /// Сколько сервер держит письма (`Event::Scheduled`); 0 — «Отправить позже» нет.
    max_delay: u64,
    /// Подписи учётной записи и подписи по умолчанию (Настройки → Подписи и шаблоны, S5-09).
    pub(crate) prefs: postman_core::prefs::AccountPrefs,
    /// «Переслать вложениями» ждёт тела писем с сервера (T1-17).
    attach_wait: Option<AttachWait>,
    /// Черновик ждёт тела с сервера, чтобы открыться на правку (T2-11).
    pub(crate) edit_wait: Option<String>,
}

/// «Переслать вложениями» (T1-17): письма `ids`; у `missing` в кэше нет тела с `Email.blobId` — спрошены у сервера.
struct AttachWait {
    ids: Vec<String>,
    missing: HashSet<String>,
}

/// Письмо для пересылки вложением: строка, `blob_id` письма целиком (RFC 5322), байты (демо — сразу).
pub(crate) type Letter = (postman_cache::EmailRow, Option<String>, Vec<u8>);

/// Имя вложения `.eml` из темы письма: без знаков, которые Windows не пускает в имя файла; пустая — «message» (как у
/// «Сообщить о фишинге» в синхронизации).
fn eml_name(subject: &str) -> String {
    let clean: String = subject.chars().map(|c| if c.is_control() || r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).take(80).collect();
    let clean = clean.trim().trim_end_matches('.');
    format!("{}.eml", if clean.is_empty() { "message" } else { clean })
}

/// Черновик «Переслать вложениями» (T1-17, как Outlook): каждое письмо — вложение `message/rfc822` `<тема>.eml`, по
/// дате; тема — пустая, у одного письма — «FW: <тема>», как у обычной пересылки (`postman_compose::respond`). Байты
/// без `data` скачивает отправка по `blob_id`, как вложения при пересылке.
pub(crate) fn attached_draft(mut letters: Vec<Letter>) -> Draft {
    letters.sort_by_key(|(r, _, _)| r.received_at);
    let subject = match letters.as_slice() {
        [(r, _, _)] => {
            let s = r.subject.trim();
            let lower = s.to_lowercase();
            if lower.starts_with("fw:") || lower.starts_with("fwd:") { s.to_owned() } else { format!("FW: {s}") }
        }
        _ => String::new(),
    };
    let attachments = letters
        .into_iter()
        .map(|(r, blob_id, data)| Attachment { name: eml_name(&r.subject), mime: "message/rfc822".into(), data, inline_cid: None, blob_id })
        .collect();
    Draft { subject, attachments, ..Default::default() }
}

/// Демо: письмо целиком из строки и тела демо-ящика (сервера, который отдал бы `Email.blobId`, нет).
fn demo_eml(row: &postman_cache::EmailRow) -> Vec<u8> {
    let body = postman_demo::body(&row.id);
    let d = Draft {
        from: Address::new(&row.from_name, &row.from_addr),
        to: body.to.iter().map(|a| Address::new(&a.name, &a.email)).collect(),
        cc: body.cc.iter().map(|a| Address::new(&a.name, &a.email)).collect(),
        subject: row.subject.clone(),
        markdown: body.text.clone().unwrap_or_else(|| row.preview.clone()),
        ..Default::default()
    };
    let id = body.message_id.clone().unwrap_or_else(|| format!("{}@demo.example", row.id));
    postman_compose::build(&d, row.received_at, &id).unwrap_or_default()
}

/// Отправленное письмо: что вернуть в композер, если отправку отменят (S5-06).
pub(crate) struct SentDraft {
    pub(crate) draft: Draft,
    /// «Отправить позже», а не обычная отправка с окном «Отменить».
    pub(crate) later: bool,
    /// Окно «Отменить» — задержка, с которой письмо ушло (CMF-03).
    pub(crate) undo_secs: u8,
}

impl Composers {
    /// Событие синхронизации: Identity и контакты — себе, итоги отправки — письмам (каждое ищет свой тикет).
    /// Отправленное письмо — экрану (окно «Отменить»).
    pub(crate) fn on_sync(&mut self, ev: &Event, cx: &mut App) -> Option<SentDraft> {
        // Адреса со своим сертификатом подписи (S5-12 Ф3) — в фоне, по своим адресам «От».
        if matches!(ev, Event::Me { .. } | Event::Senders(_)) {
            let mut addrs: Vec<String> = self.me.iter().map(|(a, _)| a.email.clone()).collect();
            addrs.extend(self.senders.iter().map(|s| s.address.email.clone()));
            if let Event::Me { address, .. } = ev {
                addrs.push(address.email.clone());
            }
            if let Event::Senders(list) = ev {
                addrs.extend(list.iter().map(|s| s.address.email.clone()));
            }
            let signable = self.signable.clone();
            cx.background_spawn(async move {
                let now = jiff::Timestamp::now().as_second();
                let found: std::collections::HashSet<String> =
                    addrs.into_iter().filter(|a| postman_core::smime::can_sign(a, now)).map(|a| a.to_lowercase()).collect();
                *signable.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = found;
            })
            .detach();
        }
        match ev {
            Event::Scheduled { max_delay, .. } => self.max_delay = *max_delay,
            Event::Me { address, signature } => {
                self.me = Some((address.clone(), signature.clone()));
                self.book.borrow_mut().remove(&address.email);
            }
            Event::Senders(list) => self.senders = list.clone(),
            Event::Contacts(list) => {
                self.book.borrow_mut().add_contacts(list.iter().map(|(n, e)| (n.as_str(), e.as_str())));
            }
            // Личные группы (T2-47) — в подсказки адреса.
            Event::ContactsLoaded(cards) => self.book.borrow_mut().set_groups(postman_core::contacts::Group::of_cards(cards)),
            // Сотрудники, переговорные и ресурсы (S4-02).
            Event::DirectoryLoaded(people) | Event::PeopleFound { people, .. } => {
                let items = people.iter().filter_map(|p| Some((p.display_name(), p.email.as_deref()?)));
                self.book.borrow_mut().add_directory(items);
            }
            _ => {}
        }
        let mut sent = None;
        for o in &self.open {
            o.view.update(cx, |c, cx| {
                if let Some(s) = c.on_sync(ev, cx) {
                    sent = Some(s);
                }
            });
        }
        sent
    }

    /// Мой адрес и подпись из Identity (живой ящик), если сервер их уже прислал.
    pub(crate) fn me(&self) -> Option<&(Address, String)> {
        self.me.as_ref()
    }

    /// Адреса, от которых можно отправить (свой и общих ящиков), без повторов — для подписей в настройках (S5-09).
    /// Адреса «От» моих учётных записей (S7-03б), без общих ящиков: (адрес, как показывать). Основная — первой.
    pub(crate) fn own_senders(&self, live: bool) -> Vec<(String, String)> {
        let demo = if live { Vec::new() } else { demo_senders() };
        let senders = if live { &self.senders } else { &demo };
        senders
            .iter()
            .filter(|s| s.account.as_deref().is_none_or(|a| postman_cache::profile_of(Some(a)) == Some(a)))
            .map(|s| (s.address.email.clone(), s.address.display()))
            .collect()
    }

    pub(crate) fn sender_addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self.me.iter().map(|(a, _)| a.email.clone()).collect();
        for s in &self.senders {
            if !out.iter().any(|e| e.eq_ignore_ascii_case(&s.address.email)) {
                out.push(s.address.email.clone());
            }
        }
        out
    }

    /// Свои адреса (без общих ящиков): автоответ отвечает на письма им (`:addresses`, S5-29).
    pub(crate) fn own_addresses(&self) -> Vec<String> {
        let mut out: Vec<String> = self.me.iter().map(|(a, _)| a.email.clone()).collect();
        for s in self.senders.iter().filter(|s| s.account.is_none()) {
            if !out.iter().any(|e| e.eq_ignore_ascii_case(&s.address.email)) {
                out.push(s.address.email.clone());
            }
        }
        out
    }

    /// Отправитель общего ящика `account` (ответ на письмо из него уходит от его имени), если сервер его дал.
    pub(crate) fn sender_for(&self, account: Option<&str>) -> Option<(Address, String)> {
        let account = account?;
        self.senders.iter().find(|s| s.account.as_deref() == Some(account)).map(|s| (s.address.clone(), s.signature.clone()))
    }

    pub(crate) fn get(&self, id: u64) -> Option<&Entity<Composer>> {
        self.open.iter().find(|o| o.id == id).map(|o| &o.view)
    }

    /// Ответ в области чтения.
    /// Адреса «От»: свои учётные записи и общие ящики.
    pub(crate) fn senders(&self) -> &[Sender] {
        &self.senders
    }

    pub(crate) fn inline_view(&self) -> Option<&Entity<Composer>> {
        self.inline.as_ref().and_then(|i| self.get(i.id))
    }

    /// Последнее открытое письмо (снимки `--screens`).
    pub(crate) fn last(&self) -> Option<&Entity<Composer>> {
        self.open.last().map(|o| &o.view)
    }
}

/// Откуда открыто письмо и что с ним сделать при открытии.
#[derive(Default)]
pub(crate) struct OpenAs {
    /// Ответ в области чтения на это письмо списка; `None` — вкладка.
    pub(crate) letter: Option<String>,
    /// Черновик уже на сервере (вернулся после отмены отправки, открыт из «Черновиков»).
    pub(crate) server_id: Option<String>,
    /// Вкладка — позади активной (T2-11: вкладки прошлого запуска).
    pub(crate) behind: bool,
    /// Личная группа (T2-47) — чипом в «Кому» («Написать» группе из «Контактов»).
    pub(crate) group: Option<Group>,
}

impl Mailer {
    /// Новое письмо во вкладке (`mail.new`, «Написать» в контактах).
    pub(crate) fn open_compose(&mut self, draft: Draft, cx: &mut Context<Self>) {
        self.open_draft(draft, OpenAs::default(), cx);
    }

    /// Ответ или пересылка в области чтения над письмом `letter`. Прежний ответ с текстом уходит во вкладку,
    /// без текста — закрывается; ответ на то же письмо с текстом остаётся (курсор — в него).
    pub(crate) fn open_reply(&mut self, draft: Draft, letter: String, cx: &mut Context<Self>) {
        if let Some(i) = &self.compose.inline {
            let (id, same) = (i.id, i.letter == letter);
            let dirty = self.compose.get(id).is_some_and(|c| c.read(cx).model.is_dirty());
            if same && dirty {
                self.focus_composer(id, cx);
                return;
            }
            self.detach_inline(cx);
        }
        self.open_draft(draft, OpenAs { letter: Some(letter), ..Default::default() }, cx);
    }

    /// Письмо — во вкладке или в области чтения. Полям ввода нужно окно: создаём после текущего обновления.
    pub(crate) fn open_draft(&mut self, draft: Draft, how: OpenAs, cx: &mut Context<Self>) {
        self.load_address_book(cx);
        let live = self.backend.is_live();
        let senders = if live { self.compose.senders.clone() } else { demo_senders() };
        // Демо: меню «Отправить позже» видно (отправка всё равно скажет «нет сервера»).
        let max_delay = if live { self.compose.max_delay } else { send_later::SERVER_HOLD_MAX };
        let account = postman_cache::account_of(how.letter.as_deref().unwrap_or(""));
        let categories = self
            .list
            .read(cx)
            .labels
            .for_account(account)
            .map(|t| t.visible().map(|c| (c.keyword.clone(), c.name.clone(), c.shown_color())).collect())
            .unwrap_or_default();
        let place = if how.letter.is_some() { Place::Inline } else { Place::Tab };
        // Ответ на письмо общего ящика (S5-28): адрес ящика и кто держит письмо — предупреждение у «От».
        let shared = how.letter.as_deref().and_then(|letter| {
            let account = postman_cache::account_of(letter)?;
            let address = senders.iter().find(|s| s.account.as_deref() == Some(account))?.address.email.clone();
            let reading = self.reading.read(cx);
            let row = reading.reading.as_ref().map(|r| &r.row).filter(|r| r.id == letter)?;
            Some(crate::shared::Reply::of(row, &address, cx))
        });
        let open = Open {
            shared,
            signable: self.compose.signable.clone(),
            certs: self.compose.certs.clone(),
            senders,
            max_delay,
            me: self.compose.me.clone(),
            book: self.compose.book.clone(),
            lists: self.contacts.read(cx).list_emails(),
            mailer: cx.entity().downgrade(),
            categories,
            place,
            screens: self.screens.is_some(),
            prefs: self.compose.prefs.clone(),
            worker: self.reading.read(cx).worker.clone(),
        };
        let (handle, mailer) = (self.window, cx.entity());
        cx.defer(move |cx| {
            let made = handle.update(cx, |_, window, cx| cx.new(|cx| Composer::new(draft, open, window, cx)));
            let Ok(view) = made else { return };
            mailer.update(cx, |m, cx| m.add_composer(view, how, cx));
        });
    }

    fn add_composer(&mut self, view: Entity<Composer>, how: OpenAs, cx: &mut Context<Self>) {
        let id = view.read(cx).id;
        if let Some(server_id) = how.server_id {
            view.update(cx, |c, _| c.model.set_server_id(server_id));
        }
        if let Some(g) = how.group {
            view.update(cx, |c, cx| {
                c.model.add_group_to(Field::To, g);
                cx.notify();
            });
        }
        let events = cx.subscribe(&view, move |this, _, ev: &ComposerEvent, cx| match ev {
            ComposerEvent::Close => this.close_composer(id, cx),
            ComposerEvent::Expand => this.expand_composer(id, cx),
        });
        self.compose.open.push(Opened { id, view, _events: events });
        match how.letter {
            Some(letter) => self.compose.inline = Some(Inline { id, letter }),
            None if how.behind => {
                self.tabs.open_behind(TabKind::Compose(id));
            }
            None => {
                self.tabs.open(TabKind::Compose(id));
            }
        }
        cx.notify();
    }

    /// Письмо — черновик (T2-11): метка `$draft` или папка «Черновики».
    pub(crate) fn is_draft(&self, row: &postman_cache::EmailRow, cx: &App) -> bool {
        row.has_keyword(postman_core::labels::DRAFT)
            || self.folders.read(cx).mailboxes.iter().any(|b| b.role.as_deref() == Some("drafts") && row.mailbox_ids.contains(&b.id))
    }

    /// Черновик — во вкладку на правку (T2-11, Enter / двойной щелчок / «Продолжить»): уже открыт — его вкладка;
    /// сохранение заменит версию на сервере. Тела ещё нет — откроется, когда придёт.
    pub(crate) fn edit_draft(&mut self, row: postman_cache::EmailRow, cx: &mut Context<Self>) {
        let open = self.compose.open.iter().find(|o| o.view.read(cx).model.server_id() == Some(row.id.as_str())).map(|o| o.id);
        if let Some(id) = open {
            let tab = self.tabs.iter().find(|t| t.kind == TabKind::Compose(id)).map(|t| t.id);
            if tab.is_some_and(|t| self.tabs.activate(t)) {
                self.refocus(cx);
                cx.notify();
            }
            return;
        }
        let shown = self.reading.read(cx).reading.as_ref().filter(|r| r.row.id == row.id).and_then(|r| r.body.clone());
        match shown.or_else(|| self.cache.body(&row.id).ok().flatten()) {
            Some(body) => {
                self.compose.edit_wait = None;
                let id = row.id.clone();
                let draft = crate::send_later::draft_of(self, row, body);
                self.open_draft(draft, OpenAs { server_id: Some(id), ..Default::default() }, cx);
            }
            None => {
                self.compose.edit_wait = Some(row.id.clone());
                self.open_letter(row, cx);
            }
        }
    }

    /// Тело письма пришло: черновик ждал его — во вкладку (T2-11).
    pub(crate) fn body_shown(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.compose.edit_wait.as_deref() != Some(id) {
            return;
        }
        let row = self.reading.read(cx).reading.as_ref().map(|r| r.row.clone()).filter(|r| r.id == id);
        if let Some(row) = row {
            self.edit_draft(row, cx);
        }
    }

    /// Письмо закрылось (отправлено, «Нет» в строке закрытия, удалён черновик): убрать вкладку или ответ.
    pub(crate) fn close_composer(&mut self, id: u64, cx: &mut Context<Self>) {
        self.compose.open.retain(|o| o.id != id);
        if self.compose.inline.as_ref().is_some_and(|i| i.id == id) {
            self.compose.inline = None;
        }
        let tab = self.tabs.iter().find(|t| t.kind == TabKind::Compose(id)).map(|t| t.id);
        if let Some(tab) = tab {
            self.tabs.close(tab);
        }
        self.refocus(cx);
        cx.notify();
    }

    /// Фокус — в активную вкладку после её смены не от щелчка (письмо закрылось, ответ развернули).
    fn refocus(&mut self, cx: &mut Context<Self>) {
        let (handle, mailer) = (self.window, cx.entity());
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| mailer.update(cx, |m, cx| m.focus_active_tab(window, cx)));
        });
    }

    /// «Развернуть»: ответ из области чтения — во вкладку, с тем же черновиком.
    pub(crate) fn expand_composer(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.compose.inline.as_ref().is_none_or(|i| i.id != id) {
            return;
        }
        self.compose.inline = None;
        if let Some(view) = self.compose.get(id) {
            view.update(cx, |c, cx| c.set_place(Place::Tab, cx));
        }
        self.tabs.open(TabKind::Compose(id));
        self.refocus(cx);
        cx.notify();
    }

    /// Ответ уходит из области чтения (открыли другое письмо): с текстом — во вкладку, без — закрывается.
    fn detach_inline(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.compose.inline.as_ref().map(|i| i.id) else { return };
        let dirty = self.compose.get(id).is_some_and(|c| c.read(cx).model.is_dirty());
        if dirty {
            self.expand_composer(id, cx);
            self.show_mail_tab(cx);
        } else {
            self.close_composer(id, cx);
        }
    }

    /// Область чтения показывает другое письмо — ответ на прежнее не остаётся над ним.
    pub(crate) fn inline_follows_letter(&mut self, cx: &mut Context<Self>) {
        let Some(letter) = self.compose.inline.as_ref().map(|i| i.letter.clone()) else { return };
        let shown = self.reading.read(cx).reading.as_ref().map(|r| r.row.id.clone());
        if shown.as_deref() != Some(letter.as_str()) {
            self.detach_inline(cx);
        }
    }

    /// Ctrl+W или «×» на вкладке письма: без изменений — закрыть, иначе строка «Сохранить черновик?».
    pub(crate) fn close_compose_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.compose.get(id).cloned() else {
            self.close_composer(id, cx);
            return;
        };
        let tab = self.tabs.iter().find(|t| t.kind == TabKind::Compose(id)).map(|t| t.id);
        if let Some(tab) = tab {
            self.tabs.activate(tab);
        }
        view.update(cx, |c, cx| c.close_or_ask(window, cx));
        cx.notify();
    }

    fn focus_composer(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(view) = self.compose.get(id).cloned() else { return };
        let handle = self.window;
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| view.update(cx, |c, cx| c.focus_body(window, cx)));
        });
    }

    /// Книга адресов из хранилища — в фоне, один раз; письма видят её, как только она готова.
    /// Ссылка `mailto:` (Postman — клиент по умолчанию, S5-30): новое письмо с адресатами, темой и текстом.
    pub(crate) fn open_mailto(&mut self, uri: &str, cx: &mut Context<Self>) {
        let Some(m) = postman_compose::parse_mailto(uri) else { return };
        let draft = postman_compose::Draft { to: m.to, cc: m.cc, bcc: m.bcc, subject: m.subject, markdown: m.body, ..Default::default() };
        self.set_section(crate::Section::Mail, cx);
        self.open_compose(draft, cx);
    }

    pub(crate) fn load_address_book(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.compose.book_started, true) {
            return;
        }
        let cache = self.cache.clone();
        // Демо: контакты и справочник демо-набора (у живого ящика они придут с сервера); я — владелец демо-ящика.
        let demo = !self.backend.is_live();
        let me = match &self.compose.me {
            Some((a, _)) => a.email.clone(),
            None if demo => postman_demo::ME.to_owned(),
            None => String::new(),
        };
        let task = cx.background_spawn(async move {
            let mut book = AddressBook::from_cache(&cache, &me);
            if demo {
                crate::contacts::add_demo(&mut book);
                book.remove(&me);
            }
            book
        });
        let book = self.compose.book.clone();
        cx.spawn(async move |this, cx| {
            let mut built = task.await;
            {
                let mut b = book.borrow_mut();
                // Контакты с сервера, пришедшие раньше книги, — не теряем.
                built.absorb(std::mem::take(&mut *b));
                *b = built;
            }
            // Книга собрана — «Часто пишу» в «Контактах» (S5-25), имена «Назначено: …» в списке и шапке (S5-28).
            let _ = this.update(cx, |m: &mut Mailer, cx| {
                m.contacts_frequent(cx);
                m.list.update(cx, |_, cx| cx.notify());
                m.reading.update(cx, |_, cx| cx.notify());
            });
        })
        .detach();
    }

    /// `mail.forward_attached` (T1-17): новое письмо вкладкой, в нём письма `ids` вложениями. Строки и тела — из кэша
    /// в фоне; у письма без тела с `Email.blobId` тело спрашивается у сервера, окно откроется, когда придут все.
    pub(crate) fn forward_attached(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.forward_attached_with(ids, false, cx);
    }

    fn forward_attached_with(&mut self, ids: Vec<String>, asked: bool, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return self.toast(pets_ui::Level::Info, tr!("actions-select-letter"), cx);
        }
        let live = self.backend.is_live();
        let cache = self.cache.clone();
        let task = cx.background_spawn(async move {
            let rows = cache.rows_by_ids(&ids).unwrap_or_default();
            rows.into_iter()
                .map(|row| {
                    let blob = if live { cache.body(&row.id).ok().flatten().and_then(|b| b.blob_id) } else { None };
                    let data = if live { Vec::new() } else { demo_eml(&row) };
                    (row, blob, data)
                })
                .collect::<Vec<Letter>>()
        });
        cx.spawn(async move |this, cx| {
            let letters = task.await;
            let _ = this.update(cx, |m, cx| m.forward_attached_ready(letters, asked, cx));
        })
        .detach();
    }

    /// Письма из кэша: всё есть — окно письма; нет тел — спросить сервер (один раз) и ждать `BodyReady`.
    fn forward_attached_ready(&mut self, letters: Vec<Letter>, asked: bool, cx: &mut Context<Self>) {
        let ready = |(_, blob, data): &Letter| blob.is_some() || !data.is_empty();
        let missing: HashSet<String> = letters.iter().filter(|l| !ready(l)).map(|(r, _, _)| r.id.clone()).collect();
        if !missing.is_empty() && !asked {
            for id in &missing {
                if !self.backend.send(Command::Body(id.clone())) {
                    self.compose.attach_wait = None;
                    return self.sync_stopped(cx);
                }
            }
            let ids = letters.iter().map(|(r, _, _)| r.id.clone()).collect();
            self.compose.attach_wait = Some(AttachWait { ids, missing });
            return self.toast(pets_ui::Level::Info, tr!("compose-attached-loading"), cx);
        }
        let skipped = missing.len();
        let letters: Vec<Letter> = letters.into_iter().filter(ready).collect();
        if letters.is_empty() {
            return self.toast(pets_ui::Level::Error, tr!("compose-attached-none"), cx);
        }
        self.open_compose(attached_draft(letters), cx);
        if skipped > 0 {
            self.toast(pets_ui::Level::Warning, tr!("compose-attached-skipped", n = skipped as i64), cx);
        }
    }

    /// Тело письма пришло (`BodyReady`): «Переслать вложениями» дождался всех — открыть письмо.
    pub(crate) fn forward_body_ready(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(w) = &mut self.compose.attach_wait else { return };
        w.missing.remove(id);
        if w.missing.is_empty() {
            let ids = std::mem::take(&mut w.ids);
            self.compose.attach_wait = None;
            self.forward_attached_with(ids, true, cx);
        }
    }

    /// «Часто пишу» (S5-25): адресаты переписки за 30 дней по частоте и давности — в «Контакты».
    pub(crate) fn contacts_frequent(&mut self, cx: &mut Context<Self>) {
        let now = jiff::Timestamp::now().as_second();
        let list: Vec<crate::contacts::Frequent> = self
            .compose
            .book
            .borrow()
            .frequent(now, postman_core::contacts::FREQUENT_DAYS, FREQUENT_MAX)
            .into_iter()
            .map(|e| crate::contacts::Frequent { name: e.name.clone(), email: e.email.clone(), last: e.last })
            .collect();
        self.contacts.update(cx, |c, cx| c.set_frequent(list, cx));
    }
}

/// Демо: свои адреса всех учётных записей (`postman_demo::ACCOUNTS`, S7-09б) и общий ящик отдела
/// (`postman_demo::SHARED`) — выбор «От» виден и без сервера; основная — первой, общий ящик — за ней, как в дереве.
pub(crate) fn demo_senders() -> Vec<Sender> {
    let sender = |account: Option<&str>, email: &str| Sender {
        account: account.map(str::to_owned),
        address: Address::new("", email),
        signature: String::new(),
    };
    let mut accounts = postman_demo::ACCOUNTS.iter().map(|a| sender(a.profile, a.email));
    let own = accounts.next();
    let shared = postman_demo::SHARED.iter().filter(|(_, _, read_only)| !read_only).map(|&(id, email, _)| sender(Some(id), email));
    own.into_iter().chain(shared).chain(accounts).collect()
}

/// Опись части (ADR 0005, `parts.rs`).
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "compose", texts: TEXTS, commands: COMMANDS, scenes: scenes::SCENES, ..crate::part::Part::NONE };

/// Тексты части (`i18n/<язык>/<часть>.ftl`).
const TEXTS: &[pets_i18n::Resource] = &[
    ("ru", include_str!("../i18n/ru/compose.ftl")),
    ("ru", include_str!("../i18n/ru/composetab.ftl")),
    ("ru", include_str!("../i18n/ru/send.ftl")),
    ("en", include_str!("../i18n/en/compose.ftl")),
    ("en", include_str!("../i18n/en/composetab.ftl")),
    ("en", include_str!("../i18n/en/send.ftl")),
];

/// Письмо — вкладка или ответ в области чтения (S3-01, S5-06б): отправка, разметка, «Развернуть», удалить черновик.
// Имя — область действий (`compose::…`), как в id команд: так обработчики читаются и их находит `docs/PARITY.md`.
#[allow(clippy::module_inception)]
pub(crate) mod compose {
    gpui_kit::actions!(
        compose,
        [Send, Attach, Close, Bold, Italic, Underline, Bullets, Numbers, Link, Quote, Table, Expand, Discard, AddressBook, ShareAttachments]
    );
}

/// Команды части (`commands.rs`: контексты и `cmd`).
const COMMANDS: &[pets_commands::Command] = {
    use crate::commands::{COMPOSER, cmd};
    &[
        // Отправить — Ctrl+Enter (Alt+S Outlook не берём: Alt не используется).
        cmd("compose.send", &["ctrl-enter"], COMPOSER),
        // Не Outlook: вложить файл — кнопкой или перетаскиванием, без клавиши.
        cmd("compose.attach", &[], COMPOSER),
        cmd("compose.close", &["escape"], COMPOSER),
        // Панель форматирования (S5-06б): как в Outlook — Ctrl+B, Ctrl+I, Ctrl+U; списки, ссылка и цитата — кнопками.
        cmd("compose.bold", &["ctrl-b"], COMPOSER),
        cmd("compose.italic", &["ctrl-i"], COMPOSER),
        cmd("compose.underline", &["ctrl-u"], COMPOSER),
        cmd("compose.bullets", &[], COMPOSER),
        cmd("compose.numbers", &[], COMPOSER),
        cmd("compose.link", &[], COMPOSER),
        cmd("compose.quote", &[], COMPOSER),
        // Таблица GFM (T3-06): кнопкой панели, без клавиши (Outlook — меню «Вставка → Таблица»); Tab — по ячейкам.
        cmd("compose.table", &[], COMPOSER),
        // Не Outlook (там «Открыть в новом окне»): ответ из области чтения — во вкладку.
        cmd("compose.expand", &[], COMPOSER),
        cmd("compose.discard", &[], COMPOSER),
        // «Кому…» (T3-03): адресная книга письма панелью — щелчком по «Кому», «Копия», «Скрытая» или отсюда (в «Кому»).
        // Без клавиши: Ctrl+Shift+B Outlook — у встречи (S5-23).
        cmd("compose.address_book", &[], COMPOSER),
        // Вложения беседы новым участникам ответа (решение владельца 2026-11-13): отключить или вернуть для письма —
        // кнопкой строки над текстом, без клавиши.
        cmd("compose.share_attachments", &[], COMPOSER),
    ]
};

/// Сцены `--screens` письма (ADR 0005).
pub(crate) mod scenes {
    use gpui_kit::*;

    use crate::composer::scenes as samples;
    use crate::screens::{self, Scene};
    use crate::{Mailer, Section};

    pub(crate) const SCENES: &[Scene] = &[
        // Новое письмо во вкладке (S5-06б): адресаты чипами (один внешний), текст, вложение, панель справа.
        Scene::new("compose", compose, compose_ready).after("task-repeat"),
        // То же письмо с открытым меню «Отправить позже» у кнопки «Отправить» (S5-06).
        Scene::new("send-later", send_later, send_later_ready).continues(),
        // Шаблон с полями (T2-52): «/кп» и пробел в тексте — строка «Поля шаблона» над текстом письма.
        Scene::new("compose-template", template, template_ready).continues(),
        // Второе письмо — вторая вкладка: все замечания «Перед отправкой» (карта, вложение, внешние, сменённый адрес).
        Scene::new("checks", checks, checks_ready),
        // «Конфиденциально» (S5-12 Ф4): адресат с сертификатом (Петров из подписанного образца демо) — замок у чипа,
        // замечаний по сертификатам нет; письмо зашифруется ему и себе.
        Scene::new("compose-confidential", confidential, confidential_ready),
        // «Конфиденциально» адресату без сертификата (S5-12.5б): «Нет сертификата» с «Найти в каталоге» между «Убрать
        // их» и «Снять „Конфиденциально“».
        Scene::new("compose-no-cert", no_cert, no_cert_ready),
        // Ответ в области чтения над письмом с цитатой (S3-02, S5-06б).
        Scene::new("reply-inline", reply, reply_ready).live(),
        // Письмо из «Черновиков» (T2-11): полоса «Черновик — не отправлен · Продолжить», без быстрого ответа.
        Scene::new("reading-draft", drafts, draft_ready).input(DRAFT_OPEN),
        // «Продолжить» — черновик во вкладке письма; сохранение заменит версию на сервере.
        Scene::new("draft-edit", stay, draft_tab_ready).input(&[screens::Input::Press("draft-edit")]).continues(),
        // T3-06: «Таблица» в конце текста, ячейки по Tab — курсор в таблице: правится её Markdown.
        Scene::new("compose-t306-table", table_letter, table_typed).input(TABLE).on_request(),
        // T3-06: то же письмо, курсор вне таблицы (Ctrl+Home) — таблица сеткой поверх своего Markdown.
        Scene::new("compose-t306-grid", stay, table_grid).input(&[screens::Input::Keys(&["ctrl-home"])]).continues().on_request(),
        // T3-06: вставка HTML из Word (буфер обмена — образец сцены, Windows не трогаем): таблица, список, жирный.
        Scene::new("compose-t306-paste", table_letter, table_pasted).input(PASTE).on_request(),
        // T3-07: новое письмо с подписью по умолчанию с логотипом: картинка под текстом, вложение inline.
        Scene::new("compose-t307-sign-image", sign_letter, sign_image_shown).on_request(),
        // Ответ всем с новым участником (решение владельца 2026-11-13): над текстом — «Новым участникам (Ольга
        // Смирнова) уйдут вложения беседы: 2 файла» и «Не отправлять».
        Scene::new("compose-share", share_letter, share_shown),
    ];

    /// Письмо сцен T3-06: адресат есть — курсор в тексте.
    fn table_letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.close_details(cx));
        m.set_section(Section::Mail, cx);
        screens::remember_composer(m, cx);
        let draft = postman_compose::Draft {
            to: vec![postman_compose::Address::new("Мария Иванова", "m.ivanova@corp.example")], // i18n-ok: образец
            subject: "План продаж".into(),                                                      // i18n-ok: образец
            markdown: "Добрый день!\n\nПлан на неделю:".into(),                                 // i18n-ok: образец
            ..Default::default()
        };
        m.open_compose(draft, cx);
        Ok(())
    }

    /// Новое письмо сцены нарисовано (не прежнее), курсор — в конце текста.
    fn letter_drawn(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let before = screens::composer_before(m);
        screens::compose_tab(m).is_some_and(|c| Some(c.read(cx).id) != before && c.read(cx).drawn())
    }

    const TABLE: &[screens::Input] = &[
        screens::Input::Until(letter_drawn),
        screens::Input::Keys(&["ctrl-end"]),
        screens::Input::Press("fmt-table"),
        screens::Input::Text("Город"), // i18n-ok: ввод сценария
        screens::Input::Keys(&["tab"]),
        screens::Input::Text("План"), // i18n-ok: ввод сценария
        screens::Input::Keys(&["tab"]),
        screens::Input::Text("Факт"), // i18n-ok: ввод сценария
        screens::Input::Keys(&["tab"]),
        screens::Input::Text("Омск"), // i18n-ok: ввод сценария
        screens::Input::Keys(&["tab"]),
        screens::Input::Text("120"),
        screens::Input::Keys(&["tab"]),
        screens::Input::Text("131"),
    ];

    fn table_typed(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Столбцы выровнены Tab: строка `---` — по ширине «Город», «План», «Факт».
        screens::compose_tab(m).is_some_and(|c| {
            let body = &c.read(cx).model.body;
            body.contains("| Город | План | Факт |\n| ----- | ---- | ---- |") && body.contains("| Омск  | 120  | 131") // i18n-ok: ввод сценария
                && c.read(cx).graphics().1.is_some()
        })
    }

    /// Курсор вне таблиц и картинок, а графика в поле есть.
    fn table_grid(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| {
            let (zones, inside) = c.read(cx).graphics();
            zones > 0 && inside.is_none()
        })
    }

    /// HTML буфера обмена сцены: так его кладёт Word (таблица, список, жирный, шрифт и размер — без них).
    const WORD_HTML: &str = include_str!("../tests/fixtures/word-paste.html");

    fn paste_word(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::hold_clipboard_html(Some(WORD_HTML.to_owned()));
        let html = screens::clipboard_html(super::PASTE_HTML_MAX).ok_or("no clipboard HTML")?;
        let c = screens::compose_tab(m).cloned().ok_or("no compose tab")?;
        c.update(cx, |c, cx| c.paste_html(html, None, window, cx));
        Ok(())
    }

    const PASTE: &[screens::Input] =
        &[screens::Input::Until(letter_drawn), screens::Input::Keys(&["ctrl-end", "enter", "enter"]), screens::Input::Run(paste_word)];

    fn table_pasted(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| {
            let c = c.read(cx);
            !c.pasting
                && c.model.body.contains("| Омск  | 120  | 131  |") // i18n-ok: образец
                && c.model.body.contains("**Итоги недели**") // i18n-ok: образец
                && c.graphics().0 > 0
                && c.graphics().1.is_none()
        })
    }

    fn sign_letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.close_details(cx));
        m.set_section(Section::Mail, cx);
        screens::remember_composer(m, cx);
        let mut ids: Vec<String> = super::demo_senders().into_iter().map(|s| s.address.email).collect();
        ids.extend(m.compose.sender_addresses());
        m.compose.prefs = crate::settings_sign::screen_sample_logo(&ids);
        let draft = postman_compose::Draft {
            to: vec![postman_compose::Address::new("Мария Иванова", "m.ivanova@corp.example")], // i18n-ok: образец
            subject: "Коммерческое предложение".into(),                                         // i18n-ok: образец
            ..Default::default()
        };
        m.open_compose(draft, cx);
        Ok(())
    }

    fn sign_image_shown(s: &Scene, m: &Mailer, cx: &App) -> bool {
        letter_drawn(s, m, cx)
            && screens::compose_tab(m).is_some_and(|c| {
                let c = c.read(cx);
                c.model.inline_images().len() == 1
                    && c.images.values().all(Option::is_some)
                    && !c.images.is_empty()
                    && c.graphics().0 == 1
                    && c.graphics().1.is_none()
            })
    }

    /// Ответ всем с новым участником (решение владельца 2026-11-13): в «Копии» — Ольга, её нет в беседе.
    fn share_letter(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.close_details(cx));
        m.set_section(Section::Mail, cx);
        screens::remember_composer(m, cx);
        let file = |name: &str, mime: &str, blob: &str| postman_compose::Attachment {
            name: name.into(),
            mime: mime.into(),
            data: postman_demo::blob(blob).unwrap_or_default(),
            inline_cid: None,
            blob_id: Some(blob.into()),
        };
        let draft = postman_compose::Draft {
            to: vec![postman_compose::Address::new("Иван Петров", "i.petrov@corp.example")], // i18n-ok: образец
            cc: vec![postman_compose::Address::new("Ольга Смирнова", "o.smirnova@corp.example")], // i18n-ok: образец
            subject: "RE: Итоги квартала".into(),                                            // i18n-ok: образец
            markdown: "Добавляю Ольгу — ей нужны отчёт и смета.\n\n".into(),                 // i18n-ok: образец
            in_reply_to: Some("q3@corp.example".into()),
            references: vec!["q3@corp.example".into()],
            share: Some(postman_compose::Share {
                known: vec!["i.petrov@corp.example".into(), postman_demo::ME.into()],
                attachments: vec![file("Отчёт.pdf", "application/pdf", "pdf"), file("Бюджет.xlsm", "application/vnd.ms-excel", "xlsm")], // i18n-ok: образец
                note: String::new(),
                off: false,
            }),
            ..Default::default()
        };
        m.open_compose(draft, cx);
        Ok(())
    }

    fn share_shown(s: &Scene, m: &Mailer, cx: &App) -> bool {
        letter_drawn(s, m, cx) && screens::compose_tab(m).is_some_and(|c| c.read(cx).model.share_line().is_some_and(|(_, n, off)| n == 2 && !off))
    }

    fn drafts(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.set_section(Section::Mail, cx);
        m.open_mailbox("drafts".into(), cx);
        Ok(())
    }

    fn drafts_listed(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let list = m.list.read(cx);
        list.mailbox.as_deref() == Some("drafts") && list.view.is_some() && !list.computing && screens::rows_loaded(list) && list.count > 0
    }

    fn open_first(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.list.update(cx, |l, cx| l.select(0, cx));
        Ok(())
    }

    const DRAFT_OPEN: &[screens::Input] = &[screens::Input::Until(drafts_listed), screens::Input::Run(open_first)];

    fn draft_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let r = m.reading.read(cx);
        r.reading.as_ref().is_some_and(|x| x.row.id == postman_demo::DRAFT && x.doc.is_some()) && r.draft.as_deref() == Some(postman_demo::DRAFT)
    }

    fn stay(_: &Scene, _: &mut Mailer, _: &mut Window, _: &mut Context<Mailer>) -> Result<(), String> {
        Ok(())
    }

    fn draft_tab_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn() && c.read(cx).model.server_id() == Some(postman_demo::DRAFT))
    }

    fn compose(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.cal.update(cx, |c, cx| c.close_details(cx));
        m.set_section(Section::Mail, cx);
        // Как `mail.new`: письмо — вкладкой (поля ввода создаются после кадра).
        let mut draft = samples::compose_draft();
        draft.attachments.push(samples::sample_attachment());
        m.open_compose(draft, cx);
        Ok(())
    }

    fn compose_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn())
    }

    fn send_later(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let Some(c) = m.compose.last().cloned() else { return Err("no compose tab".into()) };
        // «Ждать ответа» включено — письмо изменено: у вкладки точка несохранённого.
        c.update(cx, |c, cx| {
            c.model.wait = Some(postman_core::marks::WAIT_DEFAULT);
            c.toggle_later_menu(cx);
        });
        Ok(())
    }

    fn template(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        let Some(c) = m.compose.last().cloned() else { return Err("no compose tab".into()) };
        c.update(cx, |c, cx| {
            if c.later_menu_open() {
                c.toggle_later_menu(cx);
            }
            c.prefs = crate::settings_sign::screen_sample(&[]);
            let t = c.prefs.templates.first().cloned().ok_or("no template")?;
            c.use_template(t, 0, window, cx);
            Ok::<_, String>(())
        })
    }

    fn template_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn() && c.read(cx).tpl_ask.is_some())
    }

    fn send_later_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::compose_tab(m).is_some_and(|c| c.read(cx).drawn() && c.read(cx).later_menu_open())
    }

    fn checks(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::remember_composer(m, cx);
        m.load_address_book(cx);
        samples::seed_book(&m.compose.book);
        m.open_compose(samples::checks_draft(), cx);
        Ok(())
    }

    fn checks_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Новое письмо — вторая вкладка; замечания по адресатам считаются при отрисовке.
        let before = screens::composer_before(m);
        screens::compose_tab(m).is_some_and(|c| {
            let c = c.read(cx);
            Some(c.id) != before && c.drawn() && c.model.checks().count() >= 4
        })
    }

    fn confidential(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::remember_composer(m, cx);
        // Сертификат Петрова — как после открытия его подписанного письма (Ф1 запоминает подписанта).
        let cache = m.cache.clone();
        let ctx = crate::smime::demo_context();
        let trust = ctx.anchors.trust();
        if let Some(raw) = postman_demo::smime::raw("rsa") {
            let now = jiff::Timestamp::now().as_second();
            let report = postman_worker::smime::verify(raw, &trust, u64::try_from(now).unwrap_or(0));
            postman_core::smime::finish(&cache, "screens-seed-rsa", report, &ctx, now);
        }
        m.open_compose(samples::confidential_draft(), cx);
        Ok(())
    }

    fn confidential_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let before = screens::composer_before(m);
        let certs = m.compose.certs.clone();
        let known = crate::smime_seal::lock(&certs).families("i.petrov@corp.example").is_some_and(|f| !f.is_empty());
        screens::compose_tab(m).is_some_and(|c| Some(c.read(cx).id) != before && c.read(cx).drawn()) && known
    }

    fn no_cert(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        screens::remember_composer(m, cx);
        let mut d = samples::confidential_draft();
        d.to = vec![postman_compose::Address::new("Сидорова Анна", "a.sidorova@partner.example")]; // i18n-ok: образец письма
        m.open_compose(d, cx);
        Ok(())
    }

    fn no_cert_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        let before = screens::composer_before(m);
        screens::compose_tab(m).is_some_and(|c| {
            let c = c.read(cx);
            Some(c.id) != before && c.drawn() && c.model.checks().any(|k| matches!(k, postman_core::checks::Check::NoCertificate { .. }))
        })
    }

    fn reply(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        // Демо — письмо с картинкой в тексте и цитатой Outlook; живой ящик — первое письмо.
        let i = if screens::live(m) { Some(0) } else { screens::demo_row(m.list.read(cx), screens::outlook_letter) };
        let Some(i) = i else { return Err("no message to reply to".into()) };
        screens::remember_composer(m, cx);
        m.tabs.activate_kind(crate::tabs::TabKind::Main);
        m.set_section(Section::Mail, cx);
        m.list.update(cx, |l, cx| l.select(i, cx));
        // Как `mail.reply`: ответ — когда тело разобрано (ответ ждёт его сам), и не посреди отрисовки.
        let mailer = cx.entity();
        cx.defer(move |cx| mailer.update(cx, |m, cx| m.respond(postman_compose::Mode::Reply, cx)));
        Ok(())
    }

    fn reply_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        // Ответ — в области чтения над письмом (вкладки прошлых сцен ещё открыты).
        let before = screens::composer_before(m);
        let reply = m.compose.inline_view().is_some_and(|c| Some(c.read(cx).id) != before && c.read(cx).drawn());
        reply && m.tabs.active().kind.shows_mail()
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use super::{
        AUTOSAVE, Address, Attachment, Check, CloseAnswer, CloseStep, Command, Draft, Field, Format, Group, Instant, Model, Purpose, Release, Status,
        accept_suggestion, align_tables, apply_format, compose_field, current_token, group_ref, insert_table, own_end, paste_markdown, segments,
        send_later, swap_signature, table_tab,
    };
    use postman_core::contacts::{AddressBook, Source};

    /// «Переслать вложениями» (T1-17): из трёх писем — черновик с тремя `message/rfc822` по дате, с `blob_id` и
    /// именами по теме; собранное письмо (байты — как после скачивания отправкой) разбирается в три вложенных письма.
    #[test]
    fn forward_three_letters_attached() {
        let row = |id: &str, subject: &str, at: i64| postman_cache::EmailRow {
            id: id.into(),
            thread_id: id.into(),
            mailbox_ids: vec!["inbox".into()],
            received_at: at,
            subject: subject.into(),
            from_name: "Анна".into(),
            from_addr: "anna@corp.example".into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: false,
            size: 0,
            keywords: Vec::new(),
        };
        let letters = vec![
            (row("c", "Итоги", 30), Some("blob-c".to_string()), Vec::new()),
            (row("a", "Отчёт: март/апрель", 10), Some("blob-a".to_string()), Vec::new()),
            (row("b", "  ", 20), Some("blob-b".to_string()), Vec::new()),
        ];
        let mut d = super::attached_draft(letters);
        assert_eq!(d.subject, "", "несколько писем — тема пустая");
        let got: Vec<_> = d.attachments.iter().map(|a| (a.name.as_str(), a.mime.as_str(), a.blob_id.as_deref(), a.inline_cid.is_none())).collect();
        assert_eq!(
            got,
            [
                ("Отчёт_ март_апрель.eml", "message/rfc822", Some("blob-a"), true),
                ("message.eml", "message/rfc822", Some("blob-b"), true),
                ("Итоги.eml", "message/rfc822", Some("blob-c"), true),
            ]
        );
        let one = super::attached_draft(vec![(row("a", "Отчёт", 10), Some("blob-a".into()), Vec::new())]);
        assert_eq!(one.subject, "FW: Отчёт");
        // Отправка скачала письма по `blob_id`: байты — настоящие письма RFC 5322.
        let me = Address::new("Я", "me@corp.example");
        for (i, a) in d.attachments.iter_mut().enumerate() {
            let inner = Draft {
                from: Address::new("Анна", "anna@corp.example"),
                to: vec![me.clone()],
                subject: format!("Письмо {i}"),
                markdown: "текст".into(),
                ..Default::default()
            };
            a.data = postman_compose::build(&inner, 10, &format!("m{i}@corp.example")).unwrap();
        }
        d.from = me.clone();
        d.to = vec![Address::new("", "boss@corp.example")];
        let raw = postman_compose::build(&d, 100, "out@corp.example").unwrap();
        let m = postman_worker::mime::parse(&raw).unwrap();
        assert_eq!(m.parts.iter().map(|p| p.mime.as_str()).collect::<Vec<_>>(), ["message/rfc822"; 3]);
        for (i, p) in m.parts.iter().enumerate() {
            let inner = postman_worker::mime::part(&raw, p.index).unwrap();
            let inner = postman_worker::mime::parse(&inner).unwrap();
            assert_eq!(inner.message_id.as_deref(), Some(format!("m{i}@corp.example").as_str()), "вложенное письмо {i}");
            assert!(inner.text.as_deref().is_some_and(|t| t.contains("текст")));
        }
    }

    fn model() -> Model {
        Model::new(Draft::default(), Some(Address::new("Я", "me@corp.example")), "", Instant::now())
    }

    fn fill(m: &mut Model) {
        m.set(Field::To, "Анна <anna@corp.example>; petr@corp.example".into());
        m.set(Field::Cc, "ivan@corp.example".into());
        m.set(Field::Bcc, "boss@corp.example".into());
        m.set(Field::Subject, "Отчёт".into());
        m.set(Field::Body, "Привет, **Анна**!".into());
    }

    #[test]
    fn fields_make_a_draft_and_bcc_stays_in_the_envelope() {
        let mut m = model();
        fill(&mut m);
        let d = m.draft(true).unwrap();
        assert_eq!(d.from, Address::new("Я", "me@corp.example"));
        assert_eq!(d.to, vec![Address::new("Анна", "anna@corp.example"), Address::new("", "petr@corp.example")]);
        assert_eq!(d.cc, vec![Address::new("", "ivan@corp.example")]);
        assert_eq!(d.bcc, vec![Address::new("", "boss@corp.example")]);
        assert_eq!((d.subject.as_str(), d.markdown.as_str()), ("Отчёт", "Привет, **Анна**!"));
        // Скрытая копия — в конверте отправки, но не в заголовках письма.
        let envelope: Vec<&str> = d.recipients().map(|a| a.email.as_str()).collect();
        assert!(envelope.contains(&"boss@corp.example"));
        let raw = postman_compose::build(&d, 0, "x@corp.example").unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("boss@corp.example"));
    }

    #[test]
    fn send_and_save_go_through_tickets() {
        let mut m = model();
        fill(&mut m);
        let Some(Command::Send { ticket: 7, replace: None, .. }) = m.begin(Purpose::Send, 7, Instant::now()) else { panic!("send command expected") };
        assert!(m.busy());
        assert!(!m.on_sent(8), "чужой тикет");
        assert!(m.on_sent(7));
        assert!(!m.is_dirty() && !m.busy());
    }

    /// S5-06: обычная отправка — с окном «Отменить» (сервер держит письмо), «Отправить позже» — к сроку; отменённое
    /// письмо снова черновик на сервере — следующая отправка заменяет его.
    #[test]
    fn send_holds_for_undo_and_send_later_goes_at_the_time() {
        let mut m = model();
        fill(&mut m);
        let Some(Command::Send { release, .. }) = m.begin(Purpose::Send, 1, Instant::now()) else { panic!("send") };
        assert_eq!(release, Release::After(send_later::UNDO_HOLD_SECS));
        assert!(!m.sending_later());
        // CMF-03: задержка 0 — письмо уходит сразу, без окна «Отменить»; 30 — сервер держит 30 с и запас.
        let mut now = model();
        fill(&mut now);
        now.undo_secs = 0;
        let Some(Command::Send { release, .. }) = now.begin(Purpose::Send, 9, Instant::now()) else { panic!("send") };
        assert_eq!(release, Release::Now);
        let mut long = model();
        fill(&mut long);
        long.undo_secs = 30;
        let Some(Command::Send { release, .. }) = long.begin(Purpose::Send, 10, Instant::now()) else { panic!("send") };
        assert_eq!(release, Release::After(33));
        assert!(m.on_sent(1));
        m.set_server_id("draft-1".into());
        let Some(Command::Send { release, replace, .. }) = m.begin(Purpose::SendLater(1_792_562_400), 2, Instant::now()) else {
            panic!("send later")
        };
        assert_eq!((release, replace.as_deref()), (Release::At(1_792_562_400), Some("draft-1")));
        assert!(m.sending_later());
        // Без получателей не планируется, как и не отправляется.
        let mut empty = model();
        assert!(empty.begin(Purpose::SendLater(1), 3, Instant::now()).is_none());
        assert_eq!(empty.status, Status::NoRecipients);
    }

    #[test]
    fn dirty_and_clean() {
        let mut m = model();
        assert!(!m.is_dirty(), "новое письмо не грязное");
        assert!(m.request_close(), "чистое закрывается сразу");
        m.set(Field::Subject, "Тема".into());
        assert!(m.is_dirty());
        // Сохранено на сервере — снова чистое, следующее сохранение заменит эту версию.
        let Some(Command::SaveDraft { .. }) = m.begin(Purpose::Save, 1, Instant::now()) else { panic!("save expected") };
        assert!(!m.on_saved(1, "draft-1".into(), "12:00".into()) && !m.is_dirty());
        m.set(Field::Body, "текст".into());
        let Some(Command::SaveDraft { replace: Some(id), .. }) = m.begin(Purpose::Save, 2, Instant::now()) else { panic!("save expected") };
        assert_eq!(id, "draft-1");
        // Демо: сервера нет — текст остаётся, письмо грязное.
        m.refused();
        assert!(m.is_dirty() && !m.busy());
        assert_eq!(m.status, Status::NoServer(Purpose::Save));
        assert_eq!(m.body, "текст");
        // Вложение — тоже изменение; метка письма — тоже.
        let mut m = model();
        m.add_attachment(Attachment { name: "a.txt".into(), mime: "text/plain".into(), data: b"x".to_vec(), inline_cid: None, blob_id: None });
        assert!(m.is_dirty());
        m.remove_attachment(0);
        assert!(!m.is_dirty());
        m.confidential = true;
        assert!(m.is_dirty());
    }

    #[test]
    fn closing_with_changes_asks_in_the_letter() {
        let mut m = model();
        m.set(Field::Body, "текст".into());
        assert!(!m.request_close() && m.confirm_close);
        assert_eq!(m.answer_close(CloseAnswer::Cancel), CloseStep::Stay);
        assert!(!m.confirm_close);
        m.request_close();
        assert_eq!(m.answer_close(CloseAnswer::Save), CloseStep::Save);
        m.begin(Purpose::Save, 3, Instant::now());
        assert!(m.on_saved(3, "d".into(), "12:00".into()), "после «Да» письмо закрывается, когда черновик сохранён");
        m.set(Field::Body, "ещё".into());
        m.request_close();
        assert_eq!(m.answer_close(CloseAnswer::Discard), CloseStep::Close);
    }

    #[test]
    fn bad_address_is_reported() {
        let mut m = model();
        m.set(Field::To, "anna@corp.example, мусор; petr@corp".into());
        assert_eq!(m.bad(Field::To), ["мусор", "petr@corp"]);
        assert!(m.bad(Field::Cc).is_empty());
        assert_eq!(m.draft(false).unwrap_err(), Status::BadAddresses(vec!["мусор".into(), "petr@corp".into()]));
        assert!(m.begin(Purpose::Send, 1, Instant::now()).is_none());
        assert!(!m.busy());
        let mut m = model();
        m.set(Field::Subject, "без адресатов".into());
        assert_eq!(m.draft(true).unwrap_err(), Status::NoRecipients);
        assert!(m.draft(false).is_ok(), "черновик без адресатов сохраняется");
    }

    #[test]
    fn autosave_decision() {
        let t0 = Instant::now();
        let mut m = Model::new(Draft::default(), None, "", t0);
        assert_eq!(m.autosave_wait(t0), None, "без изменений — не нужно");
        m.set(Field::Body, "текст".into());
        assert_eq!(m.autosave_wait(t0), Some(AUTOSAVE));
        assert!(!m.should_autosave(t0 + AUTOSAVE / 2));
        assert!(m.should_autosave(t0 + AUTOSAVE));
        let t1 = t0 + AUTOSAVE;
        m.begin(Purpose::Autosave, 5, t1);
        assert_eq!(m.autosave_wait(t1), None, "идёт сохранение");
        m.refused();
        assert_eq!(m.autosave_wait(t1 + AUTOSAVE), None, "тот же текст не сохранился — не повторяем");
        m.set(Field::Body, "текст 2".into());
        assert!(m.should_autosave(t1 + AUTOSAVE), "текст изменился — снова");
        m.begin(Purpose::Autosave, 6, t1 + AUTOSAVE);
        m.on_failed(6, "сбой".into());
        assert_eq!(m.status, Status::Failed("сбой".into()));
        assert_eq!(m.autosave_wait(t1 + AUTOSAVE * 3), None);
        m.set(Field::Body, "текст 3".into());
        m.request_close();
        assert_eq!(m.autosave_wait(t1 + AUTOSAVE * 3), None, "пока открыт вопрос о закрытии");
    }

    #[test]
    fn signature_goes_into_an_untouched_new_message() {
        let m = Model::new(Draft::default(), None, "С уважением,\nЯ", Instant::now());
        assert_eq!(m.body, "\n\nС уважением,\nЯ");
        assert!(!m.is_dirty());
        let mut m = model();
        assert!(m.set_identity(Address::new("Я", "me@corp.example"), "Подпись"));
        assert!(!m.is_dirty() && m.body == "\n\nПодпись");
        m.set(Field::Body, "моё".into());
        assert!(!m.set_identity(Address::new("Я", "me@corp.example"), "Подпись"), "набранный текст не трогаем");
    }

    /// Подпись по умолчанию не считается правкой: иначе закрытие пустого письма спрашивало бы «Сохранить?».
    #[test]
    fn default_signature_keeps_a_new_message_clean() {
        let mut m = model();
        m.set_default_signature("С уважением");
        assert!(m.body.contains("С уважением") && !m.is_dirty() && m.request_close());
        let mut m = model();
        m.set(Field::Subject, "Тема".into());
        m.set_default_signature("С уважением");
        assert!(m.is_dirty(), "свои правки остаются правками");
    }

    /// S5-06б: подпись меняется на месте; в ответе новая встаёт перед цитатой.
    #[test]
    fn signature_is_swapped_in_place() {
        assert_eq!(swap_signature("Текст\n\nСтарая", "Старая", "Новая\nстрока"), "Текст\n\nНовая\nстрока");
        assert_eq!(swap_signature("Текст\n\nСтарая", "Старая", ""), "Текст");
        assert_eq!(swap_signature("Текст", "", "Новая"), "Текст\n\nНовая");
        let reply = "Ответ\n\n17.10.2026, Анна пишет:\n> привет\n";
        assert_eq!(own_end(reply), "Ответ\n\n".len());
        assert_eq!(swap_signature(reply, "", "Подпись"), "Ответ\n\nПодпись\n\n17.10.2026, Анна пишет:\n> привет\n");
        // Подпись в цитате не трогаем.
        assert_eq!(swap_signature("Ок\n\nА пишет:\n> Старая\n", "Старая", "Новая"), "Ок\n\nНовая\n\nА пишет:\n> Старая\n");
        let mut m = Model::new(Draft::default(), None, "Старая", Instant::now());
        m.set_signature("Новая");
        assert_eq!((m.body.as_str(), m.signature.as_str()), ("\n\nНовая", "Новая"));
    }

    /// T3-07: картинка подписи, на которую ссылается текст, — вложение inline (не в списке вложений, не «изменение»
    /// нетронутого письма); сменили подпись — картинка в письмо не уходит.
    #[test]
    fn signature_image_goes_inline() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];
        let img = postman_core::prefs::SignImage::new("logo.png", png.clone(), 7).unwrap();
        let sig = format!("Иван\n{}", img.markdown());
        let mut m = Model::new(Draft::default(), None, "", Instant::now());
        m.set_default_signature(&sig);
        assert!(m.attach_inline(std::iter::once(&img)));
        assert!(!m.attach_inline(std::iter::once(&img)), "второй раз не прикладывается");
        assert!(!m.is_dirty(), "нетронутое письмо с подписью закрывается без вопроса");
        assert_eq!(m.attachments().count(), 0, "в списке вложений картинки нет");
        assert_eq!(m.inline_images().len(), 1);
        m.set(Field::To, "ivan@corp.example; ".into());
        let d = m.draft(true).unwrap();
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].inline_cid.as_deref(), Some("sig7@postman"));
        assert_eq!(d.attachments[0].data, png);
        m.set_signature("Без картинки");
        assert!(m.draft(true).unwrap().attachments.is_empty(), "ссылки нет — картинка не уходит");
        assert!(m.inline_images().is_empty());
    }

    /// T3-06: «Таблица» — пустая таблица GFM с новой строки, выделена первая ячейка; HTML письма — таблица.
    #[test]
    fn table_is_inserted_at_cursor() {
        let (t, sel) = insert_table("Итоги", "Итоги".len(), 3, 2);
        assert_eq!(t, "Итоги\n\n|     |     |     |\n| --- | --- | --- |\n|     |     |     |\n|     |     |     |\n");
        assert_eq!(&t[sel.clone()], "   ");
        assert_eq!(sel.start, "Итоги\n\n| ".len());
        let (t, _) = insert_table("", 0, 2, 1);
        assert_eq!(t, "|     |     |\n| --- | --- |\n|     |     |\n");
        let (t, _) = insert_table("до\nпосле", "до\n".len(), 1, 1);
        assert_eq!(t, "до\n\n|     |\n| --- |\n|     |\n\nпосле");
        let html = postman_ir::markdown_to_html(&insert_table("", 0, 2, 1).0);
        assert!(html.contains("<table>") && html.contains("<td>"), "{html}");
    }

    /// T3-06: Tab — к следующей ячейке (столбцы выравниваются по набранному), в последней — новая строка; Shift+Tab —
    /// назад; вне таблицы — не наше.
    #[test]
    fn tab_moves_between_cells() {
        let (t, sel) = insert_table("", 0, 2, 1);
        let t = format!("{}Город{}", &t[..sel.start], &t[sel.end..]);
        let (t, sel) = table_tab(&t, sel.start + "Город".len(), false).unwrap();
        assert_eq!(t, "| Город |     |\n| ----- | --- |\n|       |     |\n");
        assert_eq!(&t[sel.clone()], "   ", "вторая ячейка заголовка");
        let (back, bsel) = table_tab(&t, sel.start, true).unwrap();
        assert_eq!(&back[bsel], "Город", "Shift+Tab — назад, набранное выделено");
        // Из последней ячейки — новая строка.
        let last = t.rfind("|     |").unwrap() + 2;
        let (t2, sel2) = table_tab(&t, last, false).unwrap();
        assert_eq!(t2.lines().count(), 4);
        assert_eq!(sel2.start, t2.lines().take(3).map(|l| l.len() + 1).sum::<usize>() + 2);
        assert!(table_tab(&t, 0, true).is_none(), "Shift+Tab в первой ячейке — не наше");
        assert!(table_tab("просто текст", 3, false).is_none());
        assert!(table_tab("| a | b |\nне таблица", 2, false).is_none(), "без строки выравнивания — не таблица");
    }

    /// T3-06, T3-07: таблицы и ссылки на картинки текста — для графики поля (байты, строки, ячейки).
    #[test]
    fn tables_and_image_refs_for_the_field() {
        let text = "Текст\n\n| a | b |\n| - | - |\n| c | d |\n\nИван\n![logo.png](cid:sig1@postman)\nсм. ![x](cid:y) тут";
        let t = super::tables(text);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].lines, 2..5);
        assert_eq!(&text[t[0].bytes.clone()], "| a | b |\n| - | - |\n| c | d |");
        assert_eq!(t[0].rows, vec![vec!["a".to_owned(), "b".to_owned()], vec!["c".to_owned(), "d".to_owned()]]);
        let r = super::image_refs(text);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].line, r[0].cid.as_str(), r[0].alone), (7, "sig1@postman", true));
        assert_eq!(&text[r[0].bytes.clone()], "![logo.png](cid:sig1@postman)");
        assert_eq!((r[1].line, r[1].cid.as_str(), r[1].alone), (8, "y", false));
        assert!(super::tables("| a |\nне таблица").is_empty());
    }

    /// T3-06: вставка из Word — таблица (выровнена), список, жирный, ссылка, заголовок; картинки и прочее — нет.
    #[test]
    fn pasted_html_becomes_markdown() {
        let html = r#"<h2>Отчёт</h2><p><b>Важно</b> и <i>срочно</i>, <a href="https://corp.example/r">ссылка</a></p>
            <ul><li>один</li><li>два</li></ul>
            <table><tr><th>Город</th><th>План</th></tr><tr><td>Омск</td><td>10</td></tr></table>
            <p><img src="cid:x"><span style="font-size:20pt;font-family:Arial">шрифт</span></p>"#;
        let md = paste_markdown(postman_ir::from_html(html));
        assert!(md.contains("## Отчёт"), "{md}");
        assert!(md.contains("**Важно**") && md.contains("*срочно*") && md.contains("[ссылка](https://corp.example/r)"), "{md}");
        assert!(md.contains("- один") && md.contains("- два"), "{md}");
        assert!(md.contains("| Город | План |\n| ----- | ---- |\n| Омск  | 10   |"), "{md}");
        assert!(!md.contains("cid:") && md.contains("шрифт"), "{md}");
        assert_eq!(align_tables("текст\n|a|bb|\n|-|-|\n|ccc|d|\nконец"), "текст\n| a   | bb  |\n| --- | --- |\n| ccc | d   |\nконец");
    }

    /// S5-06б: кнопки панели форматирования вставляют разметку вокруг выделения или в начало строк.
    #[test]
    fn toolbar_inserts_markdown() {
        let t = "раз два три";
        let two = "раз ".len().."раз два".len();
        assert_eq!(apply_format(t, two.clone(), Format::Bold), ("раз **два** три".into(), two.start + 2..two.end + 2));
        assert_eq!(apply_format(t, two.clone(), Format::Italic).0, "раз *два* три");
        assert_eq!(apply_format(t, two.clone(), Format::Underline).0, "раз <u>два</u> три");
        let (s, sel) = apply_format(t, two.clone(), Format::Link);
        assert_eq!(s, "раз [два](https://) три");
        assert_eq!(&s[sel], "https://");
        // Пустое выделение — пара разметки, курсор внутри.
        assert_eq!(apply_format("ab", 1..1, Format::Bold), ("a****b".into(), 3..3));
        let (s, sel) = apply_format("", 0..0, Format::Link);
        assert_eq!((s.as_str(), &s[sel]), ("<https://>", "https://"));
        // Строки: маркеры, номера, цитата; повторное нажатие снимает.
        let lines = "один\nдва\nтри";
        let all = 0..lines.len();
        assert_eq!(apply_format(lines, all.clone(), Format::Bullets).0, "- один\n- два\n- три");
        assert_eq!(apply_format(lines, all.clone(), Format::Numbers).0, "1. один\n2. два\n3. три");
        assert_eq!(apply_format(lines, 0.."один\nд".len(), Format::Quote).0, "> один\n> два\nтри");
        assert_eq!(apply_format("- один\n- два", 0..3, Format::Bullets).0, "один\n- два");
        let (s, sel) = apply_format("x\n", 2..2, Format::Bullets);
        assert_eq!((s.as_str(), sel), ("x\n- ", 4..4));
    }

    #[test]
    fn address_field_splits_into_chips_and_the_typed_address() {
        assert_eq!(segments("Анна <a@x.ru>; b@x.ru; ив"), (vec!["Анна <a@x.ru>".into(), "b@x.ru".into()], "ив".into()));
        assert_eq!(segments("\"Петров, Иван\" <p@x.ru>, "), (vec!["\"Петров, Иван\" <p@x.ru>".into()], String::new()));
        assert_eq!(segments(""), (vec![], String::new()));
        assert_eq!(compose_field(&["a@x.ru".into(), "b@x.ru".into()], "в"), "a@x.ru; b@x.ru; в");
        let m = Model::new(
            Draft { to: vec![Address::new("Анна", "a@x.ru"), Address::new("", "b@x.ru")], ..Default::default() },
            None,
            "",
            Instant::now(),
        );
        assert_eq!(segments(&m.to), (vec!["Анна <a@x.ru>".into(), "b@x.ru".into()], String::new()), "из черновика — всё чипы");
    }

    #[test]
    fn suggestion_replaces_the_typed_address() {
        assert_eq!(current_token("anna@x.ru; ив"), "ив");
        assert_eq!(current_token("ив"), "ив");
        assert_eq!(accept_suggestion("anna@x.ru; ив", "Иван <ivan@corp.example>"), "anna@x.ru; Иван <ivan@corp.example>; ");
        assert_eq!(accept_suggestion("ив", "Иван <ivan@corp.example>"), "Иван <ivan@corp.example>; ");
    }

    /// S5-06б: метка, ожидание ответа и категория — ключевыми словами черновика; вернувшийся черновик — те же поля.
    #[test]
    fn marks_become_draft_keywords_and_come_back() {
        let mut m = model();
        fill(&mut m);
        m.confidential = true;
        m.wait = Some(3);
        m.category = Some("pm.cat.clients".into());
        let today: jiff::civil::Date = "2026-10-21".parse().unwrap();
        assert_eq!(m.keywords(today), ["pm.cat.clients", "pm.conf", "pm.wait", "pm.wait.2026-10-24"]);
        let d = m.draft(false).unwrap();
        assert_eq!(d.keywords.len(), 4);
        let back =
            Model::new(Draft { keywords: [d.keywords.clone(), vec!["$label9".into(), "other".into()]].concat(), ..d }, None, "", Instant::now());
        assert!(back.confidential);
        assert_eq!((back.wait, back.category.as_deref()), (Some(3), Some("pm.cat.clients")));
        assert!(back.keywords(today).contains(&"other".to_string()), "чужие слова сохраняются");
    }

    /// S5-06б: проверки текста — подсказки, отправку не держат; DLP в клиенте нет (решение владельца 2026-11-11).
    #[test]
    fn text_checks_do_not_block_sending() {
        let mut m = model();
        fill(&mut m);
        m.set(Field::Body, "Карта 4111 1111 1111 1111. Договор во вложении.".into());
        assert!(matches!(m.checks().collect::<Vec<_>>().as_slice(), [Check::MissingAttachment { .. }]), "номер карты — не замечание");
        assert!(!m.blocked());
        assert!(m.begin(Purpose::Send, 1, Instant::now()).is_some());
    }

    #[test]
    fn recipient_checks_and_address_replacement() {
        let mut book = AddressBook::default();
        book.touch("Ирина Смирнова", "i.smirnova@partner.example", Source::Recent, 1, 1_000);
        book.touch("Ирина Смирнова", "irina.s@partner.example", Source::Recent, 1, 2_000);
        let mut m = model();
        m.set(Field::To, "Анна <anna@corp.example>; i.smirnova@partner.example; ".into());
        let own = ["corp.example".to_string()];
        m.refresh_recipient_checks(&own, &book);
        let moved: Vec<&Check> = m.checks().collect();
        assert!(matches!(moved.as_slice(), [Check::Moved { .. }]));
        m.confidential = true;
        m.refresh_recipient_checks(&own, &book);
        assert!(matches!(m.checks().next(), Some(Check::External { count: 1, .. })));
        assert!(m.replace_address("i.smirnova@partner.example", "irina.s@partner.example"));
        assert_eq!(m.to, "Анна <anna@corp.example>; irina.s@partner.example; ");
        m.refresh_recipient_checks(&own, &book);
        assert_eq!(m.checks().count(), 1, "осталось только «внешние»");
    }

    /// Личная группа (T2-47): чип `"Имя" <group:N>` в «Кому»; письмо (и черновик) уходит участникам, повтор адреса —
    /// один раз; «+» — участники чипами (кто уже в поле — не повторяется); нетронутое письмо с группой — не «изменено»;
    /// Backspace по чипу группы его убирает, а не возвращает текст.
    #[test]
    fn personal_group_expands_to_members() {
        let g = Group {
            name: "Руководители, отделы".into(),
            members: vec![Address::new("Анна", "anna@corp.example"), Address::new("", "boris@corp.example")],
        };
        let mut m = Model::new(Draft::default(), Some(Address::new("Я", "me@corp.example")), "", Instant::now());
        m.add_group_to(Field::To, g.clone());
        assert!(!m.is_dirty(), "письмо из «Контактов» не спрашивает о сохранении");
        let (closed, _) = segments(&m.to);
        assert_eq!(closed.len(), 1);
        assert_eq!(group_ref(&closed[0]), Some(0));
        assert_eq!(m.group(&closed[0]), Some(&g));
        assert!(m.bad(Field::To).len() == 1, "сам по себе чип — не адрес; отправка его раскрывает");
        m.set(Field::To, format!("{}anna@corp.example; ", m.to));
        let d = m.draft(true).unwrap();
        let to: Vec<&str> = d.to.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(to, ["anna@corp.example", "boris@corp.example"], "участники; Анна — один раз");
        assert_eq!(d.to[0].name, "Анна");
        assert_eq!(m.recipients().len(), 2);
        assert_eq!(m.add_group(g.clone()), closed[0], "та же группа — тот же чип");
        // «+»: участники — чипами, Анна уже есть.
        assert!(m.expand_group(Field::To, 0));
        assert_eq!(m.to, "boris@corp.example; anna@corp.example; ", "Анна уже была в поле");
        assert!(!m.expand_group(Field::To, 0), "уже не группа");
        assert!(group_ref("Анна <anna@corp.example>").is_none());
    }
}
