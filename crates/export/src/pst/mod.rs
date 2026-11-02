//! PST (MS-PST): новый файл данных Outlook (S8-33а, S8-33б): Unicode, без шифрования, без правки существующего файла
//! и без транзакций. Слои — [`ndb`] (блоки, деревья узлов и блоков, карты, заголовок), [`ltp`] (куча, BTH, контексты
//! свойств и таблиц), [`messaging`] (хранилище, папки, письма, получатели, вложения, вложенные письма, тома).
//!
//! Структуры и запись страниц `outlook-pst` (Microsoft) взять нельзя: в нём только чтение, а зависимость рабочего
//! кода от него — правка Cargo.toml; он — проверка нашего файла в тестах. Свои структуры — по спецификации и по
//! пустому файлу Outlook (набор узлов, шаблоны таблиц, cRef = 2, пустой DList).
//!
//! Файлы (тома и рабочие файлы писателя) создаёт вызывающий ([`Files`]): крейт сам диск не трогает.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::rc::Rc;

mod ltp;
mod messaging;
mod ndb;

pub use messaging::{Folder, VOLUME_LIMIT, Writer, volume_name, write};

/// Файл тома или рабочий файл: пишется подряд, карты и заголовок — на место.
pub trait Storage: Read + Write + Seek {}
impl<T: Read + Write + Seek> Storage for T {}

/// Где писатель берёт файлы: по имени в каталоге выгрузки (`Имя.pst`, `Имя (2).pst`, рабочие `Имя.pst.nbt.tmp`…).
pub trait Files {
    /// Новый пустой файл.
    fn create(&mut self, name: &str) -> io::Result<Box<dyn Storage>>;
    /// Удалить файл (рабочий — после тома, недописанный том — при отмене).
    fn remove(&mut self, name: &str) -> io::Result<()>;
}

/// Байты файла в памяти: общие у файла и [`Memory`].
type Shared = Rc<RefCell<Vec<u8>>>;

/// Файлы в памяти (проверки, малые файлы): копия делит те же файлы.
#[derive(Clone, Default)]
pub struct Memory(Rc<RefCell<HashMap<String, Shared>>>);

impl Memory {
    /// Байты файла `name` (он остаётся).
    pub fn take(&self, name: &str) -> Option<Vec<u8>> {
        self.0.borrow().get(name).map(|f| f.borrow().clone())
    }

    /// Имена файлов по алфавиту.
    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.0.borrow().keys().cloned().collect();
        n.sort();
        n
    }
}

impl Files for Memory {
    fn create(&mut self, name: &str) -> io::Result<Box<dyn Storage>> {
        let data = Rc::new(RefCell::new(Vec::new()));
        self.0.borrow_mut().insert(name.to_owned(), data.clone());
        Ok(Box::new(MemoryFile { data, pos: 0 }))
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.0.borrow_mut().remove(name);
        Ok(())
    }
}

struct MemoryFile {
    data: Shared,
    pos: u64,
}

impl Read for MemoryFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.data.borrow();
        let at = (self.pos as usize).min(data.len());
        let n = buf.len().min(data.len() - at);
        buf[..n].copy_from_slice(&data[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for MemoryFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut data = self.data.borrow_mut();
        let at = self.pos as usize;
        if data.len() < at + buf.len() {
            data.resize(at + buf.len(), 0);
        }
        data[at..at + buf.len()].copy_from_slice(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for MemoryFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.data.borrow().len() as i128;
        let pos = match to {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(d) => len + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        self.pos = u64::try_from(pos).map_err(|_| io::Error::other("seek до начала файла"))?;
        Ok(self.pos)
    }
}
