//! Слой приложения без UI: всё, что нужно интерфейсу, кроме самого интерфейса и протоколов серверов. Протоколы —
//! плагины `postman-backend-*` по договору `backend` (ADR 0004).
//! Интерфейс шлёт команды и получает события; данные читает из хранилища.

pub mod assign;
pub mod attach;
pub mod autoreply;
pub mod backend;
pub mod bench;
pub mod bulk;
pub mod categories;
pub mod checks;
pub mod contacts;
pub mod export;
pub mod extract;
pub mod files;
pub mod filters;
pub mod fuzzy;
pub mod indexer;
pub mod invite;
pub mod labels;
pub mod lifecycle;
pub mod link;
pub mod mailtask;
pub mod marks;
pub mod migrate;
pub mod model;
pub mod palette;
pub mod prefs;
pub mod query;
pub mod recur;
pub mod render;
pub mod rules;
pub mod schedule;
pub mod send_later;
pub mod settings;
pub mod smime;
pub mod store;
pub mod sync;
pub mod taskparse;
pub mod tasks;
pub mod thread;
pub mod viewer;
