//! Вложения беседы новым участникам (решение владельца 2026-11-13, `docs/decisions/2026-11-13-reply-all-attachments.md`):
//! участники и файлы беседы ответа — из кэша, в фоне (`Mailer::share_for`). Кто новый и копии письма —
//! `postman_compose::share`; строка над текстом черновика — `composer.rs`.
//!
//! Файлы сравниваются по содержимому: крупные части письма — по SHA-256, который хранилище уже посчитало
//! (`Cache::letter_parts`, ADR 0006); остальные — по хэшу байтов из кэша, если они есть; иначе — blob, размер и имя.

use gpui_kit::*;
use pets_i18n::tr;
use postman_cache::{AttachmentRow, Cache, EmailRow, Hash};
use postman_compose::share::{File, Letter, collect};
use postman_compose::{Attachment, Share};

use crate::Mailer;

/// Файл беседы для новых участников: не картинка текста (подпись, логотип), не приглашение, не подпись и не
/// конверт S/MIME, не пустой.
fn shareable(a: &AttachmentRow) -> bool {
    let mime = a.mime.to_ascii_lowercase();
    let smime = mime.contains("pkcs7") || a.name.to_ascii_lowercase().ends_with(".p7s");
    !(a.inline && a.cid.is_some()) && !crate::attachments::is_invite(a) && !smime && a.size > 0
}

/// SHA-256 файла: часть содержимого письма того же размера (одна) — её хэш из хранилища; иначе байты из кэша
/// (`local` — демо); байтов нет — `None`.
fn hash_of(a: &AttachmentRow, parts: &[(Hash, u64)], cached: impl Fn(&str) -> Option<Vec<u8>>) -> Option<[u8; 32]> {
    if a.size >= postman_cache::MIN_PART as u64 {
        let mut same = parts.iter().filter(|(_, size)| *size == a.size);
        if let (Some((h, _)), None) = (same.next(), same.next()) {
            return Some(h.0);
        }
    }
    cached(&a.blob_id).filter(|b| !b.is_empty()).map(|b| Hash::of(&b).0)
}

/// Письма беседы `thread_id` (без черновиков и самого черновика `skip`) — участники и файлы — и вложения беседы
/// для черновика с вложениями `carried`. Файлов нет — `None`.
pub(crate) fn gather(
    cache: &Cache,
    thread_id: &str,
    skip: Option<&str>,
    carried: &[Attachment],
    note: String,
    local: impl Fn(&str) -> Option<Vec<u8>>,
) -> Option<Share> {
    let rows = cache.thread(thread_id).ok()?;
    let letters: Vec<Letter> = rows
        .iter()
        .filter(|r| Some(r.id.as_str()) != skip && !r.has_keyword(postman_core::labels::DRAFT))
        .map(|r| letter(cache, r, &local))
        .collect();
    collect(&letters, carried, note)
}

fn letter(cache: &Cache, row: &EmailRow, local: &impl Fn(&str) -> Option<Vec<u8>>) -> Letter {
    let mut people = vec![row.from_addr.clone()];
    if let Ok(r) = cache.recipients(&row.id) {
        people.extend(r.to.into_iter().chain(r.cc));
    }
    let body = cache.body(&row.id).ok().flatten();
    if let Some(b) = &body {
        people.extend(b.to.iter().chain(&b.cc).map(|a| a.email.clone()));
    }
    let (scope, id) = postman_cache::unscoped(&row.id);
    let parts = cache.letter_of(scope.unwrap_or(""), id).ok().flatten().and_then(|l| cache.letter_parts(l).ok()).unwrap_or_default();
    let cached = |blob: &str| local(blob).or_else(|| cache.blob(blob).ok().flatten());
    let files = body
        .iter()
        .flat_map(|b| &b.attachments)
        .filter(|a| shareable(a))
        .map(|a| File { name: a.name.clone(), mime: a.mime.clone(), blob_id: a.blob_id.clone(), size: a.size, hash: hash_of(a, &parts, cached) })
        .collect();
    Letter { people, files }
}

impl Mailer {
    /// Вложения беседы для ответа на письмо `row` (в фоне): готово — в окно ответа (`in_reply_to` — его
    /// черновика). Пересылка — стандартная, без правила.
    pub(crate) fn share_for(&mut self, row: &EmailRow, draft: &postman_compose::Draft, cx: &mut Context<Self>) {
        let Some(in_reply_to) = draft.in_reply_to.clone() else { return };
        let (cache, thread, carried) = (self.cache.clone(), row.thread_id.clone(), draft.attachments.clone());
        let note = tr!("compose-share-letter");
        let demo = !self.backend.is_live();
        let task = cx.background_spawn(async move {
            let local = |blob: &str| if demo { postman_demo::blob(blob) } else { None };
            let mut share = gather(&cache, &thread, None, &carried, note, local)?;
            // Демо: байты сразу, как у пересылки.
            if demo {
                for a in &mut share.attachments {
                    a.data = a.blob_id.as_deref().and_then(postman_demo::blob).unwrap_or_default();
                }
            }
            Some(share)
        });
        cx.spawn(async move |this, cx| {
            let Some(share) = task.await else { return };
            let _ = this.update(cx, |m, cx| m.deliver_share(&in_reply_to, share, cx));
        })
        .detach();
    }

    /// Вложения беседы — в открытое окно ответа на это письмо, если у него их ещё нет.
    fn deliver_share(&mut self, in_reply_to: &str, share: Share, cx: &mut Context<Self>) {
        for o in &self.compose.open {
            let fits = o.view.read(cx).model.wants_share(in_reply_to);
            if fits {
                o.view.update(cx, |c, cx| {
                    c.model.set_share(share);
                    cx.notify();
                });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{gather, hash_of};
    use postman_cache::{Addr, AttachmentRow, BodyRow, Cache, EmailRow, Hash, Recipients};

    fn row(id: &str, from: &str, at: i64) -> EmailRow {
        EmailRow {
            id: id.into(),
            thread_id: "t1".into(),
            mailbox_ids: vec!["inbox".into()],
            received_at: at,
            subject: "Отчёт".into(),
            from_name: String::new(),
            from_addr: from.into(),
            preview: String::new(),
            seen: true,
            flagged: false,
            has_attachment: true,
            size: 0,
            keywords: Vec::new(),
        }
    }

    fn file(blob: &str, name: &str, size: u64, cid: Option<&str>) -> AttachmentRow {
        AttachmentRow {
            blob_id: blob.into(),
            name: name.into(),
            mime: "application/pdf".into(),
            size,
            cid: cid.map(Into::into),
            inline: cid.is_some(),
        }
    }

    /// Беседа из кэша: участники — «От» строки, получатели и адреса тела; файлы — без картинок текста, приглашений,
    /// подписи S/MIME и черновиков; одинаковые байты в разных письмах — один файл.
    #[test]
    fn thread_participants_and_files_from_the_cache() {
        let cache = Cache::open_in_memory().unwrap();
        let mut draft = row("d", "me@x.example", 4);
        draft.keywords = vec![postman_core::labels::DRAFT.into()];
        cache.upsert_emails(&[row("m1", "ivan@x.example", 1), row("m2", "petr@x.example", 2), row("m3", "ivan@x.example", 3), draft]).unwrap();
        cache.set_recipients(&[("m1".into(), Recipients { to: vec!["me@x.example".into()], cc: vec!["petr@x.example".into()] })]).unwrap();
        let addr = |e: &str| Addr { name: String::new(), email: e.into() };
        let body =
            |to: &[&str], attachments: Vec<AttachmentRow>| BodyRow { to: to.iter().map(|e| addr(e)).collect(), attachments, ..Default::default() };
        let mut invite = file("B-ics", "invite.ics", 10, None);
        invite.mime = "text/calendar".into();
        let mut p7s = file("B-sig", "smime.p7s", 10, None);
        p7s.mime = "application/pkcs7-signature".into();
        cache.put_body("m1", &body(&[], vec![file("B1", "Отчёт.pdf", 4, None), file("B-logo", "logo.png", 9, Some("logo@x")), invite, p7s])).unwrap();
        // Тот же отчёт в ответе Петра — другой blob, те же байты.
        cache.put_body("m2", &body(&["anna@x.example"], vec![file("B7", "Отчёт (2).pdf", 4, None), file("B2", "Смета.xlsx", 5, None)])).unwrap();
        cache.put_body("d", &body(&["olga@x.example"], vec![file("B9", "Черновик.pdf", 3, None)])).unwrap();
        cache.put_blob("B1", b"%PDF").unwrap();
        cache.put_blob("B7", b"%PDF").unwrap();

        let share = gather(&cache, "t1", None, &[], "note".into(), |_| None).unwrap();
        assert_eq!(share.known, ["ivan@x.example", "me@x.example", "petr@x.example", "anna@x.example"], "Ольга — только в черновике");
        let files: Vec<_> = share.attachments.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(files, ["Отчёт.pdf", "Смета.xlsx"]);
        assert_eq!(share.note, "note");
    }

    /// Крупная часть — по хэшу из хранилища (тот же размер), а не по байтам; мелкая — по байтам из кэша.
    #[test]
    fn large_parts_use_the_stored_hash() {
        let big = postman_cache::MIN_PART as u64 + 10;
        let stored = [(Hash([7; 32]), big), (Hash([8; 32]), 5)];
        assert_eq!(hash_of(&file("B", "a", big, None), &stored, |_| panic!("байты крупной части не читаются")), Some([7; 32]));
        // Две части того же размера — не угадываем: байты, а нет их — без хэша.
        let twice = [(Hash([7; 32]), big), (Hash([9; 32]), big)];
        assert_eq!(hash_of(&file("B", "a", big, None), &twice, |_| None), None);
        assert_eq!(hash_of(&file("B", "a", 4, None), &stored, |_| Some(b"%PDF".to_vec())), Some(Hash::of(b"%PDF").0));
    }
}
