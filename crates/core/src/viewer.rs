//! Просмотр вложений модулями WASM (S8-24, ADR 0007) — в отдельном процессе `postman-viewer`, как разбор тел в
//! [`crate::render`]: один процесс на сеанс в своём потоке, запуск — по первому запросу; процесс, остановленный
//! сторожем или упавший, клиент поднимает заново на следующем запросе.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use postman_worker::Client;
use postman_worker::wasm::{Refusal, ViewerOutput};

/// Итог просмотра `key`: ответ модуля (проверен хостом), отказ хоста или `Err` — хост не ответил.
pub struct Viewed {
    pub key: String,
    pub result: Result<Result<ViewerOutput, Refusal>, String>,
}

struct Job {
    key: String,
    module: Arc<Vec<u8>>,
    input: Arc<Vec<u8>>,
}

pub struct Viewer {
    tx: mpsc::Sender<Job>,
    pub rx: mpsc::Receiver<Viewed>,
}

impl Viewer {
    /// Хост рядом с основным исполняемым файлом.
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Self {
        Self::with_exe(Client::default_viewer_exe(), repaint)
    }

    pub fn with_exe(exe: PathBuf, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel();
        let started = std::thread::Builder::new().name("viewer".into()).spawn(move || {
            let mut client = Client::viewer(exe);
            while let Ok(job) = jobs.recv() {
                let result = client.view_module(&job.module, &job.input);
                if let Err(e) = &result {
                    tracing::warn!("viewer host failed: {e}");
                }
                if done.send(Viewed { key: job.key, result }).is_err() {
                    return;
                }
                repaint();
            }
        });
        // Поток не запустился — модули не показывают (запросы без ответа), встроенный просмотр работает.
        if let Err(e) = started {
            tracing::warn!("viewer thread not started: {e}");
        }
        Self { tx, rx }
    }

    /// Показать `input` модулем `module`; ответ — в [`Viewer::rx`] с тем же `key`.
    pub fn view(&self, key: String, module: Arc<Vec<u8>>, input: Arc<Vec<u8>>) {
        let _ = self.tx.send(Job { key, module, input });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    /// Нет `postman-viewer` — ошибка в ответе, без падения; следующий запрос — снова попытка запуска.
    #[test]
    fn missing_host_is_an_error() {
        let v = super::Viewer::with_exe("no-such-postman-viewer".into(), || {});
        for key in ["a", "b"] {
            v.view(key.into(), Arc::new(b"\0asm".to_vec()), Arc::new(Vec::new()));
            let got = v.rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(got.key, key);
            assert!(got.result.is_err());
        }
    }
}
