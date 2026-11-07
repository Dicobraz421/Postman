//! Манифест winget (решение владельца 2026-11-07: один пакетный менеджер — winget) для MSI выпуска.
//!
//! Раскладка — как в `microsoft/winget-pkgs`: `manifests/p/Pets/Postman/<версия>/` с тремя файлами
//! (версия, установщик, описание en-US) и описанием ru-RU. Хэш — готового (подписанного) MSI, поэтому
//! манифест собирается после подписи. Отправка в `winget-pkgs` — PR с разрешения владельца, `docs/DEV.md`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ids::{UPGRADE_CODE, msi_version, product_code, sha256_hex};
use crate::msi::MANUFACTURER;
use anyhow::{Context, Result};

pub const ID: &str = "Pets.Postman";
const SCHEMA: &str = "1.9.0";
const REPO: &str = "https://github.com/Dicobraz421/Postman";

/// Имя MSI выпуска — как его кладёт `package.ps1`.
pub fn msi_name(version: &str) -> String {
    format!("postman-{version}-windows-x64.msi")
}

/// Записать манифест в `out/manifests/p/Pets/Postman/<версия>/`; вернуть этот каталог.
pub fn write(msi: &Path, out: &Path, version: &str) -> Result<PathBuf> {
    let data = fs::read(msi).with_context(|| format!("read {}", msi.display()))?;
    let sha = sha256_hex(&data);
    let dir = out.join("manifests/p/Pets/Postman").join(version);
    fs::create_dir_all(&dir)?;
    for (name, text) in files(version, &sha)? {
        fs::write(dir.join(name), text)?;
    }
    Ok(dir)
}

fn files(version: &str, sha: &str) -> Result<[(String, String); 4]> {
    let display = msi_version(version)?;
    let code = product_code(version);
    let head = |kind: &str| {
        format!(
            "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{kind}.{SCHEMA}.schema.json\n\nPackageIdentifier: {ID}\nPackageVersion: {version}\n"
        )
    };
    let tail = |kind: &str| format!("ManifestType: {kind}\nManifestVersion: {SCHEMA}\n");
    let url = format!("{REPO}/releases/download/v{version}/{}", msi_name(version));

    let version_file = format!("{}DefaultLocale: en-US\n{}", head("version"), tail("version"));
    let installer = format!(
        "{}InstallerType: msi\nScope: machine\nInstallModes:\n- interactive\n- silent\n- silentWithProgress\n\
         UpgradeBehavior: install\nProductCode: '{code}'\nAppsAndFeaturesEntries:\n- DisplayName: Postman\n  \
         Publisher: {MANUFACTURER}\n  DisplayVersion: {display}\n  ProductCode: '{code}'\n  UpgradeCode: '{UPGRADE_CODE}'\n\
         Installers:\n- Architecture: x64\n  InstallerUrl: {url}\n  InstallerSha256: {sha}\n{}",
        head("installer"),
        tail("installer"),
    );
    let en = format!(
        "{}PackageLocale: en-US\nPublisher: {MANUFACTURER}\nPublisherUrl: https://github.com/Dicobraz421\nPackageName: Postman\n\
         PackageUrl: {REPO}\nLicense: Apache-2.0\nLicenseUrl: {REPO}/blob/main/LICENSE\n\
         ShortDescription: Mail, calendar and contacts for Windows (JMAP, IMAP, Exchange).\n\
         Tags:\n- email\n- calendar\n- contacts\n- jmap\n- imap\n- exchange\n\
         ReleaseNotesUrl: {REPO}/releases/tag/v{version}\n{}",
        head("defaultLocale"),
        tail("defaultLocale"),
    );
    let ru = format!(
        "{}PackageLocale: ru-RU\nPublisher: {MANUFACTURER}\nPackageName: Postman\nLicense: Apache-2.0\n\
         ShortDescription: Почта, календарь и контакты для Windows (JMAP, IMAP, Exchange).\n{}",
        head("locale"),
        tail("locale"),
    );
    Ok([
        (format!("{ID}.yaml"), version_file),
        (format!("{ID}.installer.yaml"), installer),
        (format!("{ID}.locale.en-US.yaml"), en),
        (format!("{ID}.locale.ru-RU.yaml"), ru),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_points_at_the_release_msi() {
        let tmp = tempfile::Builder::new().prefix("postman-pack").tempdir().unwrap();
        let msi = tmp.path().join("p.msi");
        fs::write(&msi, b"abc").unwrap();
        let dir = write(&msi, tmp.path(), "0.1.0-beta.2").unwrap();
        assert!(dir.ends_with("manifests/p/Pets/Postman/0.1.0-beta.2"));
        let installer = fs::read_to_string(dir.join("Pets.Postman.installer.yaml")).unwrap();
        assert!(
            installer.contains(
                "InstallerUrl: https://github.com/Dicobraz421/Postman/releases/download/v0.1.0-beta.2/postman-0.1.0-beta.2-windows-x64.msi"
            )
        );
        // SHA-256 от "abc".
        assert!(installer.contains("InstallerSha256: BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"));
        assert!(installer.contains("DisplayVersion: 0.1.302"));
        assert!(installer.contains(&format!("ProductCode: '{}'", product_code("0.1.0-beta.2"))));
        let version = fs::read_to_string(dir.join("Pets.Postman.yaml")).unwrap();
        assert!(version.contains("PackageVersion: 0.1.0-beta.2\nDefaultLocale: en-US\nManifestType: version\n"));
        for f in ["Pets.Postman.locale.en-US.yaml", "Pets.Postman.locale.ru-RU.yaml"] {
            let text = fs::read_to_string(dir.join(f)).unwrap();
            assert!(text.contains("PackageIdentifier: Pets.Postman\n") && text.contains("ManifestVersion: 1.9.0\n"), "{f}");
        }
    }
}
