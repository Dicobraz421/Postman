//! Пакет MSI (решение владельца 2026-11-07): установка для всех пользователей в `Program Files\Postman`.
//!
//! Без своих окон: Windows Installer показывает базовый ход установки и запрос UAC. Ставит файлы комплекта,
//! ярлык «Postman» в «Пуске», источник «Postman» журнала «Приложение» (как `install.ps1 -RegisterEventSource`).
//! Новая версия снимает старую (большое обновление по `UPGRADE_CODE`), старую поверх новой не ставит.
//! Права для песочниц не нужны: `Program Files` уже читаем для ALL APPLICATION PACKAGES.
//! Данные пользователя (`%LOCALAPPDATA%\Postman`) и сохранённый вход удаление не трогает.

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use msi::{Category, CodePage, Column, Insert, Package, PackageType, Value};
use sha2::{Digest, Sha256};

use crate::ids::{UPGRADE_CODE, component_code, msi_version, product_code, sha256_hex, uuid};
use crate::pe;

/// Производитель в «Приложениях» и в winget — семейство Pets (как AUMID `Pets.Postman`).
pub const MANUFACTURER: &str = "Pets";
const PRODUCT: &str = "Postman";
const CABINET: &str = "postman.cab";
const ICON: &str = "postman.ico";
const ICON_DATA: &[u8] = include_bytes!("../../../app-gpui/res/postman.ico");
const EVENT_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\Application\Postman";
/// Файл сообщений .NET Framework 4 — «%1» для любого кода: журнал показывает текст Postman как есть.
const EVENT_MESSAGES: &str = r"#%%SystemRoot%\Microsoft.NET\Framework64\v4.0.30319\EventLogMessages.dll";
/// Компонент 64-битный: реестр и папки — без перенаправления WOW64.
const COMPONENT_64BIT: i32 = 256;
const FILE_VITAL: i32 = 512;

/// Файл комплекта: ключ в таблицах (идентификатор MSI), имя в комплекте и на диске (`короткое|длинное`).
struct Item {
    key: &'static str,
    name: &'static str,
    file_name: &'static str,
}

const ITEMS: [Item; 4] = [
    Item { key: "postman.exe", name: "postman.exe", file_name: "postman.exe" },
    Item { key: "worker.exe", name: "postman-worker.exe", file_name: "POSTMA~1.EXE|postman-worker.exe" },
    Item { key: "viewer.exe", name: "postman-viewer.exe", file_name: "POSTMA~2.EXE|postman-viewer.exe" },
    Item { key: "README.txt", name: "README.txt", file_name: "README.txt" },
];

/// Собрать `out` из файлов каталога комплекта `dir` (выход `package.ps1`). Версия — версия сборки (Cargo);
/// у exe она же должна быть в ресурсе версии, иначе комплект от другой сборки. `check_files: false` — только
/// для проверки обновления в CI (`msi-check.ps1`): тот же комплект под другой версией.
pub fn build(dir: &Path, out: &Path, version: &str, check_files: bool) -> Result<()> {
    let msi_ver = msi_version(version)?;
    let core = version.split('-').next().unwrap_or(version);
    let mut files = Vec::new();
    for item in &ITEMS {
        let path = dir.join(item.name);
        if !path.exists() {
            bail!("{} not found in {}", item.name, dir.display());
        }
        let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let file_version = if item.name.ends_with(".exe") {
            let v = pe::file_version(&data).with_context(|| format!("{}: no version resource", item.name))?;
            if check_files && v != format!("{core}.0") {
                bail!("{}: file version {v}, expected {core}.0 (built by another version)", item.name);
            }
            Some(v)
        } else {
            None
        };
        files.push((item, data, file_version));
    }

    let mut hash = Sha256::new();
    hash.update(version.as_bytes());
    for (item, data, _) in &files {
        hash.update(item.key.as_bytes());
        hash.update(Sha256::digest(data));
    }
    let package_code = uuid(&format!("Pets.Postman package {}", sha256_hex(&hash.finalize())));

    let cab = cabinet(&files)?;
    let file =
        fs::File::options().read(true).write(true).create(true).truncate(true).open(out).with_context(|| format!("create {}", out.display()))?;
    let mut package = Package::create(PackageType::Installer, file)?;
    // UTF-8 (по умолчанию у крейта) Windows Installer в сводке не читает: строки пусты, пакет — «не открыть» (1620).
    // Все строки пакета — латиница.
    package.set_database_codepage(CodePage::Windows1252);
    summary(&mut package, version, package_code);
    tables(&mut package, version, &msi_ver, &files)?;
    package.write_stream(CABINET)?.write_all(&cab)?;
    package.write_stream(&format!("Icon.{ICON}"))?.write_all(ICON_DATA)?;
    package.flush()?;
    Ok(())
}

type Files<'a> = [(&'a Item, Vec<u8>, Option<String>)];

/// Встроенный CAB: файлы под ключами таблицы `File`, в порядке их `Sequence`.
fn cabinet(files: &Files) -> Result<Vec<u8>> {
    let mut builder = cab::CabinetBuilder::new();
    let folder = builder.add_folder(cab::CompressionType::MsZip);
    for (item, _, _) in files {
        folder.add_file(item.key);
    }
    let mut writer = builder.build(Cursor::new(Vec::new()))?;
    let mut i = 0;
    while let Some(mut w) = writer.next_file()? {
        w.write_all(&files[i].1)?;
        i += 1;
    }
    Ok(writer.finish()?.into_inner())
}

fn summary<F: std::io::Read + std::io::Write + std::io::Seek>(package: &mut Package<F>, version: &str, package_code: uuid::Uuid) {
    let s = package.summary_info_mut();
    s.set_codepage(CodePage::Windows1252);
    s.set_title("Installation Database");
    s.set_subject(PRODUCT);
    s.set_author(MANUFACTURER);
    s.set_comments(format!("{PRODUCT} {version}: mail, calendar and contacts"));
    s.set_creating_application("postman-pack");
    s.set_arch("x64");
    s.set_languages(&[msi::LanguageId::from_id(1033)]);
    s.set_uuid(package_code);
    // 500 — схема Windows Installer 5 (x64 требует не меньше 200); 2 — файлы сжаты (в CAB), UAC нужен.
    s.set_page_count(500);
    s.set_word_count(2);
    s.set_doc_security(2);
    s.set_creation_time_to_now();
}

fn s(v: &str) -> Value {
    Value::Str(v.to_owned())
}
fn i(v: i32) -> Value {
    Value::Int(v)
}
const NULL: Value = Value::Null;

fn tables<F: std::io::Read + std::io::Write + std::io::Seek>(p: &mut Package<F>, version: &str, msi_ver: &str, files: &Files) -> Result<()> {
    let product_code = product_code(version);
    let main = "postman.exe";

    p.create_table("Property", vec![Column::build("Property").primary_key().id_string(72), Column::build("Value").localizable().text_string(0)])?;
    let props = [
        ("ProductCode", product_code.as_str()),
        ("ProductName", PRODUCT),
        ("ProductVersion", msi_ver),
        ("ProductLanguage", "1033"),
        ("Manufacturer", MANUFACTURER),
        ("UpgradeCode", UPGRADE_CODE),
        ("ALLUSERS", "1"),
        ("ARPPRODUCTICON", ICON),
        ("ARPNOMODIFY", "1"),
        ("ARPURLINFOABOUT", "https://github.com/Dicobraz421/Postman"),
        ("ARPHELPLINK", "https://github.com/Dicobraz421/Postman/blob/main/docs/USER-GUIDE.md"),
        ("ARPCOMMENTS", "Mail, calendar and contacts"),
        ("SecureCustomProperties", "INSTALLDIR;NEWERFOUND;OLDERFOUND"),
    ];
    p.insert_rows(Insert::into("Property").rows(props.iter().map(|(k, v)| vec![s(k), s(v)]).collect()))?;

    p.create_table(
        "Directory",
        vec![
            Column::build("Directory").primary_key().id_string(72),
            Column::build("Directory_Parent").nullable().id_string(72),
            Column::build("DefaultDir").localizable().category(Category::DefaultDir).string(255),
        ],
    )?;
    p.insert_rows(Insert::into("Directory").rows(vec![
        vec![s("TARGETDIR"), NULL, s("SourceDir")],
        vec![s("ProgramFiles64Folder"), s("TARGETDIR"), s(".")],
        vec![s("INSTALLDIR"), s("ProgramFiles64Folder"), s(PRODUCT)],
        vec![s("ProgramMenuFolder"), s("TARGETDIR"), s(".")],
    ]))?;

    p.create_table(
        "Component",
        vec![
            Column::build("Component").primary_key().id_string(72),
            Column::build("ComponentId").nullable().category(Category::Guid).string(38),
            Column::build("Directory_").id_string(72),
            Column::build("Attributes").int16(),
            Column::build("Condition").nullable().category(Category::Condition).string(255),
            Column::build("KeyPath").nullable().id_string(72),
        ],
    )?;
    p.insert_rows(
        Insert::into("Component").rows(
            files
                .iter()
                .map(|(item, _, _)| vec![s(item.key), s(&component_code(item.name)), s("INSTALLDIR"), i(COMPONENT_64BIT), NULL, s(item.key)])
                .collect(),
        ),
    )?;

    p.create_table(
        "Feature",
        vec![
            Column::build("Feature").primary_key().id_string(38),
            Column::build("Feature_Parent").nullable().id_string(38),
            Column::build("Title").nullable().localizable().text_string(64),
            Column::build("Description").nullable().localizable().text_string(255),
            Column::build("Display").nullable().int16(),
            Column::build("Level").int16(),
            Column::build("Directory_").nullable().id_string(72),
            Column::build("Attributes").int16(),
        ],
    )?;
    p.insert_rows(Insert::into("Feature").row(vec![s("Main"), NULL, s(PRODUCT), NULL, i(1), i(1), s("INSTALLDIR"), i(0)]))?;

    p.create_table(
        "FeatureComponents",
        vec![Column::build("Feature_").primary_key().id_string(38), Column::build("Component_").primary_key().id_string(72)],
    )?;
    p.insert_rows(Insert::into("FeatureComponents").rows(files.iter().map(|(item, _, _)| vec![s("Main"), s(item.key)]).collect()))?;

    p.create_table(
        "File",
        vec![
            Column::build("File").primary_key().id_string(72),
            Column::build("Component_").id_string(72),
            Column::build("FileName").localizable().category(Category::Filename).string(255),
            Column::build("FileSize").int32(),
            Column::build("Version").nullable().category(Category::Version).string(72),
            Column::build("Language").nullable().category(Category::Language).string(20),
            Column::build("Attributes").nullable().int16(),
            Column::build("Sequence").int32(),
        ],
    )?;
    let mut rows = Vec::new();
    for (n, (item, data, ver)) in files.iter().enumerate() {
        let size = i32::try_from(data.len()).context("file over 2 GiB")?;
        let attrs = if ver.is_some() { i(FILE_VITAL) } else { i(0) };
        let ver = ver.as_deref().map_or(NULL, s);
        rows.push(vec![s(item.key), s(item.key), s(item.file_name), i(size), ver, NULL, attrs, i(n as i32 + 1)]);
    }
    p.insert_rows(Insert::into("File").rows(rows))?;

    p.create_table(
        "Media",
        vec![
            Column::build("DiskId").primary_key().int16(),
            Column::build("LastSequence").int32(),
            Column::build("DiskPrompt").nullable().localizable().text_string(64),
            Column::build("Cabinet").nullable().category(Category::Cabinet).string(255),
            Column::build("VolumeLabel").nullable().text_string(32),
            Column::build("Source").nullable().category(Category::Property).string(72),
        ],
    )?;
    p.insert_rows(Insert::into("Media").row(vec![i(1), i(files.len() as i32), NULL, s(&format!("#{CABINET}")), NULL, NULL]))?;

    p.create_table(
        "Shortcut",
        vec![
            Column::build("Shortcut").primary_key().id_string(72),
            Column::build("Directory_").id_string(72),
            Column::build("Name").localizable().category(Category::Filename).string(128),
            Column::build("Component_").id_string(72),
            Column::build("Target").category(Category::Shortcut).string(72),
            Column::build("Arguments").nullable().formatted_string(255),
            Column::build("Description").nullable().localizable().text_string(255),
            Column::build("Hotkey").nullable().int16(),
            Column::build("Icon_").nullable().id_string(72),
            Column::build("IconIndex").nullable().int16(),
            Column::build("ShowCmd").nullable().int16(),
            Column::build("WkDir").nullable().id_string(72),
        ],
    )?;
    p.insert_rows(Insert::into("Shortcut").row(vec![
        s("StartMenu"),
        s("ProgramMenuFolder"),
        s(PRODUCT),
        s(main),
        s(&format!("[#{main}]")),
        NULL,
        s("Mail, calendar and contacts"),
        NULL,
        NULL,
        NULL,
        NULL,
        s("INSTALLDIR"),
    ]))?;

    p.create_table(
        "Registry",
        vec![
            Column::build("Registry").primary_key().id_string(72),
            Column::build("Root").int16(),
            Column::build("Key").localizable().category(Category::RegPath).string(255),
            Column::build("Name").nullable().localizable().formatted_string(255),
            Column::build("Value").nullable().localizable().formatted_string(0),
            Column::build("Component_").id_string(72),
        ],
    )?;
    // Root 2 — HKEY_LOCAL_MACHINE; имя «-» без значения — при удалении снять ключ целиком.
    p.insert_rows(Insert::into("Registry").rows(vec![
        vec![s("EventSource"), i(2), s(EVENT_KEY), s("EventMessageFile"), s(EVENT_MESSAGES), s(main)],
        vec![s("EventSourceKey"), i(2), s(EVENT_KEY), s("-"), NULL, s(main)],
    ]))?;

    p.create_table("Icon", vec![Column::build("Name").primary_key().id_string(72), Column::build("Data").binary()])?;
    p.insert_rows(Insert::into("Icon").row(vec![s(ICON), Value::Binary]))?;

    p.create_table(
        "Upgrade",
        vec![
            Column::build("UpgradeCode").primary_key().category(Category::Guid).string(38),
            Column::build("VersionMin").primary_key().nullable().category(Category::Text).string(20),
            Column::build("VersionMax").primary_key().nullable().category(Category::Text).string(20),
            Column::build("Language").primary_key().nullable().category(Category::Language).string(255),
            Column::build("Attributes").primary_key().int32(),
            Column::build("Remove").nullable().formatted_string(255),
            Column::build("ActionProperty").category(Category::UpperCase).string(72),
        ],
    )?;
    // Старше этой — снять (VersionMax не включительно); новее — только найти, чтобы не ставить поверх (2 — OnlyDetect).
    p.insert_rows(Insert::into("Upgrade").rows(vec![
        vec![s(UPGRADE_CODE), NULL, s(msi_ver), NULL, i(0), NULL, s("OLDERFOUND")],
        vec![s(UPGRADE_CODE), s(msi_ver), NULL, NULL, i(2), NULL, s("NEWERFOUND")],
    ]))?;

    p.create_table(
        "CustomAction",
        vec![
            Column::build("Action").primary_key().id_string(72),
            Column::build("Type").int16(),
            Column::build("Source").nullable().category(Category::CustomSource).string(72),
            Column::build("Target").nullable().formatted_string(255),
        ],
    )?;
    // 19 — ошибка с текстом и отказ; 51 — задать свойство (путь установки для «Приложений» и winget).
    p.insert_rows(Insert::into("CustomAction").rows(vec![
        vec![s("PreventDowngrade"), i(19), NULL, s("A newer version of Postman is already installed.")],
        vec![s("SetInstallLocation"), i(51), s("ARPINSTALLLOCATION"), s("[INSTALLDIR]")],
    ]))?;

    let ui = [
        ("FindRelatedProducts", None, 25),
        ("PreventDowngrade", Some("NEWERFOUND"), 30),
        ("CostInitialize", None, 800),
        ("FileCost", None, 900),
        ("CostFinalize", None, 1000),
        ("ExecuteAction", None, 1300),
    ];
    let execute = [
        ("FindRelatedProducts", None, 25),
        ("PreventDowngrade", Some("NEWERFOUND"), 30),
        ("CostInitialize", None, 800),
        ("FileCost", None, 900),
        ("CostFinalize", None, 1000),
        ("SetInstallLocation", None, 1001),
        ("InstallValidate", None, 1400),
        ("InstallInitialize", None, 1500),
        ("RemoveExistingProducts", None, 1501),
        ("ProcessComponents", None, 1600),
        ("UnpublishFeatures", None, 1800),
        ("RemoveRegistryValues", None, 2600),
        ("RemoveShortcuts", None, 3200),
        ("RemoveFiles", None, 3500),
        ("InstallFiles", None, 4000),
        ("CreateShortcuts", None, 4500),
        ("WriteRegistryValues", None, 5000),
        ("RegisterUser", None, 6000),
        ("RegisterProduct", None, 6100),
        ("PublishFeatures", None, 6300),
        ("PublishProduct", None, 6400),
        ("InstallFinalize", None, 6600),
    ];
    let admin_ui = [("CostInitialize", None, 800), ("FileCost", None, 900), ("CostFinalize", None, 1000), ("ExecuteAction", None, 1300)];
    let admin = [
        ("CostInitialize", None, 800),
        ("FileCost", None, 900),
        ("CostFinalize", None, 1000),
        ("InstallValidate", None, 1400),
        ("InstallInitialize", None, 1500),
        ("InstallAdminPackage", None, 3900),
        ("InstallFiles", None, 4000),
        ("InstallFinalize", None, 6600),
    ];
    for (table, actions) in [
        ("InstallUISequence", &ui[..]),
        ("InstallExecuteSequence", &execute[..]),
        ("AdminUISequence", &admin_ui[..]),
        ("AdminExecuteSequence", &admin[..]),
    ] {
        p.create_table(
            table,
            vec![
                Column::build("Action").primary_key().id_string(72),
                Column::build("Condition").nullable().category(Category::Condition).string(255),
                Column::build("Sequence").nullable().int16(),
            ],
        )?;
        p.insert_rows(Insert::into(table).rows(actions.iter().map(|&(a, c, n)| vec![s(a), c.map_or(NULL, s), i(n)]).collect()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use msi::Select;

    /// Exe с ресурсом версии `0.1.0.0` — как у сборки версии `0.1.0-beta.2`.
    fn fake_exe(major_minor_patch: (u32, u32, u32)) -> Vec<u8> {
        let (a, b, c) = major_minor_patch;
        let mut image = b"MZ fake image ".to_vec();
        image.extend([0xBD, 0x04, 0xEF, 0xFE, 0, 0, 1, 0]);
        image.extend(((a << 16) | b).to_le_bytes());
        image.extend((c << 16).to_le_bytes());
        image.extend([0u8; 32]);
        image
    }

    fn kit(dir: &Path, v: (u32, u32, u32)) {
        for name in ["postman.exe", "postman-worker.exe", "postman-viewer.exe"] {
            fs::write(dir.join(name), fake_exe(v)).unwrap();
        }
        fs::write(dir.join("README.txt"), "Postman\r\n").unwrap();
    }

    fn property(p: &mut Package<fs::File>, name: &str) -> String {
        let rows = p.select_rows(Select::table("Property")).unwrap();
        let row = rows.into_iter().find(|r| r[0].as_str() == Some(name)).unwrap_or_else(|| panic!("no {name}"));
        row[1].as_str().unwrap().to_owned()
    }

    #[test]
    fn builds_installer_with_kit_files() {
        let tmp = tempdir();
        let dir = tmp.path();
        kit(dir, (0, 1, 0));
        let out = dir.join("postman.msi");
        build(dir, &out, "0.1.0-beta.2", true).unwrap();

        let mut p = msi::open(&out).unwrap();
        assert_eq!(p.summary_info().arch(), Some("x64"));
        assert_eq!(property(&mut p, "ProductVersion"), "0.1.302");
        assert_eq!(property(&mut p, "ProductCode"), product_code("0.1.0-beta.2"));
        assert_eq!(property(&mut p, "UpgradeCode"), UPGRADE_CODE);
        assert_eq!(property(&mut p, "ALLUSERS"), "1");
        let names: Vec<String> = p.select_rows(Select::table("File")).unwrap().map(|r| r[2].as_str().unwrap().to_owned()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"POSTMA~1.EXE|postman-worker.exe".to_owned()), "{names:?}");
        assert!(p.has_stream(CABINET));
        assert!(p.has_stream(&format!("Icon.{ICON}")));

        // Встроенный CAB читается и содержит файлы под ключами таблицы File.
        let mut cab_bytes = Vec::new();
        std::io::Read::read_to_end(&mut p.read_stream(CABINET).unwrap(), &mut cab_bytes).unwrap();
        let mut cabinet = cab::Cabinet::new(Cursor::new(cab_bytes)).unwrap();
        let mut readme = String::new();
        std::io::Read::read_to_string(&mut cabinet.read_file("README.txt").unwrap(), &mut readme).unwrap();
        assert_eq!(readme, "Postman\r\n");
        assert!(cabinet.read_file("worker.exe").is_ok());
    }

    #[test]
    fn refuses_kit_of_another_version() {
        let tmp = tempdir();
        let dir = tmp.path();
        kit(dir, (0, 2, 0));
        let err = build(dir, &dir.join("postman.msi"), "0.1.0-beta.2", true).unwrap_err();
        // Проверка обновления в CI собирает тот же комплект под другой версией.
        build(dir, &dir.join("postman.msi"), "0.1.0-beta.2", false).unwrap();
        assert!(err.to_string().contains("expected 0.1.0.0"), "{err}");
        kit(dir, (0, 1, 0));
        fs::remove_file(dir.join("postman-viewer.exe")).unwrap();
        assert!(build(dir, &dir.join("postman.msi"), "0.1.0-beta.2", true).is_err());
    }

    /// Windows Installer принимает пакет: административная установка (без прав администратора и без записи
    /// в систему) раскладывает файлы под длинными именами в `Postman\`.
    #[cfg(windows)]
    #[test]
    fn windows_installer_extracts_the_package() {
        let tmp = tempdir();
        let dir = tmp.path();
        kit(dir, (0, 1, 0));
        let out = dir.join("postman.msi");
        build(dir, &out, "0.1.0-beta.2", true).unwrap();
        let target = dir.join("admin");
        let status =
            std::process::Command::new("msiexec.exe").arg("/a").arg(&out).arg("/qn").arg(format!("TARGETDIR={}", target.display())).status().unwrap();
        assert!(status.success(), "msiexec /a: {status}");
        for name in ["postman.exe", "postman-worker.exe", "postman-viewer.exe", "README.txt"] {
            assert!(target.join("Postman").join(name).is_file(), "{name}");
        }
        assert_eq!(fs::read_to_string(target.join("Postman/README.txt")).unwrap(), "Postman\r\n");
    }

    /// H-01: временные каталоги тестов — `tempfile` с префиксом `postman-`.
    fn tempdir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("postman-pack").tempdir().unwrap()
    }
}
