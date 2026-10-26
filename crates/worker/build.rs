//! Версия `postman-worker.exe` из Cargo (S4-01) — видна в свойствах файла и в журнале Windows. Нужен rc.exe
//! из Windows SDK. Значка и манифеста нет: окна у воркера нет, запускается только клиентом.

use std::error::Error;
use std::path::Path;
use std::{env, fs};

fn main() -> Result<(), Box<dyn Error>> {
    if env::var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }
    let v = env::var("CARGO_PKG_VERSION")?;
    let n = |k: &str| env::var(k).map_err(|e| format!("{k}: {e}"));
    let comma = format!("{},{},{},0", n("CARGO_PKG_VERSION_MAJOR")?, n("CARGO_PKG_VERSION_MINOR")?, n("CARGO_PKG_VERSION_PATCH")?);
    let license = env::var("CARGO_PKG_LICENSE").unwrap_or_default();
    let rc = Path::new(&env::var("OUT_DIR")?).join("postman-worker.rc");
    fs::write(
        &rc,
        format!(
            "LANGUAGE 0, 0\n\
             1 VERSIONINFO\n\
             FILEVERSION {comma}\n\
             PRODUCTVERSION {comma}\n\
             FILEFLAGSMASK 0x3f\n\
             FILEFLAGS 0x0\n\
             FILEOS 0x40004\n\
             FILETYPE 0x1\n\
             FILESUBTYPE 0x0\n\
             BEGIN\n\
               BLOCK \"StringFileInfo\"\n\
               BEGIN\n\
                 BLOCK \"040904B0\"\n\
                 BEGIN\n\
                   VALUE \"FileDescription\", \"Postman sandbox worker\"\n\
                   VALUE \"FileVersion\", \"{v}\"\n\
                   VALUE \"InternalName\", \"postman-worker.exe\"\n\
                   VALUE \"LegalCopyright\", \"License: {license}\"\n\
                   VALUE \"OriginalFilename\", \"postman-worker.exe\"\n\
                   VALUE \"ProductName\", \"Postman\"\n\
                   VALUE \"ProductVersion\", \"{v}\"\n\
                 END\n\
               END\n\
               BLOCK \"VarFileInfo\"\n\
               BEGIN\n\
                 VALUE \"Translation\", 0x409, 1200\n\
               END\n\
             END\n"
        ),
    )?;
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional()?;
    Ok(())
}
