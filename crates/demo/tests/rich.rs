//! Письма демо целиком (`postman_demo::rich`): воркер режет их так же, как их части описывает демо, хранилище
//! собирает ровно те же байты, логотип организации — одна запись на все её письма.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // тест: «не падать» — для кода приложения (S2-03)

use postman_cache::{Cache, FolderRow, LetterRow, Owner};
use postman_demo::rich;

const FROM: (&str, &str) = ("Иванова Мария", "m.ivanova@corp.example");

#[test]
fn rich_letters_split_like_the_worker() {
    for i in [0, 7, 10, 21, 49, 70, 123, 4900] {
        let l = rich::letter(i, FROM, "Отчёт за квартал", 1_760_000_000);
        let (split, decoded) = postman_worker::mime::split(&l.raw).unwrap();
        let ours: Vec<_> = l.parts.iter().filter(|p| p.range.len() >= postman_worker::mime::MIN_SPLIT_PART).collect();
        assert_eq!(split.parts.len(), ours.len(), "письмо {i}: {:?}", split.parts);
        let mut at = 0;
        for (w, o) in split.parts.iter().zip(ours) {
            assert_eq!((w.range.start as usize..w.range.end as usize, w.mime.as_str()), (o.range.clone(), o.mime.as_str()), "письмо {i}");
            assert!(decoded[at..at + w.decoded_len as usize] == o.decoded[..], "письмо {i}: {}", o.mime);
            at += w.decoded_len as usize;
        }
        assert!(split.message_id.is_some());
    }
}

#[test]
fn rich_letters_are_stored_byte_exact_with_one_logo_per_org() {
    let c = Cache::open_in_memory().unwrap();
    let f = c
        .upsert_folder(&FolderRow { owner: Owner::Account(String::new()), parent: None, name: "in".into(), role: None, remote: Some("in".into()) })
        .unwrap();
    let mut letters = Vec::new();
    for i in (0..2000).step_by(rich::EVERY) {
        let id = c.upsert_remote("", &format!("demo{i}"), &LetterRow { folders: vec![f], ..LetterRow::default() }).unwrap();
        letters.push((id, rich::letter(i, FROM, "Тема", 1_760_000_000 + i as i64)));
    }
    let items: Vec<_> = letters.iter().map(|(id, l)| (*id, l.raw.as_slice(), l.parts.as_slice())).collect();
    c.put_contents(&items).unwrap();
    for (id, l) in &letters {
        assert!(c.letter_content(*id).unwrap().unwrap() == l.raw);
    }
    assert_eq!(c.check_refs().unwrap(), Vec::<String>::new());
    let st = c.store_stats().unwrap();
    // Логотип организации — одна запись на все её письма: дедупликация не меньше повторов логотипов.
    let mut logos = vec![0u64; rich::ORGS.len()];
    for (n, (_, l)) in letters.iter().enumerate() {
        let i = n * rich::EVERY;
        if l.parts.iter().any(|p| p.decoded == rich::logo(rich::org_of(i))) {
            logos[rich::org_of(i)] += 1;
        }
    }
    let repeats: u64 = logos.iter().enumerate().map(|(o, &k)| k.saturating_sub(1) * rich::logo(o).len() as u64).sum();
    assert!(repeats > 0 && st.dedup_saved() >= repeats, "{st:?}");
    assert!(st.parts < letters.len() as u64 && st.packed_saved() > 0, "{st:?}");
}
