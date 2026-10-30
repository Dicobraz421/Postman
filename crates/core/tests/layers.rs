//! Слои рабочей области (S2-01, `docs/specs/S2.md`; плагины путей — ADR 0004): ниже интерфейса нет
//! UI-зависимостей, слой ниже не зависит от слоя выше, у каждого крейта есть слой; `core` не знает протоколов,
//! плагин знает `core` и только свой протокол, интерфейс — плагины, но не протоколы. Проверка по `cargo tree`
//! (обычные зависимости, по Cargo.lock, без сети).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::collections::BTreeSet;
use std::process::Command;

const L0: u8 = 0;
const L1: u8 = 1;
const L2: u8 = 2;
/// L2b — плагины путей к серверам (ADR 0004): над `core`, под интерфейсом.
const L2B: u8 = 3;
const L3: u8 = 4;

/// Крейт → слой. L0 `ir`; L1 — без UI; L2 `core`; L2b — плагины путей (`backend-*`); L3 — интерфейс. `demo` —
/// данные для демо и тестов: без UI, от него зависит только интерфейс.
const LAYERS: &[(&str, u8)] = &[
    ("postman-ir", L0),
    ("postman-smime", L1),
    ("postman-jmap", L1),
    ("postman-imap", L1),
    ("postman-dav", L1),
    ("postman-ews", L1),
    ("postman-cache", L1),
    ("postman-compose", L1),
    ("postman-calendar", L1),
    ("postman-search", L1),
    ("postman-export", L1),
    ("postman-worker", L1),
    ("postman-core", L2),
    ("postman-demo", L2),
    ("postman-backend-jmap", L2B),
    ("postman-backend-imap", L2B),
    ("postman-backend-dav", L2B),
    ("postman-backend-ews", L2B),
    ("postman-gpui", L3),
];
const UI_LAYER: u8 = L3;

/// Библиотеки интерфейса и системных диалогов: только в L3.
const UI_CRATES: &[&str] = &["gpui", "egui", "eframe", "winit", "rfd"];

/// Протокольные клиенты серверов (L1, ADR 0003): каждый знает только свой плагин. Новый протокол — сюда.
const PROTOCOL_CRATES: &[&str] = &["postman-jmap", "postman-imap", "postman-dav", "postman-ews"];

/// Плагины путей (L2b, ADR 0004) и их протоколы. Новый плагин — сюда (EWS — `postman-backend-ews`, S7-06б).
const PLUGINS: &[(&str, &str)] = &[
    ("postman-backend-jmap", "postman-jmap"),
    ("postman-backend-imap", "postman-imap"),
    ("postman-backend-dav", "postman-dav"),
    ("postman-backend-ews", "postman-ews"),
];

const CORE: &str = "postman-core";
/// Выгрузка в файлы (S8): из байтов письма и свойств MAPI. От неё зависят `core` и `worker` (свойства MAPI заполняет
/// воркер, S8-29), сама — ни от них, ни от протоколов.
const EXPORT: &str = "postman-export";

/// Разбор MIME писем (S8-36): чужие письма разбирает только воркер-песочница.
const MIME_PARSER: &str = "mail-parser";
const WORKER: &str = "postman-worker";

fn cargo_tree(args: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "--offline", "--prefix", "none", "-e", "normal"])
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree");
    assert!(out.status.success(), "cargo tree {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_whitespace().next()).map(str::to_owned).collect()
}

fn layer(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|&(_, l)| l)
}

fn label(l: u8) -> &'static str {
    ["L0", "L1", "L2", "L2b", "L3"].get(usize::from(l)).copied().unwrap_or("L?")
}

fn is_ui(name: &str) -> bool {
    UI_CRATES.iter().any(|u| name == *u || name.starts_with(&format!("{u}-")) || name.starts_with(&format!("{u}_")))
}

fn is_plugin(name: &str) -> bool {
    PLUGINS.iter().any(|(p, _)| *p == name)
}

#[test]
fn every_crate_has_a_layer() {
    let members: BTreeSet<String> = cargo_tree(&["--workspace", "--depth", "0"]).into_iter().collect();
    let missing: Vec<_> = members.iter().filter(|m| layer(m).is_none()).collect();
    assert!(missing.is_empty(), "крейты без слоя — добавьте в LAYERS и в docs/specs/S2.md: {missing:?}");
    let unlisted: Vec<_> = LAYERS.iter().filter(|&&(n, l)| l == L2B && !is_plugin(n)).map(|(n, _)| n).collect();
    assert!(unlisted.is_empty(), "плагины L2b без протокола — добавьте в PLUGINS: {unlisted:?}");
}

#[test]
fn no_ui_dependencies_below_the_interface() {
    for &(name, l) in LAYERS.iter().filter(|(_, l)| *l < UI_LAYER) {
        let ui: BTreeSet<String> = cargo_tree(&["-p", name]).into_iter().filter(|d| is_ui(d)).collect();
        assert!(ui.is_empty(), "{name} ({}) зависит от библиотек интерфейса: {ui:?}", label(l));
    }
}

#[test]
fn lower_layers_do_not_depend_on_higher() {
    for &(name, l) in LAYERS {
        for dep in cargo_tree(&["-p", name, "--depth", "1"]).iter().skip(1) {
            let Some(dl) = layer(dep) else { continue };
            assert!(dl <= l, "{name} ({}) зависит от {dep} ({})", label(l), label(dl));
            assert!(dep != "postman-demo" || l == UI_LAYER, "{name} ({}) зависит от демо-данных", label(l));
        }
    }
}

/// Ядро функционально (решение владельца 2026-10-23, ADR 0004): ни протоколов, ни плагинов во всём дереве.
#[test]
fn core_does_not_know_protocols() {
    let deps: BTreeSet<String> = cargo_tree(&["-p", CORE]).into_iter().collect();
    let known: Vec<_> = PROTOCOL_CRATES.iter().filter(|p| deps.contains(**p)).collect();
    assert!(known.is_empty(), "{CORE} зависит от протокольных крейтов (их знают только плагины backend-*): {known:?}");
    let plugins: Vec<_> = PLUGINS.iter().map(|(p, _)| p).filter(|p| deps.contains(**p)).collect();
    assert!(plugins.is_empty(), "{CORE} зависит от плагинов: {plugins:?}");
}

/// `export` — только байты: ни ядра, ни протоколов, ни воркера во всём дереве (S8-03).
#[test]
fn export_knows_only_bytes() {
    let deps: BTreeSet<String> = cargo_tree(&["-p", EXPORT]).into_iter().collect();
    let known: Vec<_> = PROTOCOL_CRATES.iter().chain(&[CORE, "postman-worker"]).filter(|p| deps.contains(**p)).collect();
    assert!(known.is_empty(), "{EXPORT} зависит от ядра, протоколов или воркера: {known:?}");
}

/// Плагин — поверх `core` и своего протокола: чужих протоколов и других плагинов нет во всём дереве.
#[test]
fn plugins_know_core_and_their_own_protocol() {
    for &(plugin, own) in PLUGINS {
        let direct: BTreeSet<String> = cargo_tree(&["-p", plugin, "--depth", "1"]).into_iter().collect();
        assert!(direct.contains(CORE) && direct.contains(own), "{plugin} должен зависеть от {CORE} и {own}: {direct:?}");
        let all: BTreeSet<String> = cargo_tree(&["-p", plugin]).into_iter().collect();
        let foreign: Vec<_> = PROTOCOL_CRATES.iter().filter(|p| **p != own && all.contains(**p)).collect();
        assert!(foreign.is_empty(), "{plugin} зависит от чужих протоколов: {foreign:?}");
        let others: Vec<_> = PLUGINS.iter().map(|(p, _)| p).filter(|p| **p != plugin && all.contains(**p)).collect();
        assert!(others.is_empty(), "{plugin} зависит от других плагинов (связь — только договором core): {others:?}");
    }
}

#[test]
fn interface_does_not_know_protocols() {
    for &(name, _) in LAYERS.iter().filter(|(_, l)| *l == UI_LAYER) {
        let deps: BTreeSet<String> = cargo_tree(&["-p", name, "--depth", "1"]).into_iter().collect();
        let known: Vec<_> = PROTOCOL_CRATES.iter().filter(|p| deps.contains(**p)).collect();
        assert!(known.is_empty(), "{name} зависит от протокольных крейтов (только через плагины backend-*): {known:?}");
    }
}

/// Разборщик MIME — только в зависимостях воркера (S8-36): без поддерева воркера он не нужен ни одному крейту —
/// S/MIME, протоколы, ядро и интерфейс получают от воркера готовые части, сами письмо не разбирают.
#[test]
fn only_the_worker_parses_mail() {
    let users: BTreeSet<String> =
        cargo_tree(&["--workspace", "-i", MIME_PARSER, "--prune", WORKER]).into_iter().filter(|d| layer(d).is_some()).collect();
    assert!(users.is_empty(), "{MIME_PARSER} нужен не только через {WORKER}: {users:?}");
}
