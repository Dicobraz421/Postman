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
