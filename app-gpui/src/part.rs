//! Опись части интерфейса (ADR 0005): часть (вложения, задачи, контакты, S/MIME…) отдаёт одну константу `PART` —
//! свои тексты, команды, обработчики действий и сцены `--screens`. Части перечислены в `parts.rs`; оттуда их
//! собирают `main.rs` (тексты), `commands.rs` (реестр команд), `Mailer::render` (обработчики) и `screens.rs` (сцены).
//! Тип назван `Part`: `Feature` занят (`postman_core::model::Feature` — умения сервера).

use gpui_kit::*;

use crate::Mailer;

pub(crate) struct Part {
    /// Для журналов и тестов.
    pub name: &'static str,
    /// Свои `.ftl` (`i18n/<язык>/<часть>.ftl`), RU и EN.
    pub texts: &'static [pets_i18n::Resource],
    /// Свои команды (`cmd(…)`) с клавишами и контекстом; порядок `PARTS` — порядок реестра (палитра, клавиши,
    /// `docs/PARITY.md`).
    pub commands: &'static [pets_commands::Command],
    /// Свои `on_action` на корне экрана почты.
    pub handlers: fn(Div, &mut Context<Mailer>) -> Div,
}

impl Part {
    /// Пустая опись: `Part { name: "…", texts: TEXTS, ..Part::NONE }`.
    pub(crate) const NONE: Part =
        Part { name: "", texts: &[], commands: &[], handlers: |root, _| root };
}
