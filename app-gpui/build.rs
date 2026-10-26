//! Ресурсы `postman.exe` (S4-01): значок, версия из Cargo, манифест. Нужен rc.exe из Windows SDK.
//!
//! Манифест — с языком `LANGUAGE 0, 0`: gpui вшивает свой (`RT_MANIFEST` 1, язык 0x409), и тот же номер
//! с тем же языком линкер не принимает (CVT1100). Нейтральный по языку загрузчик берёт первым; проверка —
//! тест `manifest_is_applied`. Ресурсы — во все цели пакета (и в тесты), чтобы тест видел тот же манифест.

use std::error::Error;
use std::path::Path;
use std::{env, fs};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=res/postman.ico");
    println!("cargo:rerun-if-changed=res/postman.manifest");
    if env::var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR")?).join("res");
    let rc = Path::new(&env::var("OUT_DIR")?).join("postman.rc");
    let script = format!(
        "#pragma code_page(65001)\n\
         LANGUAGE 0, 0\n\
         1 ICON \"{icon}\"\n\
         1 24 \"{manifest}\"\n\
         {version}",
        icon = rc_path(&dir.join("postman.ico")),
        manifest = rc_path(&dir.join("postman.manifest")),
        version = version_info("Postman", "postman.exe")?,
    );
    fs::write(&rc, script)?;
    embed_resource::compile_for_everything(&rc, embed_resource::NONE).manifest_required()?;
    Ok(())
}

/// Путь в строке .rc: обратные косые удваиваются.
fn rc_path(p: &Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

/// Блок VERSIONINFO из версии пакета (`a.b.c` → `a,b,c,0`).
fn version_info(description: &str, file: &str) -> Result<String, Box<dyn Error>> {
    let v = env::var("CARGO_PKG_VERSION")?;
    let n = |k: &str| env::var(k).map_err(|e| format!("{k}: {e}"));
    let comma = format!("{},{},{},0", n("CARGO_PKG_VERSION_MAJOR")?, n("CARGO_PKG_VERSION_MINOR")?, n("CARGO_PKG_VERSION_PATCH")?);
    let license = env::var("CARGO_PKG_LICENSE").unwrap_or_default();
    Ok(format!(
        "1 VERSIONINFO\n\
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
               VALUE \"FileDescription\", \"{description}\"\n\
               VALUE \"FileVersion\", \"{v}\"\n\
               VALUE \"InternalName\", \"{file}\"\n\
               VALUE \"LegalCopyright\", \"License: {license}\"\n\
               VALUE \"OriginalFilename\", \"{file}\"\n\
               VALUE \"ProductName\", \"Postman\"\n\
               VALUE \"ProductVersion\", \"{v}\"\n\
             END\n\
           END\n\
           BLOCK \"VarFileInfo\"\n\
           BEGIN\n\
             VALUE \"Translation\", 0x409, 1200\n\
           END\n\
         END\n"
    ))
}
