//! Linux (PAR-06): значок в трее — StatusNotifierItem (freedesktop/KDE, его показывают KDE, GNOME с расширением
//! AppIndicator, XFCE, Cinnamon) и меню — `com.canonical.dbusmenu`, по D-Bus сеанса (`zbus`). API — как у
//! `gpui_windows::pets::tray`: значок, подсказка, меню, щелчок. Хоста значков (`org.kde.StatusNotifierWatcher`) нет —
//! [`Tray::new`] возвращает ошибку, значка нет (как раньше), окно закрывается.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// Значки трея — те же файлы, что в ресурсах exe Windows (`build.rs`): обычный и «есть непрочитанные».
const ICONS: [&[u8]; 2] = [include_bytes!("../../res/postman.ico"), include_bytes!("../../res/postman-unread.ico")];
/// Размеры для хоста (он выбирает ближайший).
const SIZES: [u32; 3] = [24, 32, 64];
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

/// Что сделал пользователь.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Щелчок по значку.
    Click,
    /// Пункт меню с этим номером.
    Menu(u32),
}

/// Пункт меню.
pub enum Item {
    Command(u32, String),
    Separator,
}

/// Картинка значка для SNI: (ширина, высота, ARGB32 в сетевом порядке).
type Pixmap = (i32, i32, Vec<u8>);

type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

/// Пункт меню D-Bus: номер и команда с подписью; `None` — разделитель.
type MenuItems = Vec<(i32, Option<(u32, String)>)>;

/// Узел раскладки `dbusmenu`: номер, свойства, дети.
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// Значок в трее; живёт, пока жив объект (соединение D-Bus с объектами значка и меню).
pub struct Tray {
    conn: zbus::Connection,
}

struct Sni {
    tip: String,
    icon: Vec<Pixmap>,
    on_event: OnEvent,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Sni {
    #[zbus(property)]
    fn category(&self) -> &str {
        "Communications"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "postman"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "Postman"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.icon.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        (String::new(), Vec::new(), self.tip.clone(), String::new())
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).unwrap_or_default()
    }

    fn activate(&self, _x: i32, _y: i32) {
        (self.on_event)(Event::Click);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        (self.on_event)(Event::Click);
    }

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Меню значка (`com.canonical.dbusmenu`): плоский список под корнем 0; номер пункта D-Bus = номер команды + 1.
struct Menu {
    items: Arc<Mutex<MenuItems>>,
    revision: u32,
    on_event: OnEvent,
}

impl Menu {
    fn layout(&self) -> Layout {
        let items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        let children = items
            .iter()
            .filter_map(|(id, item)| {
                let mut props: HashMap<&str, Value<'_>> = HashMap::new();
                match item {
                    Some((_, label)) => {
                        props.insert("label", Value::from(label.as_str()));
                    }
                    None => {
                        props.insert("type", Value::from("separator"));
                    }
                }
                OwnedValue::try_from(Value::from(zbus::zvariant::Structure::from((*id, props, Vec::<Value<'_>>::new())))).ok()
            })
            .collect();
        let mut root = HashMap::new();
        if let Ok(v) = OwnedValue::try_from(Value::from("submenu")) {
            root.insert("children-display".to_owned(), v);
        }
        (0, root, children)
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        (self.revision, self.layout())
    }

    fn get_group_properties(&self, _ids: Vec<i32>, _property_names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        Vec::new()
    }

    fn get_property(&self, _id: i32, _name: &str) -> zbus::fdo::Result<OwnedValue> {
        Err(zbus::fdo::Error::InvalidArgs("no such property".into()))
    }

    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id != "clicked" {
            return;
        }
        let items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, Some((cmd, _)))) = items.iter().find(|(i, _)| *i == id) {
            (self.on_event)(Event::Menu(*cmd));
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event_id, data, ts) in events {
            self.event(id, &event_id, Value::from(data), ts);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
}

impl Tray {
    /// Поставить значок `icon` (0 — обычный, 1 — «есть непрочитанные») с подсказкой `tip`; события — в `on_event`
    /// (из потока D-Bus). Нет хоста значков — ошибка.
    pub fn new(icon: u16, tip: &str, on_event: impl Fn(Event) + Send + Sync + 'static) -> Result<Tray, String> {
        let on_event: OnEvent = Arc::new(on_event);
        let sni = Sni { tip: tip.to_owned(), icon: pixmaps(icon), on_event: on_event.clone() };
        let menu = Menu { items: Arc::new(Mutex::new(Vec::new())), revision: 1, on_event };
        zbus::block_on(async move {
            let conn = zbus::Connection::session().await?;
            conn.object_server().at(ITEM_PATH, sni).await?;
            conn.object_server().at(MENU_PATH, menu).await?;
            let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
            conn.request_name(name.as_str()).await?;
            conn.call_method(
                Some("org.kde.StatusNotifierWatcher"),
                "/StatusNotifierWatcher",
                Some("org.kde.StatusNotifierWatcher"),
                "RegisterStatusNotifierItem",
                &(name.as_str(),),
            )
            .await?;
            Ok::<_, zbus::Error>(Tray { conn })
        })
        .map_err(|e| format!("no tray host (StatusNotifierWatcher): {e}"))
    }

    /// Пункты меню (заменяют прежние).
    pub fn set_menu(&self, items: Vec<Item>) {
        let flat: MenuItems = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let id = i32::try_from(i + 1).unwrap_or(i32::MAX);
                match item {
                    Item::Command(cmd, label) => (id, Some((cmd, label))),
                    Item::Separator => (id, None),
                }
            })
            .collect();
        let result = zbus::block_on(async {
            let iface = self.conn.object_server().interface::<_, Menu>(MENU_PATH).await?;
            let revision = {
                let mut menu = iface.get_mut().await;
                *menu.items.lock().unwrap_or_else(PoisonError::into_inner) = flat;
                menu.revision += 1;
                menu.revision
            };
            Menu::layout_updated(iface.signal_emitter(), revision, 0).await
        });
        if let Err(e) = result {
            tracing::warn!("tray: menu not updated: {e}");
        }
    }

    /// Подсказка значка.
    pub fn set_tip(&self, tip: &str) -> Result<(), String> {
        zbus::block_on(async {
            let iface = self.conn.object_server().interface::<_, Sni>(ITEM_PATH).await?;
            iface.get_mut().await.tip = tip.to_owned();
            Sni::new_tool_tip(iface.signal_emitter()).await
        })
        .map_err(|e| e.to_string())
    }

    /// Значок (0 — обычный, 1 — «есть непрочитанные»).
    pub fn set_icon(&self, icon: u16) -> Result<(), String> {
        let pixmaps = pixmaps(icon);
        zbus::block_on(async {
            let iface = self.conn.object_server().interface::<_, Sni>(ITEM_PATH).await?;
            iface.get_mut().await.icon = pixmaps;
            Sni::new_icon(iface.signal_emitter()).await
        })
        .map_err(|e| e.to_string())
    }
}

/// Картинки значка нужных размеров из `.ico` (все кадры — PNG); кадр не разобран — пропущен.
fn pixmaps(icon: u16) -> Vec<Pixmap> {
    let ico = ICONS[usize::from(icon.min(1))];
    SIZES.iter().filter_map(|&size| ico_png(ico, size).and_then(argb)).collect()
}

/// PNG кадра `size` из `.ico` (каталог ICONDIR, кадры — PNG); нет — `None`.
fn ico_png(ico: &[u8], size: u32) -> Option<&[u8]> {
    let count = usize::from(u16::from_le_bytes([*ico.get(4)?, *ico.get(5)?]));
    (0..count).find_map(|i| {
        let e = ico.get(6 + 16 * i..22 + 16 * i)?;
        let w = if e[0] == 0 { 256 } else { u32::from(e[0]) };
        let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        (w == size).then(|| ico.get(off..off.checked_add(len)?)).flatten()
    })
}

/// PNG → (ширина, высота, ARGB32 в сетевом порядке байтов) для `IconPixmap`.
fn argb(png_bytes: &[u8]) -> Option<Pixmap> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(png_bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    let (rgba, _) = buf.get(..info.buffer_size())?.as_chunks::<4>();
    let data = rgba.iter().flat_map(|&[r, g, b, a]| [a, r, g, b]).collect();
    Some((i32::try_from(info.width).ok()?, i32::try_from(info.height).ok()?, data))
}

#[cfg(test)]
mod tests {
    /// Значки трея: каждый размер есть в обоих `.ico` и разобран в ARGB нужной длины.
    #[test]
    fn icons_decode() {
        for icon in [0, 1] {
            let px = super::pixmaps(icon);
            assert_eq!(px.len(), super::SIZES.len(), "{icon}");
            for (w, h, data) in px {
                assert_eq!(data.len(), (w * h * 4) as usize);
            }
        }
    }
}
