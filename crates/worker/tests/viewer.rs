//! Хост модулей просмотра WASM (S8-24): отказы по пределам, проверка ответа и запуск `postman-viewer` в песочнице.
//! Модули — байтами, собранными здесь же (разбора текста `wat` в хосте нет).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // интеграционный тест: «не падать» — для кода приложения (S2-03)

use std::path::PathBuf;

use postman_ir::{Block, Inline, Marks};
use postman_worker::wasm::{self, Refusal, ViewerOutput};
use postman_worker::{Client, Level, Section, SectionTitle};

fn leb(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn sleb(mut n: i64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if (n == 0 && b & 0x40 == 0) || (n == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Вектор WASM: число элементов и они подряд.
fn vec_of(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    leb(items.len() as u64, &mut out);
    items.iter().for_each(|i| out.extend(i));
    out
}

fn name(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    leb(s.len() as u64, &mut out);
    out.extend(s.as_bytes());
    out
}

fn section(id: u8, body: Vec<u8>, out: &mut Vec<u8>) {
    out.push(id);
    leb(body.len() as u64, out);
    out.extend(body);
}

/// Тело функции без локальных: инструкции и `end`.
fn code(instrs: &[u8]) -> Vec<u8> {
    let mut body = vec![0];
    body.extend(instrs);
    body.push(0x0B);
    let mut out = Vec::new();
    leb(body.len() as u64, &mut out);
    out.extend(body);
    out
}

/// `i64.const n`.
fn i64_const(n: i64) -> Vec<u8> {
    let mut out = vec![0x42];
    sleb(n, &mut out);
    out
}

/// Ответ `pm_view`: указатель — в старших 32 битах, длина — в младших.
fn packed(ptr: u32, len: u32) -> Vec<u8> {
    i64_const(((u64::from(ptr) << 32) | u64::from(len)) as i64)
}

/// Где модуль держит данные (ответ из сегмента данных) и куда кладёт вход.
const DATA: u32 = 1024;
const INPUT: u32 = 32 * 1024;

/// Модуль ABI v1: `pm_abi` → 1, `pm_alloc` → [`INPUT`], `pm_view` — `view`; память — `pages` страниц по 64 КиБ, в ней
/// с [`DATA`] — `data`; `import` — с импортом функции `env.log`.
fn module(view: &[u8], pages: u32, data: &[u8], import: bool) -> Vec<u8> {
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    // Типы: () → i32, (i32) → i32, (i32, i32) → i64.
    section(1, vec_of(&[vec![0x60, 0, 1, 0x7F], vec![0x60, 1, 0x7F, 1, 0x7F], vec![0x60, 2, 0x7F, 0x7F, 1, 0x7E]]), &mut m);
    if import {
        section(2, vec_of(&[[name("env"), name("log"), vec![0, 0]].concat()]), &mut m);
    }
    section(3, vec_of(&[vec![0], vec![1], vec![2]]), &mut m);
    let mut memory = vec![0];
    leb(u64::from(pages), &mut memory);
    section(5, vec_of(&[memory]), &mut m);
    let first = u8::from(import);
    let export = |n: &str, kind: u8, index: u8| [name(n), vec![kind, index]].concat();
    section(
        7,
        vec_of(&[export("memory", 2, 0), export("pm_abi", 0, first), export("pm_alloc", 0, first + 1), export("pm_view", 0, first + 2)]),
        &mut m,
    );
    let mut alloc = vec![0x41];
    sleb(i64::from(INPUT), &mut alloc);
    section(10, vec_of(&[code(&[0x41, 1]), code(&alloc), code(view)]), &mut m);
    if !data.is_empty() {
        let mut segment = vec![0, 0x41];
        sleb(i64::from(DATA), &mut segment);
        segment.push(0x0B);
        leb(data.len() as u64, &mut segment);
        segment.extend(data);
        section(11, vec_of(&[segment]), &mut m);
    }
    m
}

/// Модуль, отвечающий байтами `answer` на любой вход.
fn answering(answer: &[u8]) -> Vec<u8> {
    module(&packed(DATA, answer.len() as u32), 1, answer, false)
}

fn text(s: &str) -> Inline {
    Inline::Text { text: s.into(), marks: Marks::default() }
}

fn link(href: &str, label: &str) -> Inline {
    Inline::Link { href: href.into(), host: None, suspicious: false, content: vec![text(label)] }
}

fn sections(blocks: Vec<Block>) -> ViewerOutput {
    ViewerOutput::Sections { sections: vec![Section { title: SectionTitle::None, blocks }], truncated: false }
}

fn view(module: &[u8]) -> Result<ViewerOutput, Refusal> {
    wasm::run(module, "вход".as_bytes(), wasm::FUEL)
}

#[test]
fn module_with_an_import_is_refused_before_start() {
    let answer = postcard::to_allocvec(&ViewerOutput::Error("x".into())).unwrap();
    assert!(view(&answering(&answer)).is_ok(), "тот же модуль без импорта работает");
    assert_eq!(view(&module(&packed(DATA, answer.len() as u32), 1, &answer, true)), Err(Refusal::Imports));
    assert_eq!(view(b"\0asm\x01\0\0\0garbage"), Err(Refusal::Invalid));
    assert_eq!(view(b"not wasm"), Err(Refusal::Invalid));
}

#[test]
fn endless_loop_runs_out_of_fuel() {
    // loop { br 0 } unreachable
    let endless = module(&[0x03, 0x40, 0x0C, 0, 0x0B, 0x00], 1, &[], false);
    assert_eq!(wasm::run(&endless, b"", 1_000_000), Err(Refusal::Fuel));
}

#[test]
fn memory_growth_over_the_limit_is_refused() {
    // memory.grow на 5000 страниц (312,5 МиБ) — сверх 256 МиБ; рост в пределах проходит.
    let grow = |pages: i64| {
        let mut v = vec![0x41];
        sleb(pages, &mut v);
        v.extend([0x40, 0, 0x1A]);
        v.extend(packed(0, 0));
        module(&v, 1, &[], false)
    };
    assert_eq!(view(&grow(5000)), Err(Refusal::Memory));
    // Пустой ответ — не postcard: память выросла, отказ — уже по ответу.
    assert_eq!(view(&grow(16)), Err(Refusal::Output));
    // Начальная память сверх предела — отказ до запуска.
    assert_eq!(view(&module(&packed(0, 0), 5000, &[], false)), Err(Refusal::Memory));
}

#[test]
fn javascript_link_becomes_text_and_good_links_are_checked() {
    let out = sections(vec![Block::Paragraph {
        align: Default::default(),
        content: vec![link("javascript:alert(1)", "нажми"), text(" и "), link("https://evil.example/login", "https://bank.ru")],
    }]);
    let got = view(&answering(&postcard::to_allocvec(&out).unwrap())).unwrap();
    let ViewerOutput::Sections { sections, .. } = got else { panic!("{got:?}") };
    let Block::Paragraph { content, .. } = &sections[0].blocks[0] else { panic!() };
    assert_eq!(content[0], text("нажми"), "без ссылки");
    assert_eq!(content[1], text(" и "));
    let Inline::Link { href, host, suspicious, .. } = &content[2] else { panic!("{content:?}") };
    assert_eq!(
        (href.as_str(), host.as_deref(), *suspicious),
        ("https://evil.example/login", Some("evil.example"), true),
        "хост и подозрение — от хоста"
    );
}

#[test]
fn answer_over_8_mib_is_refused() {
    // 9 МиБ памяти модуля (144 страницы) и ответ во всю её длину.
    let len = 9 * 1024 * 1024;
    assert_eq!(view(&module(&packed(0, len), 145, &[], false)), Err(Refusal::Output));
    // Ответ за пределами памяти модуля.
    assert_eq!(view(&module(&packed(65_000, 1000), 1, &[], false)), Err(Refusal::Output));
}

#[test]
fn answer_limits_are_checked() {
    let check = |out: &ViewerOutput| view(&answering(&postcard::to_allocvec(out).unwrap()));
    let image = |w: u32, h: u32, len: usize| ViewerOutput::Image { width: w, height: h, rgba: vec![7; len] };
    assert!(check(&image(2, 3, 24)).is_ok());
    assert_eq!(check(&image(2, 3, 23)), Err(Refusal::Output));
    assert_eq!(wasm::checked(image(2049, 1, 2049 * 4)), Err(Refusal::Output));
    let deep = (0..=wasm::MAX_NESTING).fold(vec![Block::Rule], |b, _| vec![Block::Quote(b)]);
    assert_eq!(check(&sections(deep)), Err(Refusal::Output));
    let many = vec![Block::Rule; 10_001];
    assert_eq!(check(&sections(many)), Err(Refusal::Output));
    let error = check(&ViewerOutput::Error("сбой\u{1b}[31m".repeat(500))).unwrap();
    assert_eq!(error, ViewerOutput::Error("сбой[31m".repeat(500).chars().take(wasm::MAX_ERROR).collect()));
}

/// Хост в песочнице: модуль показывает, картинка приходит пикселями; в воркере модулей нет.
#[test]
fn viewer_runs_in_the_sandbox() {
    let mut c = Client::viewer(PathBuf::from(env!("CARGO_BIN_EXE_postman-viewer")));
    let out =
        sections(vec![Block::Paragraph { align: Default::default(), content: vec![text("Глава 1"), link("javascript:x", " — далее")] }]);
    let got = c.view_module(&answering(&postcard::to_allocvec(&out).unwrap()), b"FB2").unwrap().unwrap();
    assert_eq!(got, sections(vec![Block::Paragraph { align: Default::default(), content: vec![text("Глава 1"), text(" — далее")] }]));
    if cfg!(windows) && std::env::var_os("POSTMAN_REQUIRE_FULL_SANDBOX").is_none() {
        assert!(matches!(c.level(), Level::Full | Level::Partial));
    } else {
        assert_eq!(c.level(), Level::Full, "{:?}", c.fallback());
    }
    let image = ViewerOutput::Image { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
    assert_eq!(c.view_module(&answering(&postcard::to_allocvec(&image).unwrap()), b"").unwrap(), Ok(image));
    let answer = postcard::to_allocvec(&out).unwrap();
    assert_eq!(c.view_module(&module(&packed(DATA, answer.len() as u32), 1, &answer, true), b""), Ok(Err(Refusal::Imports)));
    assert!(c.view_module(&answering(&answer), b"").unwrap().is_ok(), "после отказа хост жив");

    let mut worker = Client::new(PathBuf::from(env!("CARGO_BIN_EXE_postman-worker")), wasm::VIEW_TIMEOUT);
    let e = worker.view_module(&answering(&[]), b"").unwrap_err();
    assert!(e.contains("postman-viewer"), "{e}");
}
