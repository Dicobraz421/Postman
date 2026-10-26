//! Журнал в файл и отчёт для поддержки (S5-30, «Настройки → Общие»). Журнал — `<данные>\logs\postman-ДАТА.log`
//! (процесс без консоли пишет туда поток ошибок, `sys::shell::log_to_file`), старше недели — удаляется при запуске.
//! Отчёт — архив: журнал за неделю, версия, настройки без адресов; паролей, токенов и текстов писем в нём нет.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use postman_core::prefs::Prefs;

/// Сколько хранить журнал.
const KEEP: Duration = Duration::from_secs(7 * 86_400);
/// Обновления — по явному действию: страница сборок (в фоне сети нет).
pub(crate) const RELEASES: &str = "https://github.com/Dicobraz/Postman/actions";

pub(crate) fn log_dir(data: &Path) -> PathBuf {
    data.join("logs")
}

fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

/// Перед `init_logging`: старое — удалить, поток ошибок — в файл дня (если консоли нет).
pub(crate) fn start_log_file(data: &Path) {
    let dir = log_dir(data);
    prune(&dir, SystemTime::now());
    crate::sys::shell::log_to_file(&dir.join(format!("postman-{}.log", today())));
}

/// Файлы журнала старше недели.
fn prune(dir: &Path, now: SystemTime) {
    for path in logs(dir) {
        let old = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn logs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "log")).collect();
    out.sort();
    out
}

/// Сколько байт в папке журнала.
pub(crate) fn folder_size(dir: &Path) -> u64 {
    logs(dir).iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum()
}

/// Куда положить отчёт: рабочий стол, без него — папка загрузок или временная.
pub(crate) fn report_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let dir = home.iter().flat_map(|h| [h.join("Desktop"), h.join("Downloads")]).find(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("postman-report-{}.zip", today()))
}

/// Настройки для отчёта: без адресов (адрес ИБ) — остальное не личное.
fn redacted(prefs: &Prefs) -> Prefs {
    let mut p = prefs.clone();
    p.general.phishing_report = p.general.phishing_report.as_ref().map(|_| "<address>".to_owned());
    p
}

/// Собрать отчёт: `logs/…`, `version.txt`, `settings.json`.
pub(crate) fn build_report(data: &Path, prefs: &Prefs, out: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let err = |e: zip::result::ZipError| e.to_string();
    let io = |e: std::io::Error| e.to_string();
    zip.start_file("version.txt", opts).map_err(err)?;
    let version = format!(
        "Postman {}\nos: {} {}\nlanguage: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        prefs.general.language.as_deref().unwrap_or("auto")
    );
    zip.write_all(version.as_bytes()).map_err(io)?;
    zip.start_file("settings.json", opts).map_err(err)?;
    let settings = serde_json::to_string_pretty(&redacted(prefs)).map_err(|e| e.to_string())?;
    zip.write_all(settings.as_bytes()).map_err(io)?;
    for path in logs(&log_dir(data)) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(text) = std::fs::read(&path) else { continue };
        zip.start_file(format!("logs/{name}"), opts).map_err(err)?;
        zip.write_all(&text).map_err(io)?;
    }
    zip.finish().map_err(err)?;
    Ok(())
}

/// Команда, которая даёт AppContainer доступ на чтение к папке программы (полная песочница).
pub(crate) fn icacls_command() -> String {
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    format!("icacls \"{}\" /grant \"*S-1-15-2-1:(OI)(CI)(RX)\" /T", dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Отчёт: версия, настройки без адреса ИБ, журнал; старые файлы журнала удаляются.
    #[test]
    fn report_has_logs_version_and_redacted_settings() {
        let d = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        let logs = log_dir(d.path());
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("postman-2026-10-21.log"), "INFO started\n").unwrap();
        std::fs::write(logs.join("notes.txt"), "x").unwrap();
        let mut prefs = Prefs::default();
        prefs.general.phishing_report = Some("soc@corp.example".into());
        let out = d.path().join("report.zip");
        build_report(d.path(), &prefs, &out).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().unwrap().to_string()).collect();
        names.sort();
        assert_eq!(names, ["logs/postman-2026-10-21.log", "settings.json", "version.txt"]);
        let mut settings = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("settings.json").unwrap(), &mut settings).unwrap();
        assert!(!settings.contains("soc@corp.example"), "адреса в отчёт не попадают");
        assert_eq!(folder_size(&logs), "INFO started\n".len() as u64);
        // Через восемь дней — файл журнала удалён, чужие файлы — нет.
        prune(&logs, SystemTime::now() + Duration::from_secs(8 * 86_400));
        assert!(logs.join("notes.txt").exists());
        assert!(!logs.join("postman-2026-10-21.log").exists());
    }
}
