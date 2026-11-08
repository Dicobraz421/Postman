//! Перетаскивание файлов из окна в другие приложения (OLE drag source): свои `IDataObject` и `IDropSource`,
//! `DoDragDrop` с `DROPEFFECT_COPY`. Форматы:
//! - `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` — виртуальные файлы: байты из памяти или ленивого источника, без
//!   записи на диск (как у Outlook);
//! - запасной `CF_HDROP` — копии на диске, если они есть у всех файлов. Каталог копий — [`TempCopies`]: имя задаёт
//!   приложение, приложение же пишет файл (метка «из Интернета» и безопасное имя — его правила) и чистит каталог.
//!
//! Что можно тащить — решает приложение: сюда приходит уже отобранное.
//!
//! `DoDragDrop` крутит свой модальный цикл сообщений: звать в потоке окна, но не из обработчика gpui (приложение
//! занято, окно не нарисуется) — из задачи `cx.spawn` вне `update`. Ленивый источник вызывается в этом цикле, в
//! потоке окна, когда получатель просит байты.

use std::cell::Cell;
use std::io;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, DV_E_LINDEX, E_FAIL, E_NOTIMPL,
    HGLOBAL, OLE_E_ADVISENOTSUPPORTED, POINT, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC,
    IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, DROPFILES, FD_FILESIZE, FD_PROGRESSUI, FILEDESCRIPTORW,
    FILEGROUPDESCRIPTORW, SHCreateStdEnumFmtEtc,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};
use windows::core::{BOOL, Error, HRESULT, Ref, Result, implement};

/// Ленивый источник байтов: зовётся в потоке окна, когда получатель просит содержимое (может звать не раз).
pub type Provider = Box<dyn Fn() -> io::Result<Vec<u8>>>;

/// Содержимое перетаскиваемого файла.
enum Contents {
    Bytes(Arc<Vec<u8>>),
    Lazy { size: Option<u64>, provider: Provider },
}

/// Файл, который тащат: имя (приложение уже сделало его безопасным), содержимое, копия на диске для `CF_HDROP`.
pub struct DragFile {
    name: String,
    contents: Contents,
    copy: Option<PathBuf>,
}

impl DragFile {
    /// Байты уже в памяти.
    pub fn bytes(name: impl Into<String>, data: Arc<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            contents: Contents::Bytes(data),
            copy: None,
        }
    }

    /// Байты — по запросу получателя; `size` — размер для индикатора копирования, если известен.
    pub fn lazy(name: impl Into<String>, size: Option<u64>, provider: Provider) -> Self {
        Self {
            name: name.into(),
            contents: Contents::Lazy { size, provider },
            copy: None,
        }
    }

    /// Копия файла на диске (обычно в [`TempCopies::slot`]): запасной `CF_HDROP` для получателей без виртуальных
    /// файлов.
    pub fn with_copy(mut self, path: PathBuf) -> Self {
        self.copy = Some(path);
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn size(&self) -> Option<u64> {
        match &self.contents {
            Contents::Bytes(b) => Some(b.len() as u64),
            Contents::Lazy { size, .. } => *size,
        }
    }

    fn data(&self) -> io::Result<Vec<u8>> {
        match &self.contents {
            Contents::Bytes(b) => Ok(b.to_vec()),
            Contents::Lazy { provider, .. } => provider(),
        }
    }
}

/// Итог перетаскивания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Бросили в приложение, которое приняло файлы.
    Dropped,
    /// Отменили: Esc, кнопку отпустили до начала или над местом, которое не принимает.
    Cancelled,
}

/// Порог начала перетаскивания в точках окна (`SM_CXDRAG`, `SM_CYDRAG` — в пикселях экрана).
pub fn threshold(scale: f32) -> (f32, f32) {
    // SAFETY: GetSystemMetrics только читает системную метрику, указателей нет.
    let (x, y) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
    let scale = if scale > 0. { scale } else { 1. };
    (x.max(1) as f32 / scale, y.max(1) as f32 / scale)
}

/// Перетащить файлы (поток окна, модальный цикл OLE). Пустой список — сразу `Cancelled`.
pub fn drag_out(files: Vec<DragFile>) -> io::Result<Outcome> {
    if files.is_empty() {
        return Ok(Outcome::Cancelled);
    }
    let data: IDataObject = FileData::new(files).into();
    let source: IDropSource = Source { first: Cell::new(true) }.into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: оба объекта — наши COM-объекты, живут до конца вызова; `effect` — наша переменная. OLE в потоке окна
    // инициализирован платформой (`OleInitialize` в `WindowsPlatform::new`).
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    match hr {
        DRAGDROP_S_DROP => Ok(Outcome::Dropped),
        DRAGDROP_S_CANCEL => Ok(Outcome::Cancelled),
        e => Err(io::Error::other(format!("DoDragDrop: {}", e.message()))),
    }
}

/// Каталог копий для `CF_HDROP`: `%TEMP%\<app>\drag`, свой подкаталог на каждое перетаскивание — имена файлов разных
/// источников не сталкиваются. Чистить при старте (остатки прошлого запуска) и при выходе.
#[derive(Debug, Clone)]
pub struct TempCopies {
    root: PathBuf,
}

impl TempCopies {
    /// `%TEMP%\<app>\drag`; `app` — техническое имя приложения (`Postman`).
    pub fn new(app: &str) -> Self {
        Self::at(std::env::temp_dir().join(app).join("drag"))
    }

    /// Свой корень (тесты, portable).
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Новый пустой подкаталог под копии одного перетаскивания.
    pub fn slot(&self) -> io::Result<PathBuf> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = self
            .root
            .join(format!("{nanos:x}-{:x}", NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Удалить все копии; каталога нет — не ошибка.
    pub fn clean(&self) -> io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Номера зарегистрированных форматов «FileGroupDescriptorW» и «FileContents».
fn virtual_formats() -> (u16, u16) {
    static FORMATS: OnceLock<(u16, u16)> = OnceLock::new();
    *FORMATS.get_or_init(|| {
        // SAFETY: строки — статические константы с нулём в конце; функция только регистрирует имя формата.
        let (d, c) = unsafe {
            (
                RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW),
                RegisterClipboardFormatW(CFSTR_FILECONTENTS),
            )
        };
        (u16::try_from(d).unwrap_or(0), u16::try_from(c).unwrap_or(0))
    })
}

fn format(cf: u16, lindex: i32) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// Байты в памяти, которую OLE отдаёт получателю (он и освобождает её — `ReleaseStgMedium`).
fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    // SAFETY: выделяем не меньше `bytes.len()` байт; указатель от GlobalLock действителен до GlobalUnlock, копируем
    // ровно `bytes.len()` байт; области не пересекаются (память только что выделена).
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h);
        if p.is_null() {
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        // Возвращает «ошибку» с кодом 0, когда блок разблокирован полностью, — это норма.
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

/// Байты структуры `repr(C)` без указателей.
fn bytes_of<T: Copy>(v: &T) -> Vec<u8> {
    // SAFETY: `T` — plain-old-data структура Win32 (`FILEGROUPDESCRIPTORW`, `FILEDESCRIPTORW`, `DROPFILES`) без
    // указателей; читаем ровно её размер из живой ссылки.
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()).to_vec() }
}

#[implement(IDataObject)]
struct FileData {
    files: Vec<DragFile>,
    descriptor: u16,
    contents: u16,
}

impl FileData {
    fn new(files: Vec<DragFile>) -> Self {
        let (descriptor, contents) = virtual_formats();
        Self {
            files,
            descriptor,
            contents,
        }
    }

    /// Копии на диске есть у всех файлов — можно отдать `CF_HDROP`.
    fn has_copies(&self) -> bool {
        self.files.iter().all(|f| f.copy.is_some())
    }

    /// Что отдаём: описание виртуальных файлов, их байты, запасной список файлов на диске.
    fn formats(&self) -> Vec<FORMATETC> {
        let mut v = vec![format(self.descriptor, -1)];
        v.extend((0..self.files.len()).map(|i| format(self.contents, i as i32)));
        if self.has_copies() {
            v.push(format(CF_HDROP.0, -1));
        }
        v
    }

    /// Номер файла для `FILECONTENTS`: `lindex` в пределах списка; −1 у одного файла — он же.
    fn index(&self, lindex: i32) -> Option<usize> {
        match lindex {
            -1 if self.files.len() == 1 => Some(0),
            i => usize::try_from(i).ok().filter(|&i| i < self.files.len()),
        }
    }

    fn supports(&self, f: &FORMATETC) -> bool {
        if f.tymed & TYMED_HGLOBAL.0 as u32 == 0 || f.dwAspect != DVASPECT_CONTENT.0 || f.cfFormat == 0 {
            return false;
        }
        match f.cfFormat {
            d if d == self.descriptor => true,
            // QueryGetData спрашивает `FILECONTENTS` и с −1 («есть ли вообще»).
            c if c == self.contents => f.lindex == -1 || self.index(f.lindex).is_some(),
            h if h == CF_HDROP.0 => self.has_copies(),
            _ => false,
        }
    }

    /// `FILEGROUPDESCRIPTORW` на все файлы: число и описания подряд (у структуры — место под одно).
    fn descriptor_bytes(&self) -> Vec<u8> {
        let one = FILEGROUPDESCRIPTORW {
            cItems: self.files.len() as u32,
            ..Default::default()
        };
        let head = size_of::<FILEGROUPDESCRIPTORW>() - size_of::<FILEDESCRIPTORW>();
        let mut out = bytes_of(&one)[..head].to_vec();
        for file in &self.files {
            let mut fd = FILEDESCRIPTORW {
                dwFlags: FD_PROGRESSUI.0 as u32,
                ..Default::default()
            };
            if let Some(size) = file.size() {
                fd.dwFlags |= FD_FILESIZE.0 as u32;
                fd.nFileSizeHigh = (size >> 32) as u32;
                fd.nFileSizeLow = size as u32;
            }
            // Структура может быть упакована: имя — своим массивом и присваиванием, без ссылок на поле.
            let mut file_name = [0u16; 260];
            for (dst, src) in file_name.iter_mut().zip(file.name.encode_utf16().take(259)) {
                *dst = src;
            }
            fd.cFileName = file_name;
            out.extend(bytes_of(&fd));
        }
        out
    }

    /// `DROPFILES` и пути копий: UTF-16, каждый с нулём, двойной ноль в конце.
    fn hdrop_bytes(&self) -> Vec<u8> {
        let head = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            pt: POINT::default(),
            fNC: false.into(),
            fWide: true.into(),
        };
        let mut out = bytes_of(&head);
        for path in self.files.iter().filter_map(|f| f.copy.as_deref()) {
            for u in path.as_os_str().to_string_lossy().encode_utf16().chain([0]) {
                out.extend_from_slice(&u.to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }
}

impl IDataObject_Impl for FileData_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        // SAFETY: OLE передаёт указатель на FORMATETC вызывающего на время вызова; null — ошибка формата.
        let f = unsafe { pformatetcin.as_ref() }.ok_or_else(|| Error::from(DV_E_FORMATETC))?;
        if !self.supports(f) {
            return Err(DV_E_FORMATETC.into());
        }
        let bytes = match f.cfFormat {
            d if d == self.descriptor => self.descriptor_bytes(),
            c if c == self.contents => {
                let i = self.index(f.lindex).ok_or_else(|| Error::from(DV_E_LINDEX))?;
                self.files[i].data().map_err(|e| Error::new(E_FAIL, e.to_string()))?
            }
            _ => self.hdrop_bytes(),
        };
        let h = global(&bytes)?;
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: h },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        // SAFETY: как в GetData — указатель вызывающего на время вызова.
        match unsafe { pformatetc.as_ref() } {
            Some(f) if self.supports(f) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        // SAFETY: срез форматов живёт до конца вызова; оболочка копирует его в свой перечислитель.
        unsafe { SHCreateStdEnumFmtEtc(&self.formats()) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<'_, IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

/// Источник: левая кнопка отпущена — бросить, Esc — отменить. Кнопку отпустили ещё до начала (между порогом и
/// `DoDragDrop`) — отмена, а не бросок в окно под курсором.
#[implement(IDropSource)]
struct Source {
    first: Cell<bool>,
}

impl Source {
    fn decide(&self, escape: bool, held: bool) -> HRESULT {
        let first = self.first.replace(false);
        match () {
            _ if escape => DRAGDROP_S_CANCEL,
            _ if !held && first => DRAGDROP_S_CANCEL,
            _ if !held => DRAGDROP_S_DROP,
            _ => S_OK,
        }
    }
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        self.decide(fescapepressed.as_bool(), grfkeystate.0 & MK_LBUTTON.0 != 0)
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Memory::GlobalSize;
    use windows::Win32::System::Ole::ReleaseStgMedium;

    fn pdf() -> DragFile {
        DragFile::bytes("отчёт.pdf", Arc::new(b"%PDF-1.7 body".to_vec()))
    }

    /// Байты HGLOBAL из GetData; носитель освобождается.
    fn get(obj: &IDataObject, f: FORMATETC) -> Vec<u8> {
        // SAFETY: тест: носитель от нашего GetData, читаем GlobalSize байт под замком и освобождаем его один раз.
        unsafe {
            let mut m = obj.GetData(&f).unwrap();
            let h = m.u.hGlobal;
            let n = GlobalSize(h);
            let p = GlobalLock(h);
            let out = std::slice::from_raw_parts(p.cast::<u8>(), n).to_vec();
            let _ = GlobalUnlock(h);
            ReleaseStgMedium(&mut m);
            out
        }
    }

    fn ok(obj: &IDataObject, f: FORMATETC) -> bool {
        // SAFETY: тест: указатель на локальный FORMATETC живёт до конца вызова.
        (unsafe { obj.QueryGetData(&f) }) == S_OK
    }

    /// Описания файлов из `FILEGROUPDESCRIPTORW`: (имя, размер, есть ли размер).
    fn descriptors(bytes: &[u8]) -> Vec<(String, u64, bool)> {
        let head = size_of::<FILEGROUPDESCRIPTORW>() - size_of::<FILEDESCRIPTORW>();
        let n = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), head + n * size_of::<FILEDESCRIPTORW>());
        (0..n)
            .map(|i| {
                // SAFETY: тест: в буфере ровно `n` описаний после заголовка; читаем без выравнивания.
                let fd: FILEDESCRIPTORW = unsafe {
                    std::ptr::read_unaligned(bytes[head + i * size_of::<FILEDESCRIPTORW>()..].as_ptr().cast())
                };
                // Упакованные поля — копиями (`{ … }`), не ссылками.
                let (flags, hi, lo, file_name) = ({ fd.dwFlags }, { fd.nFileSizeHigh }, { fd.nFileSizeLow }, {
                    fd.cFileName
                });
                let name = String::from_utf16_lossy(&file_name[..file_name.iter().position(|&c| c == 0).unwrap()]);
                (
                    name,
                    (u64::from(hi) << 32) | u64::from(lo),
                    flags & FD_FILESIZE.0 as u32 != 0,
                )
            })
            .collect()
    }

    fn hdrop_paths(bytes: &[u8]) -> Vec<String> {
        let wide: Vec<u16> = bytes[size_of::<DROPFILES>()..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert!(wide.ends_with(&[0, 0]), "двойной ноль в конце");
        wide[..wide.len() - 2]
            .split(|&c| c == 0)
            .map(String::from_utf16_lossy)
            .collect()
    }

    #[test]
    fn pets_drag_offers_virtual_file_and_hdrop() {
        let (descriptor, contents) = virtual_formats();
        let obj: IDataObject =
            FileData::new(vec![pdf().with_copy(PathBuf::from(r"C:\Temp\App\drag\1\отчёт.pdf"))]).into();
        assert!(ok(&obj, format(descriptor, -1)));
        assert!(ok(&obj, format(contents, 0)));
        assert!(ok(&obj, format(contents, -1)));
        assert!(!ok(&obj, format(contents, 1)), "второго файла нет");
        assert!(ok(&obj, format(CF_HDROP.0, -1)));
        assert!(!ok(&obj, format(1, -1)), "CF_TEXT не отдаём");
        let bare: IDataObject = FileData::new(vec![pdf()]).into();
        assert!(!ok(&bare, format(CF_HDROP.0, -1)), "без копии — без CF_HDROP");

        assert_eq!(
            get(&obj, format(contents, 0)),
            b"%PDF-1.7 body",
            "FILECONTENTS — байты файла"
        );
        assert_eq!(
            get(&obj, format(contents, -1)),
            b"%PDF-1.7 body",
            "−1 у одного файла — он же"
        );
        assert_eq!(
            descriptors(&get(&obj, format(descriptor, -1))),
            [("отчёт.pdf".to_string(), 13, true)]
        );
        assert_eq!(
            hdrop_paths(&get(&obj, format(CF_HDROP.0, -1))),
            [r"C:\Temp\App\drag\1\отчёт.pdf"]
        );
    }

    #[test]
    fn pets_drag_many_files_and_lazy_contents() {
        let (descriptor, contents) = virtual_formats();
        let calls = std::rc::Rc::new(Cell::new(0));
        let counter = calls.clone();
        let lazy = DragFile::lazy(
            "big.bin",
            None,
            Box::new(move || {
                counter.set(counter.get() + 1);
                Ok(vec![7; 5])
            }),
        );
        let failing = DragFile::lazy(
            "gone.txt",
            Some(3),
            Box::new(|| Err(io::Error::other("source is gone"))),
        );
        let obj: IDataObject = FileData::new(vec![pdf(), lazy, failing]).into();
        assert_eq!(calls.get(), 0, "ленивый источник — только по запросу");
        assert!(!ok(&obj, format(CF_HDROP.0, -1)), "копий нет — без CF_HDROP");
        assert!(ok(&obj, format(contents, 2)) && !ok(&obj, format(contents, 3)));
        assert_eq!(
            descriptors(&get(&obj, format(descriptor, -1))),
            [
                ("отчёт.pdf".to_string(), 13, true),
                ("big.bin".to_string(), 0, false),
                ("gone.txt".to_string(), 3, true)
            ]
        );
        assert_eq!(get(&obj, format(contents, 1)), [7; 5]);
        assert_eq!(calls.get(), 1);
        // SAFETY: тест: указатель на локальный FORMATETC живёт до конца вызова.
        assert!(
            unsafe { obj.GetData(&format(contents, 2)) }.is_err(),
            "ошибка источника — ошибка GetData"
        );
        // SAFETY: тест: как выше.
        assert!(
            unsafe { obj.GetData(&format(contents, -1)) }.is_err(),
            "−1 при нескольких файлах — не файл"
        );

        let copies = FileData::new(vec![
            pdf().with_copy(PathBuf::from(r"C:\a\1.pdf")),
            pdf().with_copy(PathBuf::from(r"C:\b\2.pdf")),
        ]);
        assert_eq!(hdrop_paths(&copies.hdrop_bytes()), [r"C:\a\1.pdf", r"C:\b\2.pdf"]);
    }

    #[test]
    fn pets_drag_source_drops_on_release_and_cancels_early_release() {
        let s = Source { first: Cell::new(true) };
        assert_eq!(s.decide(false, false), DRAGDROP_S_CANCEL, "кнопку отпустили до начала");
        let s = Source { first: Cell::new(true) };
        assert_eq!(s.decide(false, true), S_OK);
        assert_eq!(s.decide(false, true), S_OK);
        assert_eq!(s.decide(false, false), DRAGDROP_S_DROP);
        let s = Source { first: Cell::new(true) };
        assert_eq!(s.decide(false, true), S_OK);
        assert_eq!(s.decide(true, true), DRAGDROP_S_CANCEL, "Esc");
        let (x, y) = threshold(2.);
        assert!(x > 0. && y > 0.);
        assert_eq!(drag_out(Vec::new()).unwrap(), Outcome::Cancelled);
    }

    #[test]
    fn pets_drag_temp_copies_are_removed() {
        let base = std::env::temp_dir().join(format!("pets-drag-test-{}", std::process::id()));
        let copies = TempCopies::at(base.join("drag"));
        let (a, b) = (copies.slot().unwrap(), copies.slot().unwrap());
        assert_ne!(a, b, "свой подкаталог на каждое перетаскивание");
        assert!(a.starts_with(copies.root()) && a.is_dir());
        std::fs::write(a.join("x.pdf"), b"x").unwrap();
        copies.clean().unwrap();
        assert!(!copies.root().exists());
        copies.clean().unwrap();
        assert!(TempCopies::new("App").root().ends_with(r"App\drag"));
        let _ = std::fs::remove_dir_all(base);
    }
}
