//! Фаззинг хранилища (S2-03 «не падать»): случайные последовательности того, что делают команды списка
//! и синхронизация — письма приходят, перемещаются между папками, удаляются, помечаются, — сверка с
//! простой моделью после каждого шага. Инварианты: ничего не падает, счётчики папок сходятся,
//! страница папки — её письма от новых к старым, строки по id — актуальные.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::collections::BTreeMap;

use postman_cache::{Cache, EmailRow};

const MAILBOXES: &[&str] = &["inbox", "archive", "trash"];
const IDS: u64 = 60;
const STEPS: usize = 300;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn row(rng: &mut Rng, id: u64) -> EmailRow {
    // Иногда письмо сразу в двух папках, иногда — в той же дважды (защита от двойного счёта).
    let mut mailbox_ids = vec![MAILBOXES[rng.below(3) as usize].to_string()];
    match rng.below(10) {
        0 => mailbox_ids.push(MAILBOXES[rng.below(3) as usize].to_string()),
        1 => mailbox_ids.push(mailbox_ids[0].clone()),
        _ => {}
    }
    EmailRow {
        id: format!("m{id}"),
        thread_id: format!("t{}", id % 7),
        mailbox_ids,
        // Совпадающие даты — нарочно: порядок тогда решает id.
        received_at: 1_700_000_000 + rng.below(40) as i64 * 60,
        subject: format!("тема {id}"),
        from_name: "Отправитель".into(),
        from_addr: "a@corp.example".into(),
        preview: String::new(),
        seen: rng.below(2) == 0,
        flagged: false,
        has_attachment: false,
        size: 100,
        keywords: KEYWORDS.iter().filter(|_| rng.below(3) == 0).map(|k| k.to_string()).collect(),
    }
}

/// Метки писем (S5-02): индекс ключевых слов сверяется с моделью.
const KEYWORDS: &[&str] = &["pm.cat.a", "pm.cat.b"];

fn check(cache: &Cache, model: &BTreeMap<String, EmailRow>, step: usize) {
    for mb in MAILBOXES {
        let mut want: Vec<&EmailRow> = model.values().filter(|r| r.mailbox_ids.iter().any(|m| m == mb)).collect();
        want.sort_by(|a, b| b.received_at.cmp(&a.received_at).then_with(|| a.id.cmp(&b.id)));
        assert_eq!(cache.count(mb).unwrap(), want.len(), "шаг {step}: счётчик {mb}");
        let page: Vec<String> = cache.page(mb, 0, 1000).unwrap().into_iter().map(|r| r.id).collect();
        let want: Vec<String> = want.into_iter().map(|r| r.id.clone()).collect();
        assert_eq!(page, want, "шаг {step}: страница {mb}");
        // Окно в середине — то, что читает виртуальный список.
        if want.len() > 4 {
            let mid: Vec<String> = cache.page(mb, 2, 3).unwrap().into_iter().map(|r| r.id).collect();
            assert_eq!(mid, want[2..5], "шаг {step}: окно {mb}");
        }
    }
    let all = postman_cache::all_folders(None);
    for kw in KEYWORDS.iter().copied().chain(["$flagged"]) {
        for mb in MAILBOXES.iter().copied().chain([all.as_str()]) {
            let mut want: Vec<&EmailRow> =
                model.values().filter(|r| r.has_keyword(kw) && (mb == all || r.mailbox_ids.iter().any(|m| m == mb))).collect();
            want.sort_by(|a, b| b.received_at.cmp(&a.received_at).then_with(|| a.id.cmp(&b.id)));
            let want: Vec<String> = want.into_iter().map(|r| r.id.clone()).collect();
            assert_eq!(cache.keyword_count(kw, mb).unwrap(), want.len(), "шаг {step}: {kw} в {mb}");
            let page: Vec<String> = cache.keyword_page(kw, mb, 0, 1000).unwrap().into_iter().map(|r| r.id).collect();
            assert_eq!(page, want, "шаг {step}: письма {kw} в {mb}");
        }
    }
    let ids: Vec<String> = model.keys().cloned().collect();
    let rows = cache.rows_by_ids(&ids).unwrap();
    assert_eq!(rows.len(), model.len(), "шаг {step}: строки по id");
    for r in rows {
        let m = &model[&r.id];
        assert_eq!((r.seen, r.flagged, &r.mailbox_ids, &r.keywords), (m.seen, m.flagged, &m.mailbox_ids, &m.keywords), "шаг {step}: {}", r.id);
    }
}

fn run(seed: u64) {
    let cache = Cache::open_in_memory().unwrap();
    let mut rng = Rng(seed);
    let mut model: BTreeMap<String, EmailRow> = BTreeMap::new();
    for step in 0..STEPS {
        match rng.below(10) {
            // Пришли письма (новые или обновлённые: перемещение, смена флагов сервером).
            0..=3 => {
                let n = 1 + rng.below(5);
                let rows: Vec<EmailRow> = (0..n)
                    .map(|_| {
                        let id = rng.below(IDS);
                        row(&mut rng, id)
                    })
                    .collect();
                cache.upsert_emails(&rows).unwrap();
                for r in rows {
                    model.insert(r.id.clone(), r);
                }
            }
            // Удалены — в том числе несуществующие.
            4 => {
                let ids: Vec<String> = (0..1 + rng.below(3)).map(|_| format!("m{}", rng.below(IDS + 5))).collect();
                cache.delete_emails(&ids).unwrap();
                for id in &ids {
                    model.remove(id);
                }
            }
            // Метки ставятся и снимаются на месте — в том числе у отсутствующих писем.
            5 => {
                let id = format!("m{}", rng.below(IDS + 5));
                let kw = KEYWORDS[rng.below(KEYWORDS.len() as u64) as usize].to_string();
                let on = rng.below(2) == 0;
                let (add, remove) = if on { (vec![kw.clone()], vec![]) } else { (vec![], vec![kw.clone()]) };
                let got = cache.set_keywords(&id, &add, &remove).unwrap();
                assert_eq!(got.is_some(), model.contains_key(&id));
                if let Some(r) = model.get_mut(&id) {
                    r.keywords.retain(|k| *k != kw);
                    if on {
                        r.keywords.push(kw);
                        r.keywords.sort();
                    }
                }
            }
            // Пометки «прочитано» и флаг — в том числе у отсутствующих писем.
            _ => {
                let id = format!("m{}", rng.below(IDS + 5));
                let (seen, flagged) = (rng.below(2) == 0, rng.below(2) == 0);
                cache.set_flags(&id, seen, flagged).unwrap();
                if let Some(r) = model.get_mut(&id) {
                    r.seen = seen;
                    r.flagged = flagged;
                }
            }
        }
        check(&cache, &model, step);
    }
}

#[test]
fn random_command_sequences_keep_the_cache_consistent() {
    for seed in [0x9E37_79B9_7F4A_7C15, 0xD1B5_4A32_D192_ED03, 0x2545_F491_4F6C_DD1D] {
        run(seed);
    }
}
