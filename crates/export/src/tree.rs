//! Дерево папок учётки → дерево файлов выгрузки: имена без запрещённых символов и зарезервированных имён Windows,
//! без совпадений у соседей (Windows не различает регистр).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Раскладка папок на диске.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Каталог на папку, подпапки — внутри (EML, MSG).
    Dirs,
    /// Maildir: каталог на папку, подпапки — внутри, рядом с `cur`, `new`, `tmp`.
    Maildir,
    /// Maildir++: все папки — в корне, `.Родитель.Ребёнок`; корень — папка без пути (INBOX).
    MaildirPlus,
    /// mbox, раскладка Thunderbird: файл `Папка`, подпапки — в каталоге `Папка.sbd/`.
    Thunderbird,
    /// mbox для Apple Mail: пакет `Папка.mbox/mbox`, подпапки — пакетами внутри пакета родителя.
    AppleMail,
}

/// Пути папок и файлов от корня выгрузки. Одна папка — всегда один путь; имена выдаются по мере обхода.
#[derive(Debug)]
pub struct Tree {
    layout: Layout,
    /// Имя папки на диске (без `.sbd`, `.mbox`) по её пути в учётке.
    names: HashMap<Vec<String>, String>,
    /// Занятые имена в нижнем регистре по каталогам.
    taken: HashMap<PathBuf, HashSet<String>>,
}

impl Tree {
    pub fn new(layout: Layout) -> Self {
        Self { layout, names: HashMap::new(), taken: HashMap::new() }
    }

    /// Путь папки: каталог (`Dirs`, `Maildir`, `MaildirPlus`) или файл mbox (`Thunderbird`, `AppleMail`).
    /// Пустой путь — корень выгрузки; у mbox корню нужен файл, он получает имя `_`.
    pub fn folder(&mut self, path: &[String]) -> PathBuf {
        match self.layout {
            Layout::Dirs | Layout::Maildir | Layout::MaildirPlus => self.children(path),
            _ if path.is_empty() => self.folder(&[String::new()]),
            Layout::Thunderbird => {
                let name = self.name(path);
                self.children(&path[..path.len() - 1]).join(name)
            }
            Layout::AppleMail => self.children(path).join("mbox"),
        }
    }

    /// Свободное имя файла в каталоге `dir` (путь от корня выгрузки): «Тема.eml», «Тема (2).eml»…
    pub fn unique(&mut self, dir: &Path, name: &str) -> String {
        let safe = safe_name(name);
        let (stem, ext) = match safe.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_alphanumeric()) => (s.to_string(), format!(".{e}")),
            _ => (safe.clone(), String::new()),
        };
        let taken = self.taken(dir);
        let mut candidate = safe;
        let mut n = 2;
        while !taken.insert(candidate.to_lowercase()) {
            candidate = format!("{stem} ({n}){ext}");
            n += 1;
        }
        candidate
    }

    /// Каталог, где лежат подпапки папки `path` (для `MaildirPlus` — ключ соседей `.Родитель`).
    fn children(&mut self, path: &[String]) -> PathBuf {
        let Some((_, parent)) = path.split_last() else { return PathBuf::new() };
        let name = self.name(path);
        match self.layout {
            Layout::Dirs | Layout::Maildir => self.children(parent).join(name),
            Layout::Thunderbird => self.children(parent).join(format!("{name}.sbd")),
            Layout::AppleMail => self.children(parent).join(format!("{name}.mbox")),
            Layout::MaildirPlus => {
                let names: Vec<String> = (1..=path.len()).map(|i| self.name(&path[..i])).collect();
                PathBuf::from(format!(".{}", names.join(".")))
            }
        }
    }

    /// Имя папки на диске; соседям — разные имена с учётом `.sbd` и `.mbox`.
    fn name(&mut self, path: &[String]) -> String {
        if let Some(name) = self.names.get(path) {
            return name.clone();
        }
        let Some((last, parent)) = path.split_last() else { return String::new() };
        let mut base = safe_name(last);
        if self.layout == Layout::MaildirPlus {
            // Точка — разделитель уровней Maildir++.
            base = base.replace('.', "_");
        }
        let dir = self.children(parent);
        let layout = self.layout;
        let forms = |n: &str| -> Vec<String> {
            match layout {
                Layout::Dirs | Layout::Maildir | Layout::MaildirPlus => vec![n.to_lowercase()],
                Layout::Thunderbird => vec![n.to_lowercase(), format!("{n}.sbd").to_lowercase()],
                Layout::AppleMail => vec![format!("{n}.mbox").to_lowercase()],
            }
        };
        let taken = self.taken(&dir);
        let mut candidate = base.clone();
        let mut n = 2;
        while forms(&candidate).iter().any(|f| taken.contains(f)) {
            candidate = format!("{base} ({n})");
            n += 1;
        }
        taken.extend(forms(&candidate));
        self.names.insert(path.to_vec(), candidate.clone());
        candidate
    }

    fn taken(&mut self, dir: &Path) -> &mut HashSet<String> {
        let maildir = self.layout == Layout::Maildir;
        self.taken
            .entry(dir.to_path_buf())
            .or_insert_with(|| if maildir { crate::maildir::DIRS.iter().map(|d| d.to_string()).collect() } else { HashSet::new() })
    }
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "com¹", "com²",
    "com³", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "lpt¹", "lpt²", "lpt³",
];

/// Имя файла, безопасное для любой ФС: без путей, управляющих и bidi-символов, без запрещённых символов и
/// зарезервированных имён Windows, не длиннее 150 символов. Те же правила, что у `files::safe_name` ядра
/// (`export` от ядра не зависит), плюс `CONIN$`, `COM0`, `COM¹`… и пробел перед расширением (`NUL .txt`).
pub fn safe_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    let stem = s.split('.').next().unwrap_or("").trim_end().to_lowercase();
    if s.is_empty() || RESERVED.contains(&stem.as_str()) {
        s = format!("_{s}");
    }
    if s.chars().count() > 150 {
        let ext = s.rsplit_once('.').map(|(_, e)| e.to_string()).filter(|e| e.chars().count() <= 10).unwrap_or_default();
        let keep: String = s.chars().take(140).collect();
        s = if ext.is_empty() { keep } else { format!("{keep}.{ext}") };
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn windows_names() {
        assert_eq!(safe_name("CON"), "_CON");
        assert_eq!(safe_name("nul.txt"), "_nul.txt");
        assert_eq!(safe_name("NUL .eml"), "_NUL .eml");
        assert_eq!(safe_name("Com1"), "_Com1");
        assert_eq!(safe_name("conin$"), "_conin$");
        assert_eq!(safe_name("Console"), "Console");
        assert_eq!(safe_name("Re: отчёт? <да>|нет*"), "Re_ отчёт_ _да__нет_");
        assert_eq!(safe_name("a/b\\c"), "a_b_c");
        assert_eq!(safe_name("Итоги. . "), "Итоги");
        assert_eq!(safe_name("счёт\u{202e}fdp.exe\n"), "счётfdp.exe");
        assert_eq!(safe_name(""), "_");
        let long = format!("{}.eml", "я".repeat(300));
        let safe = safe_name(&long);
        assert_eq!(safe.chars().count(), 144);
        assert!(safe.ends_with(".eml"));
    }

    #[test]
    fn folders_as_dirs_and_collisions() {
        let mut t = Tree::new(Layout::Dirs);
        assert_eq!(t.folder(&[]), PathBuf::new());
        assert_eq!(t.folder(&p(&["Входящие", "Проекты"])), Path::new("Входящие").join("Проекты"));
        assert_eq!(t.folder(&p(&["AUX"])), PathBuf::from("_AUX"));
        // «A:» и «A_» после замены совпадают, «Work» и «work» — для Windows тоже.
        assert_eq!(t.folder(&p(&["A:"])), PathBuf::from("A_"));
        assert_eq!(t.folder(&p(&["A_"])), PathBuf::from("A_ (2)"));
        assert_eq!(t.folder(&p(&["Work"])), PathBuf::from("Work"));
        assert_eq!(t.folder(&p(&["work"])), PathBuf::from("work (2)"));
        // Повторный запрос — тот же путь.
        assert_eq!(t.folder(&p(&["A_"])), PathBuf::from("A_ (2)"));
        assert_eq!(t.unique(Path::new("Work"), "Отчёт.eml"), "Отчёт.eml");
        assert_eq!(t.unique(Path::new("Work"), "отчёт.eml"), "отчёт (2).eml");
        assert_eq!(t.unique(Path::new("Work"), "Отчёт.eml"), "Отчёт (3).eml");
        assert_eq!(t.unique(Path::new(""), "work.eml"), "work.eml");
    }

    #[test]
    fn three_levels_thunderbird() {
        let mut t = Tree::new(Layout::Thunderbird);
        assert_eq!(t.folder(&p(&["Архив"])), PathBuf::from("Архив"));
        assert_eq!(t.folder(&p(&["Архив", "2026", "Октябрь"])), Path::new("Архив.sbd").join("2026.sbd").join("Октябрь"));
        assert_eq!(t.folder(&p(&["Архив", "2026"])), Path::new("Архив.sbd").join("2026"));
        // Папка с именем чужого `.sbd` не занимает его.
        assert_eq!(t.folder(&p(&["Архив.sbd"])), PathBuf::from("Архив.sbd (2)"));
        assert_eq!(t.folder(&[]), PathBuf::from("_"));
    }

    #[test]
    fn three_levels_apple_mail() {
        let mut t = Tree::new(Layout::AppleMail);
        assert_eq!(t.folder(&p(&["Архив", "2026", "Октябрь"])), Path::new("Архив.mbox").join("2026.mbox").join("Октябрь.mbox").join("mbox"));
        assert_eq!(t.folder(&p(&["Архив"])), Path::new("Архив.mbox").join("mbox"));
        assert_eq!(t.folder(&p(&["Архив", "mbox"])), Path::new("Архив.mbox").join("mbox.mbox").join("mbox"));
    }

    #[test]
    fn maildir_layouts() {
        let mut t = Tree::new(Layout::Maildir);
        assert_eq!(t.folder(&p(&["Входящие", "cur"])), Path::new("Входящие").join("cur (2)"));
        assert_eq!(t.folder(&p(&["tmp"])), PathBuf::from("tmp (2)"));
        let mut t = Tree::new(Layout::MaildirPlus);
        assert_eq!(t.folder(&[]), PathBuf::new());
        assert_eq!(t.folder(&p(&["Архив"])), PathBuf::from(".Архив"));
        assert_eq!(t.folder(&p(&["Архив", "v1.2", "Октябрь"])), PathBuf::from(".Архив.v1_2.Октябрь"));
        assert_eq!(t.folder(&p(&["Архив", "v1_2"])), PathBuf::from(".Архив.v1_2 (2)"));
    }
}
