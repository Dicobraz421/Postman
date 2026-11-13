//! Процесс-воркер; запускается только клиентом `postman_worker::Client`.

/// Предел кучи на macOS (MAC-03): `RLIMIT_AS` там не действует — считает распределитель, предел даёт клиент.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: postman_worker::Limited = postman_worker::Limited;

fn main() {
    // Действие удаления (MSI, `install.ps1 -Uninstall`): снять профиль песочницы и выйти; код 1 — отказ ОС.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--remove-appcontainer") {
        let code = match postman_worker::remove_appcontainer() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
        std::process::exit(code);
    }
    postman_worker::serve();
}
