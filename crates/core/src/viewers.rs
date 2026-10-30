//! Модули просмотра WASM (S8-26, ADR 0007, решение A): поиск, описание и выбор. Показывает модуль хост
//! `postman-viewer` ([`crate::viewer::Viewer`]); здесь — какие модули есть и какой взять для файла.
//!
//! Модуль — пара `<имя>.json` (описание) + `<имя>.wasm` в каталоге модулей ([`dirs`]): сначала системный
//! (`%ProgramFiles%\Postman\viewers`, ставит администратор), затем пользовательский (`%LOCALAPPDATA%\Postman\viewers`,
//! в просмотре — «сторонний»). Реестр читается один раз при старте в своём потоке ([`Registry::spawn`]); плохое
//! описание или модуль — пропуск с записью в журнал. Модуль с тем же именем у пользователя, что и у системы, —
//! пропуск: системный главнее.
//!
//! Описание (JSON): `name`, `version`, `abi` (= [`wasm::ABI`]), `extensions` (без точки), `mime` (без параметров),
//! `signatures` (`prefix` — с начала файла, `contains` — в первых [`HEAD`] байтах; текстом UTF-8 или массивом байтов),
//! `limits` (`input`, `memory`, `fuel`, `output` — не больше общих пределов хоста; нет — общий). Хотя бы одно из
//! расширений, MIME и сигнатур — иначе модуль ни к чему не подходит.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use postman_worker::wasm;
use serde::Deserialize;

/// Сигнатуры сверяются с первыми байтами файла не длиннее.
pub const HEAD: usize = 4096;
/// Описание больше — не читается.
pub const MAX_MANIFEST: u64 = 64 * 1024;
/// Модулей больше — остальные пропускаются (модуль держится в памяти целиком, до [`wasm::MAX_MODULE`]).
pub const MAX_MODULES: usize = 32;
/// Имя и версия модуля — не длиннее (символов).
const MAX_TEXT: usize = 64;

/// Почему модуль пропущен (в журнал).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(String),
    #[error("description is larger than {MAX_MANIFEST} bytes")]
    TooLarge,
    #[error("description: {0}")]
    Json(String),
    #[error("ABI {0} is not supported")]
    Abi(u32),
    #[error("bad name or version")]
    Name,
    #[error("bad extension {0:?}")]
    Extension(String),
    #[error("bad MIME type {0:?}")]
    Mime(String),
    #[error("bad signature")]
    Signature,
    #[error("no extensions, MIME types or signatures")]
    Nothing,
    #[error("limit {0} is above the host limit")]
    Limit(&'static str),
    #[error("module is not WebAssembly or larger than the host limit")]
    Wasm,
}

/// Пределы модуля: не больше общих пределов хоста ([`postman_worker::wasm`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Файл больше модулю не передаётся.
    pub input: usize,
    pub memory: usize,
    pub fuel: u64,
    pub output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { input: wasm::MAX_INPUT, memory: wasm::MAX_MEMORY, fuel: wasm::FUEL, output: wasm::MAX_OUTPUT }
    }
}

impl Limits {
    fn checked(self) -> Result<Self, Error> {
        let host = Limits::default();
        let over = [
            ("input", self.input as u64, host.input as u64),
            ("memory", self.memory as u64, host.memory as u64),
            ("fuel", self.fuel, host.fuel),
            ("output", self.output as u64, host.output as u64),
        ];
        match over.into_iter().find(|&(_, own, host)| own == 0 || own > host) {
            Some((name, ..)) => Err(Error::Limit(name)),
            None => Ok(self),
        }
    }
}

/// Сигнатура: первые байты файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub prefix: Vec<u8>,
    /// Ещё и эти байты где-то в первых [`HEAD`].
    pub contains: Option<Vec<u8>>,
}

impl Signature {
    pub fn matches(&self, head: &[u8]) -> bool {
        let head = &head[..head.len().min(HEAD)];
        head.starts_with(&self.prefix) && self.contains.as_ref().is_none_or(|c| head.windows(c.len()).any(|w| w == c.as_slice()))
    }
}

/// Проверенное описание модуля.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    pub name: String,
    pub version: String,
    /// Строчными, без точки.
    pub extensions: Vec<String>,
    /// Строчными, без параметров.
    pub mime: Vec<String>,
    pub signatures: Vec<Signature>,
    pub limits: Limits,
}

#[derive(Deserialize)]
struct Manifest {
    name: String,
    version: String,
    abi: u32,
    #[serde(default)]
    extensions: Vec<String>,
    #[serde(default)]
    mime: Vec<String>,
    #[serde(default)]
    signatures: Vec<RawSignature>,
    #[serde(default)]
    limits: Limits,
}

#[derive(Deserialize)]
struct RawSignature {
    prefix: Bytes,
    #[serde(default)]
    contains: Option<Bytes>,
}

/// Байты сигнатуры: текстом (UTF-8) или массивом чисел.
#[derive(Deserialize)]
#[serde(untagged)]
enum Bytes {
    Text(String),
    Raw(Vec<u8>),
}

impl Bytes {
    fn bytes(self) -> Result<Vec<u8>, Error> {
        let b = match self {
            Bytes::Text(s) => s.into_bytes(),
            Bytes::Raw(b) => b,
        };
        if b.is_empty() || b.len() > HEAD { Err(Error::Signature) } else { Ok(b) }
    }
}

impl Description {
    /// Описание `<имя>.json`: всё, что модуль объявил, проверено.
    pub fn parse(json: &str) -> Result<Self, Error> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| Error::Json(e.to_string()))?;
        if m.abi != wasm::ABI {
            return Err(Error::Abi(m.abi));
        }
        let text = |s: &str| !s.trim().is_empty() && s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control);
        if !text(&m.name) || !text(&m.version) {
            return Err(Error::Name);
        }
        let extensions = m
            .extensions
            .into_iter()
            .map(|e| {
                let e = e.to_ascii_lowercase();
                let good = (1..=16).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_alphanumeric());
                if good { Ok(e) } else { Err(Error::Extension(e)) }
            })
            .collect::<Result<_, _>>()?;
        let mime = m
            .mime
            .into_iter()
            .map(|t| {
                let t = t.trim().to_ascii_lowercase();
                let good = t.len() <= 128
                    && t.split_once('/').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
                    && t.bytes().all(|b| b.is_ascii_graphic() && b != b';');
                if good { Ok(t) } else { Err(Error::Mime(t)) }
            })
            .collect::<Result<_, _>>()?;
        let signatures = m
            .signatures
            .into_iter()
            .map(|s| Ok(Signature { prefix: s.prefix.bytes()?, contains: s.contains.map(Bytes::bytes).transpose()? }))
            .collect::<Result<_, Error>>()?;
        let d =
            Description { name: m.name.trim().into(), version: m.version.trim().into(), extensions, mime, signatures, limits: m.limits.checked()? };
        if d.extensions.is_empty() && d.mime.is_empty() && d.signatures.is_empty() {
            return Err(Error::Nothing);
        }
        Ok(d)
    }
}

/// Модуль в реестре.
#[derive(Debug, Clone)]
pub struct Module {
    /// Имя файла без `.json`, строчными: ключ выключенного модуля в настройках.
    pub id: String,
    pub description: Description,
    /// Из пользовательского каталога: подпись «сторонний».
    pub third_party: bool,
    /// Модуль для хоста ([`crate::viewer::Viewer::view`]).
    pub wasm: Arc<Vec<u8>>,
}

/// Модуль в реестре по номеру: реестр после чтения не меняется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(u32);

/// Каталог модулей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    pub path: PathBuf,
    /// Пользовательский: модули — «сторонние».
    pub third_party: bool,
}

/// Каталоги модулей: системный, затем пользовательский (переменных окружения нет — нет и каталога).
pub fn dirs() -> Vec<Dir> {
    let dir = |var: &str, third_party| std::env::var_os(var).map(|v| Dir { path: PathBuf::from(v).join("Postman").join("viewers"), third_party });
    [dir("ProgramFiles", false), dir("LOCALAPPDATA", true)].into_iter().flatten().collect()
}

/// Найденные модули: сначала системные, по имени файла.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    modules: Vec<Module>,
}

impl Registry {
    /// Прочитать каталоги в своём потоке; реестр — в приёмник, затем `repaint`. Поток не запустился — модулей нет
    /// (приёмник закрыт без реестра).
    pub fn spawn(dirs: Vec<Dir>, repaint: impl FnOnce() + Send + 'static) -> mpsc::Receiver<Registry> {
        let (tx, rx) = mpsc::channel();
        let started = std::thread::Builder::new().name("viewers".into()).spawn(move || {
            if tx.send(Registry::load(&dirs)).is_ok() {
                repaint();
            }
        });
        if let Err(e) = started {
            tracing::warn!("viewer modules thread not started: {e}");
        }
        rx
    }

    /// Прочитать каталоги по порядку (блокирует: файлы).
    pub fn load(dirs: &[Dir]) -> Self {
        let mut modules: Vec<Module> = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir.path) else { continue };
            let mut manifests: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) && p.is_file())
                .collect();
            manifests.sort();
            for path in manifests {
                let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase) else { continue };
                if modules.iter().any(|m| m.id == id) {
                    tracing::warn!("viewer module {}: skipped, a system module has the same name", path.display());
                    continue;
                }
                if modules.len() >= MAX_MODULES {
                    tracing::warn!("viewer module {}: skipped, more than {MAX_MODULES} modules", path.display());
                    continue;
                }
                match load_module(&path) {
                    Ok((description, wasm)) => {
                        tracing::info!("viewer module {id}: {} {}", description.name, description.version);
                        modules.push(Module { id, description, third_party: dir.third_party, wasm: Arc::new(wasm) });
                    }
                    Err(e) => tracing::warn!("viewer module {}: skipped: {e}", path.display()),
                }
            }
        }
        Self { modules }
    }

    pub fn get(&self, id: ModuleId) -> Option<&Module> {
        self.modules.get(id.0 as usize)
    }

    /// Все модули по порядку (список в «Настройках → Просмотр вложений»).
    pub fn modules(&self) -> impl Iterator<Item = (ModuleId, &Module)> {
        self.modules.iter().enumerate().map(|(i, m)| (ModuleId(i as u32), m))
    }

    /// Модуль для файла: по расширению `ext` (строчными, без точки), затем MIME `mime` (строчными, без параметров),
    /// затем по первым байтам `head`; выключенные (`off` — их [`Module::id`]) — как нет их. Из подходящих — первый:
    /// системный главнее пользовательского.
    pub fn find(&self, ext: &str, mime: &str, head: &[u8], off: &BTreeSet<String>) -> Option<ModuleId> {
        let pick = |fits: &dyn Fn(&Description) -> bool| self.modules().find(|(_, m)| !off.contains(&m.id) && fits(&m.description)).map(|(id, _)| id);
        let by_ext = || pick(&|d| d.extensions.iter().any(|e| e == ext));
        let by_mime = || pick(&|d| d.mime.iter().any(|m| m == mime));
        let by_head = || pick(&|d| d.signatures.iter().any(|s| s.matches(head)));
        (!ext.is_empty())
            .then(by_ext)
            .flatten()
            .or_else(|| (!mime.is_empty()).then(by_mime).flatten())
            .or_else(|| (!head.is_empty()).then(by_head).flatten())
    }
}

/// Описание `path` и модуль рядом (`<имя>.wasm`).
fn load_module(path: &Path) -> Result<(Description, Vec<u8>), Error> {
    let io = |e: std::io::Error| Error::Io(e.to_string());
    if std::fs::metadata(path).map_err(io)?.len() > MAX_MANIFEST {
        return Err(Error::TooLarge);
    }
    let description = Description::parse(&std::fs::read_to_string(path).map_err(io)?)?;
    let wasm_path = path.with_extension("wasm");
    if std::fs::metadata(&wasm_path).map_err(io)?.len() > wasm::MAX_MODULE as u64 {
        return Err(Error::Wasm);
    }
    let wasm = std::fs::read(&wasm_path).map_err(io)?;
    // Двоичный модуль WASM версии 1; полную проверку делает хост.
    if !wasm.starts_with(b"\0asm\x01\0\0\0") || wasm.len() > wasm::MAX_MODULE {
        return Err(Error::Wasm);
    }
    Ok((description, wasm))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FB2: &str = r#"{"name": "FB2", "version": "1.0", "abi": 1, "extensions": ["FB2"], "mime": ["application/x-fictionbook+xml"],
        "signatures": [{"prefix": "<?xml", "contains": "<FictionBook"}, {"prefix": [80, 75, 3, 4]}], "limits": {"memory": 134217728}}"#;

    #[test]
    fn description_is_checked() {
        let d = Description::parse(FB2).unwrap();
        assert_eq!((d.name.as_str(), d.version.as_str()), ("FB2", "1.0"));
        assert_eq!(d.extensions, ["fb2"]);
        assert_eq!(d.limits, Limits { memory: 128 * 1024 * 1024, ..Limits::default() });
        assert!(d.signatures[0].matches(b"<?xml version=\"1.0\"?>\n<FictionBook xmlns=\"\">"));
        assert!(!d.signatures[0].matches(b"<?xml version=\"1.0\"?><html>"));
        assert!(d.signatures[1].matches(b"PK\x03\x04rest"));
        let fb2_far = [b"<?xml".as_slice(), &[b' '; HEAD], b"<FictionBook"].concat();
        assert!(!d.signatures[0].matches(&fb2_far), "только первые HEAD байт");

        let with = |field: &str, value: &str| {
            let mut v: serde_json::Value = serde_json::from_str(FB2).unwrap();
            v[field] = serde_json::from_str(value).unwrap();
            Description::parse(&v.to_string())
        };
        assert_eq!(with("abi", "2"), Err(Error::Abi(2)));
        assert_eq!(with("name", r#"" ""#), Err(Error::Name));
        assert_eq!(with("extensions", r#"[".fb2"]"#), Err(Error::Extension(".fb2".into())));
        assert_eq!(with("mime", r#"["text/xml; charset=utf-8"]"#), Err(Error::Mime("text/xml; charset=utf-8".into())));
        assert_eq!(with("signatures", r#"[{"prefix": ""}]"#), Err(Error::Signature));
        assert!(matches!(with("abi", r#""1""#), Err(Error::Json(_))));
        let mut bare: serde_json::Value = serde_json::from_str(FB2).unwrap();
        for f in ["extensions", "mime", "signatures"] {
            bare.as_object_mut().unwrap().remove(f);
        }
        assert_eq!(Description::parse(&bare.to_string()), Err(Error::Nothing));
    }

    /// Предел больше общего (или нулевой) — модуль не принимается; равный общему — можно.
    #[test]
    fn limit_above_the_host_limit_is_refused() {
        let limits = |l: &str| {
            let mut v: serde_json::Value = serde_json::from_str(FB2).unwrap();
            v["limits"] = serde_json::from_str(l).unwrap();
            Description::parse(&v.to_string())
        };
        assert_eq!(limits(&format!(r#"{{"memory": {}}}"#, wasm::MAX_MEMORY + 1)), Err(Error::Limit("memory")));
        assert_eq!(limits(&format!(r#"{{"fuel": {}}}"#, wasm::FUEL * 2)), Err(Error::Limit("fuel")));
        assert_eq!(limits(&format!(r#"{{"input": {}}}"#, wasm::MAX_INPUT + 1)), Err(Error::Limit("input")));
        assert_eq!(limits(r#"{"output": 0}"#), Err(Error::Limit("output")));
        assert_eq!(limits(&format!(r#"{{"output": {}}}"#, wasm::MAX_OUTPUT)).unwrap().limits, Limits::default());
    }

    fn put(dir: &Path, name: &str, json: &str, wasm: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
        std::fs::write(dir.join(format!("{name}.wasm")), wasm).unwrap();
    }

    const WASM: &[u8] = b"\0asm\x01\0\0\0";

    /// Два модуля на одно расширение: системный главнее пользовательского; выключенный — как нет его.
    #[test]
    fn system_module_wins_over_user_module() {
        let root = tempfile::tempdir().unwrap();
        let (system, user) = (root.path().join("system"), root.path().join("user"));
        put(&system, "fb2", FB2, WASM);
        put(&user, "books", FB2, WASM);
        // То же имя, что у системного, — пропуск; плохие — пропуск.
        put(&user, "FB2", FB2, WASM);
        put(&user, "broken", "{", WASM);
        put(&user, "notwasm", &FB2.replace("FB2\"]", "fb3\"]"), b"MZ");
        std::fs::write(user.join("nowasm.json"), FB2).unwrap();
        let dirs = vec![
            Dir { path: system, third_party: false },
            Dir { path: user, third_party: true },
            Dir { path: root.path().join("none"), third_party: true },
        ];
        let r = Registry::load(&dirs);
        let ids: Vec<_> = r.modules().map(|(_, m)| (m.id.as_str(), m.third_party)).collect();
        assert_eq!(ids, [("fb2", false), ("books", true)]);

        let none = BTreeSet::new();
        let id = r.find("fb2", "", b"", &none).unwrap();
        assert_eq!(r.get(id).map(|m| (m.id.as_str(), m.third_party)), Some(("fb2", false)));
        let off = BTreeSet::from(["fb2".to_string()]);
        let id = r.find("fb2", "", b"", &off).unwrap();
        assert_eq!(r.get(id).map(|m| (m.id.as_str(), m.third_party)), Some(("books", true)));
        assert_eq!(r.find("fb2", "", b"", &BTreeSet::from(["fb2".into(), "books".into()])), None);
        // По MIME и по сигнатуре — когда расширение не подошло.
        assert_eq!(r.find("xml", "application/x-fictionbook+xml", b"", &none), r.find("fb2", "", b"", &none));
        assert_eq!(r.find("", "", b"<?xml version=\"1.0\"?><FictionBook>", &none), r.find("fb2", "", b"", &none));
        assert_eq!(r.find("txt", "text/plain", b"plain text", &none), None);

        let rx = Registry::spawn(dirs, || {});
        assert_eq!(rx.recv().unwrap().modules().count(), 2);
    }
}
