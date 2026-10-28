//! Перетаскивание вложения в другое приложение (T1-18): OLE drag source — свои `IDataObject` и `IDropSource`,
//! `DoDragDrop` с `DROPEFFECT_COPY`. Форматы:
//! - `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` — виртуальный файл: байты из памяти, без записи на диск (как у
//!   Outlook);
//! - запасной `CF_HDROP` — копия во временном каталоге Postman (`%TEMP%\Postman\drag`) с Mark-of-the-Web; каталог
//!   удаляется при старте и при выходе.
//!
//! `DoDragDrop` крутит свой модальный цикл сообщений: звать в потоке окна, но не из обработчика gpui (приложение
//! занято, окно не нарисуется) — из задачи `cx.spawn` вне `update`.

use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL, HGLOBAL, OLE_E_ADVISENOTSUPPORTED, POINT, S_OK,
};
use windows::Win32::System::Com::{
    DATADIR_GET, DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0,
    TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, DROPFILES, FD_FILESIZE, FD_PROGRESSUI, FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, SHCreateStdEnumFmtEtc,
};
use windows::core::{BOOL, Error, HRESULT, Ref, Result, implement};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};

/// Файл, который тащат: имя (уже безопасное) и байты.
pub struct DragFile {
    pub name: String,
    pub data: Arc<Vec<u8>>,
}

/// Порог начала перетаскивания в точках окна (`SM_CXDRAG`, `SM_CYDRAG` — в пикселях экрана).
pub fn threshold(scale: f32) -> (f32, f32) {
    // SAFETY: GetSystemMetrics только читает системную метрику, указателей нет.
    let (x, y) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
    let scale = if scale > 0. { scale } else { 1. };
    (x.max(1) as f32 / scale, y.max(1) as f32 / scale)
}

/// Каталог временных копий для `CF_HDROP`.
pub fn temp_root() -> PathBuf {
    std::env::temp_dir().join("Postman").join("drag")
}

/// Копия файла для `CF_HDROP` (в фоне, до начала перетаскивания): свой подкаталог на каждое перетаскивание — имена
/// вложений разных писем не сталкиваются; метка «из Интернета», как у «Сохранить как».
pub fn prepare(root: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    let dir = root.join(format!("{:x}", jiff::Timestamp::now().as_nanosecond()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(postman_core::files::safe_name(name));
    postman_core::files::write(&path, data)?;
    Ok(path)
}

/// Удалить временные копии (старт и выход).
pub fn clean(root: &Path) {
    match std::fs::remove_dir_all(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("drag copies {}: {e}", root.display()),
    }
}

/// Перетащить файл (поток окна, модальный цикл OLE). `copy` — копия на диске для `CF_HDROP`; нет — только
/// виртуальный файл. `Ok(true)` — бросили, `Ok(false)` — отменили.
pub fn drag(file: &DragFile, copy: Option<PathBuf>) -> std::result::Result<bool, String> {
    let data: IDataObject = FileData::new(file, copy).into();
    let source: IDropSource = Source { first: Cell::new(true) }.into();
    let mut effect = DROPEFFECT_NONE;
    // SAFETY: оба объекта — наши COM-объекты, живут до конца вызова; `effect` — наша переменная. OLE в потоке окна
    // инициализирован платформой gpui (`OleInitialize`).
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    match hr {
        DRAGDROP_S_DROP => Ok(true),
        DRAGDROP_S_CANCEL => Ok(false),
        e => Err(e.message()),
    }
}

/// Номера зарегистрированных форматов «FileGroupDescriptorW» и «FileContents».
fn virtual_formats() -> (u16, u16) {
    static FORMATS: OnceLock<(u16, u16)> = OnceLock::new();
    *FORMATS.get_or_init(|| {
        // SAFETY: строки — статические константы с нулём в конце; функция только регистрирует имя формата.
        let (d, c) = unsafe { (RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW), RegisterClipboardFormatW(CFSTR_FILECONTENTS)) };
        (u16::try_from(d).unwrap_or(0), u16::try_from(c).unwrap_or(0))
    })
}

fn format(cf: u16, lindex: i32) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex, tymed: TYMED_HGLOBAL.0 as u32 }
}

/// Байты в памяти, которую OLE отдаёт получателю (он и освобождает её — `ReleaseStgMedium`).
fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    // SAFETY: выделяем не меньше `bytes.len()` байт; указатель от GlobalLock действителен до GlobalUnlock, копируем
    // ровно `bytes.len()` байт; области не пересекаются (память только что выделена).
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h);
        if p.is_null() {
            return Err(Error::from_win32());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        // Возвращает «ошибку» с кодом 0, когда блок разблокирован полностью, — это норма.
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

/// Байты структуры `repr(C)` без указателей.
fn bytes_of<T: Copy>(v: &T) -> Vec<u8> {
    // SAFETY: `T` — plain-old-data структура Win32 (`FILEGROUPDESCRIPTORW`, `DROPFILES`) без указателей; читаем ровно
    // её размер из живой ссылки.
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), size_of::<T>()).to_vec() }
}

#[implement(IDataObject)]
struct FileData {
    name: String,
    data: Arc<Vec<u8>>,
    copy: Option<PathBuf>,
    descriptor: u16,
    contents: u16,
}

impl FileData {
    fn new(file: &DragFile, copy: Option<PathBuf>) -> Self {
        let (descriptor, contents) = virtual_formats();
        Self { name: file.name.clone(), data: file.data.clone(), copy, descriptor, contents }
    }

    /// Что отдаём: описание виртуального файла, его байты, запасной список файлов на диске.
    fn formats(&self) -> Vec<FORMATETC> {
        let mut v = vec![format(self.descriptor, -1), format(self.contents, 0)];
        if self.copy.is_some() {
            v.push(format(CF_HDROP.0, -1));
        }
        v
    }

    fn supports(&self, f: &FORMATETC) -> bool {
        if f.tymed & TYMED_HGLOBAL.0 as u32 == 0 || f.dwAspect != DVASPECT_CONTENT.0 || f.cfFormat == 0 {
            return false;
        }
        match f.cfFormat {
            d if d == self.descriptor => true,
            c if c == self.contents => f.lindex <= 0,
            h if h == CF_HDROP.0 => self.copy.is_some(),
            _ => false,
        }
    }

    fn descriptor_bytes(&self) -> Vec<u8> {
        let mut fd = FILEDESCRIPTORW { dwFlags: (FD_FILESIZE.0 | FD_PROGRESSUI.0) as u32, ..Default::default() };
        let size = self.data.len() as u64;
        fd.nFileSizeHigh = (size >> 32) as u32;
        fd.nFileSizeLow = size as u32;
        // Структура упакована (pack 1): имя — своим массивом и присваиванием, без ссылок на поле.
        let mut file_name = [0u16; 260];
        for (dst, src) in file_name.iter_mut().zip(self.name.encode_utf16().take(259)) {
            *dst = src;
        }
        fd.cFileName = file_name;
        bytes_of(&FILEGROUPDESCRIPTORW { cItems: 1, fgd: [fd] })
    }

    /// `DROPFILES` и путь копии: UTF-16, двойной ноль в конце.
    fn hdrop_bytes(&self) -> Vec<u8> {
        let head = DROPFILES { pFiles: size_of::<DROPFILES>() as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
        let mut out = bytes_of(&head);
        let path = self.copy.as_deref().map(|p| p.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        for u in path.encode_utf16().chain([0, 0]) {
            out.extend_from_slice(&u.to_le_bytes());
        }
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
            c if c == self.contents => self.data.to_vec(),
            _ => self.hdrop_bytes(),
        };
        let h = global(&bytes)?;
        Ok(STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: ManuallyDrop::new(None) })
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

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        let first = self.first.replace(false);
        let held = grfkeystate.0 & MK_LBUTTON.0 != 0;
        match () {
            _ if fescapepressed.as_bool() => DRAGDROP_S_CANCEL,
            _ if !held && first => DRAGDROP_S_CANCEL,
            _ if !held => DRAGDROP_S_DROP,
            _ => S_OK,
        }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use windows::Win32::System::Memory::GlobalSize;
    use windows::Win32::System::Ole::ReleaseStgMedium;

    fn object(copy: Option<PathBuf>) -> IDataObject {
        FileData::new(&DragFile { name: "отчёт.pdf".into(), data: Arc::new(b"%PDF-1.7 body".to_vec()) }, copy).into()
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

    #[test]
    fn data_object_offers_virtual_file_and_hdrop() {
        let (descriptor, contents) = virtual_formats();
        let obj = object(Some(PathBuf::from(r"C:\Temp\Postman\drag\1\отчёт.pdf")));
        // SAFETY: тест: указатели на локальные FORMATETC живут до конца вызова.
        let ok = |f: FORMATETC| unsafe { obj.QueryGetData(&f) } == S_OK;
        assert!(ok(format(descriptor, -1)));
        assert!(ok(format(contents, 0)));
        assert!(ok(format(CF_HDROP.0, -1)));
        assert!(!ok(format(1, -1)), "CF_TEXT не отдаём");
        let bare = object(None);
        // SAFETY: тест: как выше.
        assert_ne!(unsafe { bare.QueryGetData(&format(CF_HDROP.0, -1)) }, S_OK, "без копии — без CF_HDROP");

        assert_eq!(get(&obj, format(contents, 0)), b"%PDF-1.7 body", "FILECONTENTS — байты вложения");
        let d = get(&obj, format(descriptor, -1));
        assert!(d.len() >= size_of::<FILEGROUPDESCRIPTORW>());
        // SAFETY: тест: байты — FILEGROUPDESCRIPTORW, скопированная целиком; читаем без выравнивания.
        let g: FILEGROUPDESCRIPTORW = unsafe { std::ptr::read_unaligned(d.as_ptr().cast()) };
        // Упакованные поля — копиями (`{ … }`), не ссылками.
        let (items, fd) = ({ g.cItems }, { g.fgd }[0]);
        let (size, file_name) = ({ fd.nFileSizeLow }, { fd.cFileName });
        assert_eq!(items, 1);
        assert_eq!(size, 13);
        let name = String::from_utf16_lossy(&file_name[..file_name.iter().position(|&c| c == 0).unwrap()]);
        assert_eq!(name, "отчёт.pdf");
        let h = get(&obj, format(CF_HDROP.0, -1));
        let path: Vec<u16> = h[size_of::<DROPFILES>()..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert!(String::from_utf16_lossy(&path).starts_with(r"C:\Temp\Postman\drag\1\отчёт.pdf"));
        assert!(path.ends_with(&[0, 0]));
    }

    #[test]
    fn temp_copy_is_removed() {
        let root = tempfile::Builder::new().prefix("postman-").tempdir().unwrap();
        let drag = root.path().join("drag");
        let p = prepare(&drag, "a/b:c.pdf", b"x").unwrap();
        assert!(p.starts_with(&drag) && p.is_file());
        assert_eq!(std::fs::read(&p).unwrap(), b"x");
        clean(&drag);
        assert!(!drag.exists());
        clean(&drag);
    }
}
