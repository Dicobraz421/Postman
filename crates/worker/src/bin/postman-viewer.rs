//! Хост модулей просмотра WASM (ADR 0007); запускается только клиентом `postman_worker::Client::viewer`.

fn main() {
    postman_worker::wasm::serve();
}
