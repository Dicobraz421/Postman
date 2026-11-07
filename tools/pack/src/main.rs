//! `postman-pack` — сборка поставки Postman из готового комплекта (`scripts/windows/package.ps1`).
//!
//! ```text
//! postman-pack msi <каталог комплекта> <файл .msi>     пакет MSI (установка в Program Files)
//! postman-pack msi <каталог> <файл .msi> --as <версия> тот же комплект под другой версией (проверка обновления)
//! postman-pack winget <файл .msi> <каталог>          манифест winget для этого MSI
//! postman-pack version                              версия сборки
//! ```
//! Версия — версия этой сборки (Cargo), она же у postman.exe: собирать тем же `cargo build`.
//! Вывод по-английски, как остальная консоль.

mod ids;
mod msi;
mod pe;
mod winget;

use std::path::PathBuf;

use anyhow::{Result, bail};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["msi", dir, out] => {
            let out = PathBuf::from(out);
            msi::build(dir.as_ref(), &out, VERSION, true)?;
            println!("msi: {} (Postman {VERSION}, product version {})", out.display(), ids::msi_version(VERSION)?);
        }
        ["msi", dir, out, "--as", version] => {
            let out = PathBuf::from(out);
            msi::build(dir.as_ref(), &out, version, false)?;
            println!("msi: {} (as Postman {version}, product version {})", out.display(), ids::msi_version(version)?);
        }
        ["winget", msi, out] => {
            let dir = winget::write(msi.as_ref(), out.as_ref(), VERSION)?;
            println!("winget: {}", dir.display());
        }
        ["version"] => println!("{VERSION}"),
        _ => bail!("usage: postman-pack msi <kit dir> <out.msi> [--as <version>] | winget <file.msi> <out dir> | version"),
    }
    Ok(())
}
