//! Версии и коды поставки: версия MSI из версии Cargo, коды GUID без случайности.
//!
//! Коды выводятся из имени и версии (SHA-256 → GUID): та же версия даёт тот же `ProductCode`,
//! компонент — тот же `ComponentId` во всех версиях. `UPGRADE_CODE` — один на всю жизнь продукта.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Код семейства продукта: по нему новая версия находит и снимает старую. Не менять.
pub const UPGRADE_CODE: &str = "{6F1C2B8E-3D4A-4E59-9B7C-2A5D8E1F0C47}";

/// Версия MSI `a.b.n` из версии Cargo `a.b.c[-pre.N]`.
///
/// Windows Installer сравнивает только три числа (`a < 256`, `b < 256`, `n < 65536`) и не знает
/// предварительных версий, а бета должна быть старше прошлой стабильной и младше своей:
/// `n = c·1000 + r`, где `r` — `alpha.N` → N, `beta.N` → 300 + N, `rc.N` → 600 + N, стабильная → 999.
/// `0.1.0-beta.2` → `0.1.302`, `0.1.0` → `0.1.999`, `0.1.1-beta.1` → `0.1.1301`.
pub fn msi_version(version: &str) -> Result<String> {
    let (core, pre) = version.split_once('-').map_or((version, None), |(c, p)| (c, Some(p)));
    let parts: Vec<u32> = core.split('.').map(str::parse).collect::<Result<_, _>>().with_context(|| format!("version {version}"))?;
    let [major, minor, patch] = parts[..] else { bail!("version {version}: expected a.b.c") };
    let rank = match pre {
        None => 999,
        Some(p) => {
            let (kind, n) = p.split_once('.').with_context(|| format!("version {version}: expected -beta.N"))?;
            let n: u32 = n.parse().with_context(|| format!("version {version}"))?;
            let base = match kind {
                "alpha" => 0,
                "beta" => 300,
                "rc" => 600,
                _ => bail!("version {version}: pre-release must be alpha, beta or rc"),
            };
            if !(1..300).contains(&n) {
                bail!("version {version}: pre-release number must be 1..299");
            }
            base + n
        }
    };
    let build = patch * 1000 + rank;
    if major > 255 || minor > 255 || build > 65535 {
        bail!("version {version}: out of the Windows Installer range (a, b < 256, c < 65)");
    }
    Ok(format!("{major}.{minor}.{build}"))
}

/// SHA-256 заглавными шестнадцатеричными (как `InstallerSha256` winget).
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02X}")).collect()
}

/// UUID из строки: первые 16 байт SHA-256 с битами версии 5.
pub fn uuid(seed: &str) -> uuid::Uuid {
    let hash = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    uuid::Builder::from_sha1_bytes(bytes).into_uuid()
}

/// GUID из строки в виде MSI: `{XXXXXXXX-XXXX-…}`, заглавные.
pub fn guid(seed: &str) -> String {
    format!("{{{}}}", uuid(seed).hyphenated().to_string().to_uppercase())
}

/// `ProductCode` версии: один на версию, у новой — новый (большое обновление).
pub fn product_code(version: &str) -> String {
    guid(&format!("Pets.Postman product {version}"))
}

/// `ComponentId` компонента: постоянный, пока у компонента тот же ключевой файл.
pub fn component_code(component: &str) -> String {
    guid(&format!("Pets.Postman component {component}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msi_version_orders_betas_before_release() {
        assert_eq!(msi_version("0.1.0-beta.2").unwrap(), "0.1.302");
        assert_eq!(msi_version("0.1.0-rc.1").unwrap(), "0.1.601");
        assert_eq!(msi_version("0.1.0").unwrap(), "0.1.999");
        assert_eq!(msi_version("0.1.1-alpha.3").unwrap(), "0.1.1003");
        assert_eq!(msi_version("1.2.3").unwrap(), "1.2.3999");
        let order = ["0.1.0-beta.1", "0.1.0-beta.2", "0.1.0-rc.1", "0.1.0", "0.1.1-beta.1", "0.1.1", "0.2.0-beta.1"];
        let n: Vec<Vec<u32>> = order.iter().map(|v| msi_version(v).unwrap().split('.').map(|p| p.parse().unwrap()).collect()).collect();
        assert!(n.windows(2).all(|w| w[0] < w[1]), "{n:?}");
    }

    #[test]
    fn msi_version_rejects_what_it_cannot_order() {
        for v in ["0.1", "0.1.0-dev", "0.1.0-beta", "0.1.0-beta.300", "0.1.65", "256.0.0", "x.1.0"] {
            assert!(msi_version(v).is_err(), "{v}");
        }
    }

    #[test]
    fn codes_are_stable_guids() {
        let a = product_code("0.1.0-beta.2");
        assert_eq!(a, product_code("0.1.0-beta.2"));
        assert_ne!(a, product_code("0.1.0"));
        assert_eq!(a.len(), 38);
        assert!(a.starts_with('{') && a.ends_with('}'));
        assert_eq!(a, a.to_uppercase());
        assert_ne!(component_code("postman.exe"), component_code("postman-worker.exe"));
        assert!(uuid::Uuid::parse_str(UPGRADE_CODE.trim_matches(['{', '}'])).is_ok());
    }
}
