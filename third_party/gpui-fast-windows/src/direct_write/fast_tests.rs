//! Главный инвариант быстрого пути: на корпусе строк он даёт то же, что старый.
//! Запуск и сводка: `cargo test … --lib --features test-support -- fast_ --nocapture`.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use gpui::{Font, FontId, FontRun, FontStyle, FontWeight, LineLayout, font, px};

use super::super::{DirectWriteComponents, DirectWriteState, DirectWriteTextSystem};
use super::{CHARS, MAX_TABLES, all_chars, char_index};

/// Шрифты Урфина: Cascadia Mono → Consolas (текст), Segoe UI Variable → Segoe UI (оболочка).
const FAMILIES: &[&str] = &[
    "Cascadia Mono",
    "Cascadia Code",
    "Consolas",
    "Segoe UI",
    "Segoe UI Variable Text",
];
const SIZES: &[f32] = &[10.0, 11.0, 12.0, 12.5, 13.0, 14.0, 15.0, 16.0, 18.0, 24.0];

fn system() -> DirectWriteTextSystem {
    DirectWriteTextSystem::new(None).unwrap()
}

fn id(sys: &DirectWriteTextSystem, f: &Font) -> FontId {
    sys.state.write().select_and_cache_font(&sys.components, f).unwrap()
}

/// Плоский вид: разбиение на `ShapedRun` не сравниваем, оно ни на что не влияет — потребители
/// gpui идут по глифам подряд и берут `font_id` прогона.
fn flat(l: &LineLayout) -> Vec<(usize, u32, u32, f32, usize, bool)> {
    l.runs
        .iter()
        .flat_map(|r| {
            r.glyphs.iter().map(|g| {
                (
                    r.font_id.0,
                    g.id.0,
                    g.position.x.as_f32().to_bits(),
                    g.position.y.as_f32(),
                    g.index,
                    g.is_emoji,
                )
            })
        })
        .collect()
}

#[derive(Default)]
struct Stat {
    fast: usize,
    slow: usize,
    fast_glyphs: usize,
    bad: usize,
}

fn check(
    state: &mut DirectWriteState,
    c: &DirectWriteComponents,
    text: &str,
    size: f32,
    runs: &[FontRun],
    stat: &mut Stat,
) {
    let old = state.layout_line_slow(c, text, px(size), runs).unwrap();
    let Some(new) = state.layout_line_fast(c, text, px(size), runs) else {
        stat.slow += 1;
        return;
    };
    stat.fast += 1;
    stat.fast_glyphs += text.len();
    let same = flat(&old) == flat(&new)
        && old.width.as_f32().to_bits() == new.width.as_f32().to_bits()
        && old.ascent == new.ascent
        && old.descent == new.descent
        && old.font_size == new.font_size
        && old.len == new.len;
    if !same {
        stat.bad += 1;
        if stat.bad <= 5 {
            let (a, b) = (flat(&old), flat(&new));
            let first = a.iter().zip(&b).position(|(x, y)| x != y);
            eprintln!(
                "РАСХОЖДЕНИЕ {size} {runs:?} {text:?}\n  ширина {} / {}, глифов {} / {}, первое {first:?} {:?} {:?}, ascent {:?}/{:?} descent {:?}/{:?}",
                old.width.as_f32(),
                new.width.as_f32(),
                a.len(),
                b.len(),
                first.map(|i| a[i]),
                first.map(|i| b[i]),
                old.ascent,
                new.ascent,
                old.descent,
                new.descent,
            );
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn corpus() -> Vec<String> {
    let mut lines: Vec<String> = [
        "hello, world",
        "Съешь ещё этих мягких французских булок, да выпей чаю",
        "ЁЖИК В ТУМАНЕ ё ъ ы Ё № 42 — «цитата»…",
        "mixed: файл report.txt открыт, строка 42",
        "AV To Ty WA LT P. fi fl ffi ffl",
        "АУ ТА ГА Г. Р. УА",
        "-> => != === !== <= >= <- <-- --> ::: ... www w ww || && |> <| /* */ // ## #{ ",
        "   ведущие и хвостовые пробелы   ",
        "    ",
        " ",
        "a",
        "я",
        "\tтаб в начале",
        "таб\tв\tсередине\t",
        "col1\tcol2\t\tcol4",
        "\t",
        "a\t",
        "§ ± × ÷ ° µ ¶ · ¸ » « ¿ ¡ ¨ © ®",
        "NBSP:\u{a0}раз\u{a0}два",
        "soft\u{ad}hyphen",
        "тире – — ‐ ‑ ‒ ― и ‘кавычки’ “двойные” „нижние” • … ‰ ‡ †",
        "стрелки ← ↑ → ↓",
        "é vs e\u{301}, й vs и\u{306}",
        "i\u{301} j\u{301} і\u{308}",
        "emoji 😀 and ✨ ☺ ❤",
        "עברית mixed",
        "العربية mixed",
        "ﬁ ligature char",
        "中文 mixed",
        "\u{200b}zw\u{200d}j\u{200c}",
        "x\u{7f}y\u{1}z",
        "pub fn layout_line(&mut self, text: &str) -> Result<LineLayout> {",
        "    let x = a <= b && c != d; // сравнение",
        "2026-11-05T12:00:00.000Z INFO urfin::view: открыт файл C:\\Users\\Иван\\Документы\\отчёт.txt",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    for src in [
        include_str!("../../README.md"),
        include_str!("../direct_write.rs"),
        include_str!("fast.rs"),
    ] {
        lines.extend(src.lines().filter(|l| !l.is_empty()).map(ToString::to_string));
    }
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let simple: Vec<char> = all_chars().filter_map(char::from_u32).collect();
    let tricky: Vec<char> = "\t \u{301}\u{a0}\u{ad}\u{200b}é€😀".chars().collect();
    for _ in 0..600 {
        let n = 1 + rng.below(90);
        let mut s = String::new();
        for _ in 0..n {
            s.push(simple[rng.below(simple.len())]);
        }
        lines.push(s);
    }
    for _ in 0..200 {
        let n = 1 + rng.below(60);
        let mut s = String::new();
        for _ in 0..n {
            let table = if rng.below(8) == 0 { &tricky } else { &simple };
            s.push(table[rng.below(table.len())]);
        }
        lines.push(s);
    }
    lines
}

/// Разрезать строку на прогоны: подсветка синтаксиса даёт один шрифт разными цветами,
/// полужирное — другой шрифт.
fn split(text: &str, parts: usize, fonts: &[FontId], rng: &mut Rng) -> Vec<FontRun> {
    let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).skip(1).collect();
    let mut cuts = BTreeSet::new();
    for _ in 1..parts {
        if !ends.is_empty() {
            cuts.insert(ends[rng.below(ends.len())]);
        }
    }
    cuts.insert(text.len());
    let mut at = 0;
    let mut out = Vec::new();
    for (i, cut) in cuts.into_iter().enumerate() {
        out.push(FontRun {
            len: cut - at,
            font_id: fonts[i % fonts.len()],
        });
        at = cut;
    }
    out
}

#[test]
fn pets_fast_matches_slow_on_corpus() {
    let lines = corpus();
    let sys = system();
    let mut total = 0usize;
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut report = BTreeMap::new();
    for family in FAMILIES {
        let mut regular = font(family.to_string());
        regular.fallbacks = None;
        let mut bold = regular.clone();
        bold.weight = FontWeight::BOLD;
        let mut italic = regular.clone();
        italic.style = FontStyle::Italic;
        let (r, b, i) = (id(&sys, &regular), id(&sys, &bold), id(&sys, &italic));
        let mut state = sys.state.write();
        let c = &sys.components;
        let mut stat = Stat::default();
        for &size in SIZES {
            for line in &lines {
                for (fonts, parts) in [
                    (&[r][..], 1),
                    (&[r][..], 2),
                    (&[r][..], 5),
                    (&[b][..], 1),
                    (&[i][..], 1),
                    (&[r, b][..], 3),
                    (&[r, i][..], 2),
                ] {
                    let runs = split(line, parts, fonts, &mut rng);
                    check(&mut state, c, line, size, &runs, &mut stat);
                }
            }
        }
        total += stat.bad;
        report.insert(
            *family,
            format!(
                "быстро {} строк, старым путём {}, символов быстро {}, расхождений {}",
                stat.fast, stat.slow, stat.fast_glyphs, stat.bad
            ),
        );
    }
    for (family, line) in report {
        eprintln!("{family}: {line}");
    }
    assert_eq!(total, 0, "расхождения быстрого пути со старым");
}

/// Урфин грузит шрифты файлами через `add_fonts`, не из системной коллекции.
#[test]
fn pets_fast_matches_slow_with_added_fonts() {
    let lines = corpus();
    let sys = system();
    {
        let mut state = sys.state.write();
        let fonts = ["CascadiaMono.ttf", "SegUIVar.ttf", "consola.ttf"]
            .iter()
            .filter_map(|f| std::fs::read(format!("C:/Windows/Fonts/{f}")).ok())
            .map(Cow::Owned)
            .collect();
        state.add_fonts(&sys.components, fonts).unwrap();
    }
    let mut rng = Rng(0xabcd_ef01_2345_6789);
    for family in ["Cascadia Mono", "Segoe UI Variable Text", "Consolas"] {
        let regular = font(family.to_string());
        let mut bold = regular.clone();
        bold.weight = FontWeight::BOLD;
        let (r, b) = (id(&sys, &regular), id(&sys, &bold));
        let mut state = sys.state.write();
        let c = &sys.components;
        let mut stat = Stat::default();
        for &size in &SIZES[2..6] {
            for line in &lines {
                for (fonts, parts) in [(&[r][..], 1), (&[r][..], 4), (&[b][..], 1), (&[r, b][..], 3)] {
                    let runs = split(line, parts, fonts, &mut rng);
                    check(&mut state, c, line, size, &runs, &mut stat);
                }
            }
        }
        eprintln!(
            "{family} (add_fonts): быстро {} строк, старым путём {}, расхождений {}",
            stat.fast, stat.slow, stat.bad
        );
        assert_eq!(stat.bad, 0);
    }
}

/// Строка 64 КиБ: столько бывает в ряду редактора.
#[test]
fn pets_fast_matches_slow_on_long_line() {
    let sys = system();
    let f = id(&sys, &font("Cascadia Mono"));
    let mut state = sys.state.write();
    let c = &sys.components;
    let mut stat = Stat::default();
    let text = "строка long line 0123456789 ".repeat(3000);
    let runs = [FontRun {
        len: text.len(),
        font_id: f,
    }];
    for size in [13.0, 14.0] {
        check(&mut state, c, &text, size, &runs, &mut stat);
    }
    assert_eq!((stat.fast, stat.bad), (2, 0));
}

#[test]
fn pets_fast_chars_by_font() {
    let sys = system();
    for family in FAMILIES {
        let f = id(&sys, &font(family.to_string()));
        let mut state = sys.state.write();
        let c = &sys.components;
        let mut slow = String::new();
        let mut fast = 0;
        let mut missing = 0;
        for cp in all_chars() {
            let ch = char::from_u32(cp).unwrap();
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            let runs = [FontRun {
                len: s.len(),
                font_id: f,
            }];
            let ix = char_index(ch).unwrap();
            match state.fast_font(f) {
                Some(t) if t.cmap[ix] == 0 => missing += 1,
                _ => {
                    if state.layout_line_fast(c, s, px(13.0), &runs).is_some() {
                        fast += 1;
                    } else {
                        slow.push(ch);
                    }
                }
            }
        }
        eprintln!(
            "{family}: разбор {}, быстро {fast} из {CHARS}, нет в шрифте {missing}, старым путём {:?}",
            state.fast_font(f).is_some(),
            slow,
        );
    }
}

#[test]
fn pets_fast_keeps_ligature_fonts_on_slow_path() {
    let sys = system();
    let code = id(&sys, &font("Cascadia Code"));
    let mono = id(&sys, &font("Cascadia Mono"));
    let mut state = sys.state.write();
    let c = &sys.components;
    let line = |text: &str, font_id| {
        [FontRun {
            len: text.len(),
            font_id,
        }]
    };
    for s in ["->", "=>", "!=", "===", "www", "<="] {
        assert!(state.layout_line_fast(c, s, px(13.0), &line(s, code)).is_none());
        assert!(state.layout_line_fast(c, s, px(13.0), &line(s, mono)).is_some());
    }
    let s = "plain text";
    assert!(state.layout_line_fast(c, s, px(13.0), &line(s, code)).is_some());
}

#[test]
fn pets_fast_features_force_slow_path() {
    let sys = system();
    let mut f = font("Cascadia Mono");
    f.features = gpui::FontFeatures::disable_ligatures();
    let f = id(&sys, &f);
    let mut state = sys.state.write();
    let s = "plain text";
    let runs = [FontRun {
        len: s.len(),
        font_id: f,
    }];
    assert!(state.layout_line_fast(&sys.components, s, px(13.0), &runs).is_none());
}

#[test]
fn pets_fast_cache_is_bounded() {
    let sys = system();
    let f = id(&sys, &font("Cascadia Mono"));
    let mut state = sys.state.write();
    let c = &sys.components;
    let s = "bounded строка";
    let runs = [FontRun {
        len: s.len(),
        font_id: f,
    }];
    for i in 0..500 {
        let size = 8.0 + i as f32 / 16.0;
        assert!(state.layout_line_fast(c, s, px(size), &runs).is_some());
        assert!(state.fast.tables.len() <= MAX_TABLES);
    }
    // После сброса таблиц ширина та же.
    let mut stat = Stat::default();
    check(&mut state, c, s, 13.0, &runs, &mut stat);
    assert_eq!((stat.fast, stat.bad), (1, 0));
}

#[test]
fn pets_fast_is_faster() {
    let sys = system();
    let f = id(&sys, &font("Cascadia Mono"));
    let mut state = sys.state.write();
    let c = &sys.components;
    let lines: Vec<String> = (0..1000)
        .map(|i| format!("2026-11-05 12:00:{i:02} INFO строка номер {i} и ещё немного текста"))
        .collect();
    for run_count in [1usize, 6] {
        let runs: Vec<Vec<FontRun>> = {
            let mut rng = Rng(7);
            lines.iter().map(|l| split(l, run_count, &[f], &mut rng)).collect()
        };
        let t = std::time::Instant::now();
        for _ in 0..5 {
            for (l, r) in lines.iter().zip(&runs) {
                state.layout_line_slow(c, l, px(13.0), r).unwrap();
            }
        }
        let slow = t.elapsed() / 5000;
        let t = std::time::Instant::now();
        for _ in 0..5 {
            for (l, r) in lines.iter().zip(&runs) {
                state.layout_line_fast(c, l, px(13.0), r).unwrap();
            }
        }
        let fast = t.elapsed() / 5000;
        eprintln!("прогонов {run_count}: старый {slow:?} на строку, быстрый {fast:?}");
    }
}
