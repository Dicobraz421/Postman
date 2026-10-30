//! Части интерфейса (ADR 0005): единственный список. Новая часть — свой модуль (`mod x;` в `main.rs`) с `PART` и
//! строка `x::PART,` здесь. Порядок — порядок реестра команд (палитра, таблица клавиш, `docs/PARITY.md`) и сцен
//! `--screens` (сцена с `after` встаёт за названной сценой другой части).

use crate::part::Part;

pub(crate) const PARTS: &[Part] = &[
    crate::actions::PART,
    crate::thread::PART,
    crate::attachments::PART,
    crate::chrome::PART,
    crate::smime::PART,
    crate::search::PART,
    crate::palette::PART,
    crate::calendar::PART,
    crate::compose::PART,
    crate::meeting::PART,
    crate::contacts::PART,
    crate::tasks::PART,
    crate::invite_card::PART,
    crate::settings::PART,
    crate::shared::PART,
    crate::folders::PART,
    crate::local::PART,
    crate::migration::PART,
    crate::labels::PART,
    crate::filters::PART,
    crate::lifecycle::PART,
    crate::bulk::PART,
    crate::states::PART,
    crate::notices::PART,
    crate::keytips::PART,
    crate::signin::PART,
    crate::buttons::PART,
    crate::features::PART,
];

/// Тексты всех частей (без `pets_ui::FTL`: его добавляет запуск).
pub(crate) fn texts() -> Vec<&'static [pets_i18n::Resource]> {
    PARTS.iter().map(|p| p.texts).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::PARTS;

    /// Описи частей согласованы (ADR 0005): имена частей и сцен уникальны, тексты части — на RU и EN, сцена,
    /// продолжающая прошлую, не открывает часть, `after` называет сцену другой части.
    #[test]
    fn parts_are_consistent() {
        let mut parts = HashSet::new();
        let mut scenes = HashSet::new();
        for p in PARTS {
            assert!(parts.insert(p.name), "часть {} — дважды", p.name);
            let count = |lang: &str| p.texts.iter().filter(|(l, _)| *l == lang).count();
            assert_eq!(count("ru"), count("en"), "{}: тексты RU и EN — разное число файлов", p.name);
            assert_eq!(count("ru") + count("en"), p.texts.len(), "{}: тексты не на RU и EN", p.name);
            for (i, s) in p.scenes.iter().enumerate() {
                assert!(scenes.insert(s.name), "сцена {} — дважды", s.name);
                assert!(!s.continues || (i > 0 && s.after.is_none()), "{}: продолжает прошлую, но открывает часть", s.name);
                if let Some(a) = s.after {
                    assert!(!p.scenes.iter().any(|x| x.name == a), "{}: after — сцена своей части", s.name);
                }
            }
        }
        for s in PARTS.iter().flat_map(|p| p.scenes) {
            if let Some(a) = s.after {
                assert!(scenes.contains(a), "{}: after — нет сцены {a}", s.name);
            }
        }
    }
}
