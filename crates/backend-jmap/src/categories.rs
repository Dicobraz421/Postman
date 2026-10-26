//! Таблица категорий по JMAP (S5-02б, решение владельца №4): файл FileNode `.postman/categories.json` в аккаунте.
//! Договор и формат файла — `postman_core::categories`.
//!
//! - **Чтение** — при входе (`sync`, для своего аккаунта и общих ящиков) и по push, когда у аккаунта изменились
//!   файлы (`FileNode` в `StateChange`): папка по имени в корне, файл по имени в папке, содержимое — blob.
//! - **Запись** — сведение и запись с проверкой: прочитать таблицу и состояние FileNode, свести свою с ней
//!   (`Categories::merge` — у каждой категории побеждает поздняя правка), загрузить blob и заменить им
//!   содержимое файла с `ifInState`. Состояние изменилось (другой клиент успел записать) — всё заново, до
//!   `ATTEMPTS` раз. Папку и файл создаёт первая запись; созданные одновременно двумя клиентами — `alreadyExists`,
//!   тогда тоже заново.
//! - **Нет FileNode** (сервер не объявил или аккаунт без файлов) — `Reply::Unsupported`: таблица живёт только в
//!   локальном файле (`labels::LocalFile`). Сервер недоступен — `Reply::Failed`, правка ждёт (`Book::pending`).
//! - **Снять категорию с писем** — только явным выбором при удалении (`Request::Purge`).

use postman_cache::{Cache, scoped};
use postman_core::categories::{ATTEMPTS, Error as FileError, FILE, FOLDER, Scope, decode, encode};
pub use postman_core::categories::{Reply, Request};
use postman_core::labels::Categories;
use postman_jmap::{Client, FILENODE, Filter, Written};

/// Ошибка таблицы на сервере: JMAP или файла (`postman_core::categories::Error`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Jmap(#[from] postman_jmap::Error),
    #[error("{0}")]
    File(#[from] FileError),
}

const MIME: &str = "application/json";
/// Писем за один проход снятия категории (`Email/query` + `Email/set`).
const PURGE_PAGE: u64 = 500;
/// Проходов снятия не больше (страховка от бесконечного цикла).
const PURGE_ROUNDS: usize = 200;

/// Где таблица на сервере сейчас.
struct Remote {
    folder: Option<String>,
    file: Option<String>,
    /// Состояние FileNode аккаунта на момент чтения.
    state: String,
}

/// id аккаунта на сервере: свой (`own`) или общий ящик.
fn server_id<'a>(own: &'a str, account: &'a Scope) -> &'a str {
    account.as_deref().unwrap_or(own)
}

async fn read(client: &Client, acc: &str) -> Result<(Option<Categories>, Remote), Error> {
    if !client.has(FILENODE) || !client.has_files(acc) {
        return Err(Error::File(FileError::Unsupported));
    }
    // Папка, открытая другим пользователем (`mail:share`): сессия объявляет у его аккаунта FileNode, но файлы
    // закрыты (`forbidden`, `docs/STALWART.md` п. 32) — таблицы у такого аккаунта нет.
    let (folder, state) = client.file_node(acc, None, FOLDER).await.map_err(|e| match e {
        postman_jmap::Error::Method { kind, .. } if kind == "forbidden" => Error::File(FileError::Unsupported),
        e => e.into(),
    })?;
    let Some(folder) = folder.filter(|f| f.blob_id.is_none()) else {
        return Ok((None, Remote { folder: None, file: None, state }));
    };
    let (file, state) = client.file_node(acc, Some(&folder.id), FILE).await?;
    let Some((file, blob)) = file.and_then(|f| f.blob_id.clone().map(|b| (f, b))) else {
        return Ok((None, Remote { folder: Some(folder.id), file: None, state }));
    };
    let data = client.download(acc, &blob, FILE, MIME).await?;
    Ok((Some(decode(&data)?), Remote { folder: Some(folder.id), file: Some(file.id), state }))
}

/// Таблица аккаунта `acc` (id на сервере); `None` — файла ещё нет.
pub async fn load(client: &Client, acc: &str) -> Result<Option<Categories>, Error> {
    Ok(read(client, acc).await?.0)
}

/// Свести `mine` с таблицей на сервере и записать с проверкой состояния. Ответ — таблица, как она теперь на сервере.
pub async fn save(client: &Client, acc: &str, mine: &Categories) -> Result<Categories, Error> {
    for _ in 0..ATTEMPTS {
        let (theirs, remote) = read(client, acc).await?;
        let merged = match &theirs {
            Some(t) => mine.merge(t),
            None => mine.clone(),
        };
        if theirs.as_ref() == Some(&merged) {
            return Ok(merged);
        }
        let (folder, state) = match remote.folder {
            Some(f) => (f, Some(remote.state)),
            None => match client.create_folder(acc, None, FOLDER).await? {
                (f, Some(state)) => (f, Some(state)),
                // Папку только что создал другой клиент — состояние уже не то, что читали.
                (_, None) => continue,
            },
        };
        let blob = client.upload(acc, encode(&merged)?, MIME).await?;
        let written = client.write_file(acc, &folder, remote.file.as_deref(), FILE, &blob, MIME, state.as_deref()).await?;
        match written {
            Written::Done { .. } => return Ok(merged),
            Written::Stale | Written::Exists(_) => continue,
        }
    }
    Err(Error::File(FileError::Busy))
}

/// Таблицы своего аккаунта (`own`) и общих ящиков (`shared` — их id) при входе.
pub async fn load_all(client: &Client, own: &str, shared: &[String], notify: &impl Fn(Reply)) {
    let accounts = std::iter::once(None).chain(shared.iter().cloned().map(Some));
    for account in accounts {
        handle(client, own, None, Request::Load { account }, notify).await;
    }
}

/// Выполнить запрос; ответ — через `notify`. `cache` — для [`Request::Purge`] (письма в кэше — без слова сразу).
pub async fn handle(client: &Client, own: &str, cache: Option<&Cache>, request: Request, notify: &impl Fn(Reply)) {
    let failed = |account: Scope, e: Error| match e {
        Error::File(FileError::Unsupported) => Reply::Unsupported { account },
        e => Reply::Failed { account, error: e.to_string() },
    };
    match request {
        Request::Load { account } => {
            let reply = match load(client, server_id(own, &account)).await {
                Ok(table) => Reply::Loaded { account, table },
                Err(e) => failed(account, e),
            };
            notify(reply);
        }
        Request::Save { account, table } => {
            let reply = match save(client, server_id(own, &account), &table).await {
                Ok(table) => Reply::Saved { account, table },
                Err(e) => failed(account, e),
            };
            notify(reply);
        }
        Request::Purge { account, keyword, ticket } => {
            let mail = match &account {
                Some(id) => client.in_account(id),
                None => client.clone(),
            };
            let reply = match purge(&mail, account.as_deref(), cache, &keyword).await {
                Ok(count) => Reply::Purged { account, keyword, ticket, count },
                Err(e) => Reply::PurgeFailed { ticket, error: e.to_string() },
            };
            notify(reply);
        }
    }
}

/// Снять слово со всех писем почтового аккаунта клиента `mail` (во всех папках): страницами, пока находится.
async fn purge(mail: &Client, scope: Option<&str>, cache: Option<&Cache>, keyword: &str) -> postman_jmap::Result<usize> {
    let kw = vec![postman_core::labels::normalize(keyword)];
    let mut count = 0;
    for _ in 0..PURGE_ROUNDS {
        let page = mail.filtered(&Filter::HasKeyword(kw[0].clone()), PURGE_PAGE).await?;
        if page.ids.is_empty() {
            break;
        }
        let failed = mail.set_keywords(&page.ids, &[], &kw).await?;
        if let Some((_, e)) = failed.into_iter().next() {
            return Err(e);
        }
        if let Some(cache) = cache {
            // Кэш поправит и сверка по push; здесь — чтобы список сразу был без метки. Письма нет в кэше — не ошибка.
            for id in &page.ids {
                let _ = cache.set_keywords(&scoped(scope, id), &[], &kw);
            }
        }
        count += page.ids.len();
    }
    Ok(count)
}
