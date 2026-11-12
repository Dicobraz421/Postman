//! Процесс-воркер; запускается только клиентом `postman_worker::Client`.

/// Предел кучи на macOS (MAC-03): `RLIMIT_AS` там не действует — считает распределитель, предел даёт клиент.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: postman_worker::Limited = postman_worker::Limited;

fn main() {
    postman_worker::serve();
}
