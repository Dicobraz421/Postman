//! Внешние картинки письма (T3-04, решение `docs/decisions/2026-11-11-remote-images.md`).
//!
//! IR хранит адреса внешних картинок ([`postman_ir::Inline::Remote`]); сами они не грузятся. При «Спрашивать»
//! (Настройки → Безопасность) строка «Внешние картинки не загружаются (N)» предлагает «Загрузить картинки» (это
//! письмо, этот раз) и «Всегда для домена …» (домен отправителя — в доверенные); письма доверенных доменов грузят
//! картинки сами. «Не загружаются никогда» (выбор или политика организации, `security.rs`) и письмо, помеченное
//! опасным, — только строка, без кнопок.
//!
//! Сеть — в процессе интерфейса, но байты он не разбирает: качает [`Web`] (тот же HTTPS-клиент, что ссылки занятости
//! S8-42: только `https`, имя сервера, а не IP и не `localhost`, без входа, кук и заголовков авторизации, перенаправления
//! — только на такие же адреса, 5 с), раскодирует воркер-песочница (`Request::Image`, как картинки `cid:`); песочница
//! не ослаблена, у воркера сети нет. Пределы: [`MAX_IMAGE`] на картинку, [`MAX_LETTER`] на письмо, [`MAX_IMAGES`]
//! картинок. Байты открытых писем — в памяти ([`CACHED_LETTERS`] писем): повторное открытие — без сети. Демо и
//! снимки — без сети ([`Stub`]).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use pets_i18n::tr;
use pets_ui::Level as Toast;
use postman_core::prefs::{Images, Security, sender_domain};
use postman_worker::Client as Worker;

use crate::reading::{ReadingEvent, ReadingPane, render_image};
use crate::roles::Look;
use crate::settings_tab::{SettingsView, link, row};

/// Одна картинка — не больше (байты ответа сервера).
pub(crate) const MAX_IMAGE: usize = 5 * 1024 * 1024;
/// Все картинки письма вместе — не больше.
pub(crate) const MAX_LETTER: usize = 20 * 1024 * 1024;
/// Картинок письма — не больше: остальные остаются подписями.
pub(crate) const MAX_IMAGES: usize = 40;
/// Качаются одновременно.
const PARALLEL: usize = 4;
/// Писем, чьи картинки держатся в памяти.
pub(crate) const CACHED_LETTERS: usize = 16;
/// Наибольшая сторона картинки (как у `cid:`).
const MAX_SIDE: u32 = 1600;

/// Картинки открытого письма.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Remote {
    /// Не загружались.
    #[default]
    Blocked,
    Loading,
    /// Загружены; `failed` — сколько не пришло или не раскодировалось.
    Done {
        failed: usize,
    },
}

/// Что предлагает письмо с внешними картинками.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Только строка: «не загружаются никогда» или письмо помечено опасным.
    Never,
    /// «Загрузить картинки»; `domain` — для «Всегда для домена …» (адрес отправителя разобрался).
    Ask { domain: Option<String> },
    /// Домен отправителя — в доверенных: загружаются сами.
    Auto,
}

/// Что предлагает письмо от `from`: действующий выбор `images` (политика уже учтена), доверенные домены `security`,
/// письмо помечено опасным (`threat`).
pub(crate) fn offer(images: Images, security: &Security, from: &str, threat: bool) -> Offer {
    match images {
        Images::Never => Offer::Never,
        _ if threat => Offer::Never,
        Images::Ask if security.trusts_images_from(from) => Offer::Auto,
        Images::Ask => Offer::Ask { domain: sender_domain(from) },
    }
}

/// Адреса для загрузки: без повторов, не больше [`MAX_IMAGES`].
pub(crate) fn plan<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in urls {
        if out.len() == MAX_IMAGES {
            break;
        }
        if !out.iter().any(|o| o == u) {
            out.push(u.to_owned());
        }
    }
    out
}

/// Остаток байтов письма: предел каждой из `n` картинок, качающихся вместе, — поровну, не больше [`MAX_IMAGE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    left: usize,
}

impl Budget {
    pub(crate) fn new() -> Self {
        Budget { left: MAX_LETTER }
    }

    pub(crate) fn cap(&self, n: usize) -> usize {
        (self.left / n.max(1)).min(MAX_IMAGE)
    }

    pub(crate) fn spend(&mut self, bytes: usize) {
        self.left = self.left.saturating_sub(bytes);
    }

    pub(crate) fn empty(&self) -> bool {
        self.left == 0
    }
}

/// `http://` → `https://`: без TLS Postman по сети не ходит (как ссылки занятости).
pub(crate) fn https(url: &str) -> String {
    match url.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("http://") => format!("https://{}", &url[7..]),
        _ => url.to_owned(),
    }
}

/// Скачать картинку не больше `max` байт. Байты не разбирает. Блокирует поток — только в фоне.
pub(crate) trait Fetch: Send + Sync {
    fn fetch(&self, url: &str, max: usize) -> Result<Vec<u8>, String>;
}

/// Сеть: публичный ресурс без входа (`postman_backend_dav::fetch_public`).
pub(crate) struct Web;

impl Fetch for Web {
    fn fetch(&self, url: &str, max: usize) -> Result<Vec<u8>, String> {
        postman_backend_dav::fetch_public(&https(url), max)
    }
}

/// Демо и снимки: без сети, картинка-заглушка — полосы цвета адреса (PNG, раскодирует воркер, как настоящую).
pub(crate) struct Stub;

impl Fetch for Stub {
    fn fetch(&self, url: &str, max: usize) -> Result<Vec<u8>, String> {
        let png = stub_png(url)?;
        if png.len() > max { Err(format!("image is larger than {max} bytes")) } else { Ok(png) }
    }
}

/// Заглушка 600×200 (адрес с `banner`) или 220×140: цвет — от адреса.
fn stub_png(url: &str) -> Result<Vec<u8>, String> {
    let (w, h) = if url.contains("banner") { (600u32, 200u32) } else { (220, 140) };
    let seed = url.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let base = [(seed & 0x7f) as u8 + 60, ((seed >> 8) & 0x7f) as u8 + 60, ((seed >> 16) & 0x7f) as u8 + 90];
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let band = if (x / 40 + y / 40) % 2 == 0 { 0 } else { 40 };
            let shade = (y * 60 / h) as u8;
            pixels.extend(base.iter().map(|c| c.saturating_add(band).saturating_sub(shade)));
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Байты картинок писем в памяти: письмо → (адрес, байты); старые письма вытесняются.
type Letters = VecDeque<(String, Vec<(String, Arc<Vec<u8>>)>)>;

static CACHE: Mutex<Letters> = Mutex::new(VecDeque::new());

fn cached(letter: &str) -> Option<Vec<(String, Arc<Vec<u8>>)>> {
    let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.iter().find(|(id, _)| id == letter).map(|(_, v)| v.clone())
}

fn remember(letter: &str, images: Vec<(String, Arc<Vec<u8>>)>) {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|(id, _)| id != letter);
    cache.push_back((letter.to_owned(), images));
    while cache.len() > CACHED_LETTERS {
        cache.pop_front();
    }
}

/// Итог одной картинки: байты (для памяти; `None` — не пришли) и кадр или ошибка.
type Got = (Option<Arc<Vec<u8>>>, Result<Arc<RenderImage>, String>);

/// Одна картинка: байты из памяти или сети, затем — воркер. Байты — для памяти (`None` — не пришли).
fn get(fetch: &dyn Fetch, worker: &Mutex<Worker>, url: &str, max: usize, known: Option<Arc<Vec<u8>>>) -> Got {
    let bytes = match known {
        Some(b) => b,
        None => match fetch.fetch(url, max) {
            Ok(b) => Arc::new(b),
            Err(e) => return (None, Err(e)),
        },
    };
    let decoded = {
        let mut w = worker.lock().unwrap_or_else(PoisonError::into_inner);
        w.decode_image(&bytes, MAX_SIDE)
    };
    (Some(bytes), decoded.and_then(render_image))
}

impl ReadingPane {
    /// Письмо открылось или разобралось: картинки из памяти (уже загружались в этом запуске) или с доверенного домена
    /// — загрузить; иначе ждать выбора.
    pub(crate) fn ensure_remote(&mut self, cx: &mut Context<Self>) {
        let Some(r) = &self.reading else { return };
        let Some(doc) = &r.doc else { return };
        if r.remote != Remote::Blocked || doc.remote_urls().is_empty() {
            return;
        }
        let auto = self.remote_offer(cx) == Some(Offer::Auto);
        if auto || cached(&r.row.id).is_some() {
            self.load_remote(cx);
        }
    }

    /// Предложение открытого письма; `None` — письма или внешних картинок нет.
    pub(crate) fn remote_offer(&self, cx: &App) -> Option<Offer> {
        let r = self.reading.as_ref()?;
        r.doc.as_ref().filter(|d| !d.remote_urls().is_empty())?;
        let p = crate::prefs::get(cx);
        let images = crate::security::effective(&p).images;
        Some(offer(images, &p.security, &r.row.from_addr, postman_core::filters::is_threat(&r.row.keywords)))
    }

    /// «Загрузить картинки» (и доверенный домен): в фоне по [`PARALLEL`], каждая порция — сразу в письмо.
    pub(crate) fn load_remote(&mut self, cx: &mut Context<Self>) {
        if self.remote_offer_never(cx) {
            return;
        }
        let Some(r) = self.reading.as_mut() else { return };
        let Some(doc) = &r.doc else { return };
        if r.remote == Remote::Loading {
            return;
        }
        let urls = plan(doc.remote_urls());
        r.remote = Remote::Loading;
        let id = r.row.id.clone();
        let known = cached(&id).unwrap_or_default();
        let fetch: Arc<dyn Fetch> = if self.live { Arc::new(Web) } else { Arc::new(Stub) };
        let worker = self.worker.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut budget = Budget::new();
            let (mut failed, mut kept) = (0, Vec::new());
            for chunk in urls.chunks(PARALLEL) {
                if budget.empty() {
                    failed += chunk.len();
                    continue;
                }
                let cap = budget.cap(chunk.len());
                let tasks: Vec<_> = chunk
                    .iter()
                    .map(|url| {
                        let (fetch, worker, url) = (fetch.clone(), worker.clone(), url.clone());
                        let have = known.iter().find(|(u, _)| *u == url).map(|(_, b)| b.clone());
                        cx.background_executor().spawn(async move {
                            let got = get(fetch.as_ref(), &worker, &url, cap, have);
                            (url, got)
                        })
                    })
                    .collect();
                let results = futures::future::join_all(tasks).await;
                let mut shown = Vec::new();
                for (url, (bytes, image)) in results {
                    if let Some(b) = bytes {
                        budget.spend(b.len());
                        kept.push((url.clone(), b));
                    }
                    match image {
                        Ok(image) => shown.push((url, image)),
                        Err(e) => {
                            failed += 1;
                            tracing::warn!("remote image is not shown: {e}");
                        }
                    }
                }
                let current = this
                    .update(cx, |this, cx| {
                        let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) else { return false };
                        r.images.extend(shown);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    // Письмо закрыли: дальше не качаем; уже пришедшее — в памяти.
                    remember(&id, kept);
                    return;
                }
            }
            remember(&id, kept);
            this.update(cx, |this, cx| {
                if let Some(r) = this.reading.as_mut().filter(|r| r.row.id == id) {
                    r.remote = Remote::Done { failed };
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn remote_offer_never(&self, cx: &App) -> bool {
        matches!(self.remote_offer(cx), None | Some(Offer::Never))
    }

    /// «Всегда для домена …»: домен — в доверенные (Настройки → Безопасность), картинки письма — сейчас; тост.
    pub(crate) fn trust_remote(&mut self, domain: String, cx: &mut Context<Self>) {
        crate::prefs::update(cx, |p| {
            p.security.trust_image_domain(&domain);
        });
        cx.emit(ReadingEvent::Toast(Toast::Info, tr!("images-trusted", domain = domain)));
        self.load_remote(cx);
    }

    /// Строка над текстом письма: сколько картинок не загружено и что с ними сделать; загрузились все — строки нет.
    pub(crate) fn remote_banner(&self, l: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = self.reading.as_ref()?;
        let doc = r.doc.as_ref()?;
        if doc.remote_images_blocked == 0 {
            return None;
        }
        let offer = self.remote_offer(cx).unwrap_or(Offer::Never);
        let text = match r.remote {
            Remote::Loading => tr!("images-loading"),
            Remote::Done { failed: 0 } => return None,
            Remote::Done { failed } => tr!("images-failed", n = failed),
            Remote::Blocked if offer == Offer::Auto => tr!("images-loading"),
            Remote::Blocked => tr!("reading-remote-blocked", n = doc.remote_images_blocked),
        };
        let line = div()
            .id("reading-images")
            .children(crate::screens::spot("reading-images"))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(l.warning_bg())
            .child(div().flex_1().min_w(px(200.)).child(text));
        let Offer::Ask { domain } = offer else { return Some(line.into_any_element()) };
        if r.remote != Remote::Blocked {
            return Some(line.into_any_element());
        }
        let load = crate::buttons::secondary("images-load", l)
            .child(tr!("images-load"))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("images-load-hint")).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.load_remote(cx)));
        let trust = domain.map(|d| {
            let label = tr!("images-trust", domain = d.as_str());
            crate::buttons::secondary("images-trust", l)
                .child(label)
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new(tr!("images-trust-hint")).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.trust_remote(d.clone(), cx)))
        });
        Some(line.child(load).children(trust).into_any_element())
    }
}

/// «Настройки → Безопасность → Доверенные домены»: поле ввода и «домен не годится».
pub(crate) struct DomainsState {
    input: Entity<InputState>,
    bad: bool,
}

impl DomainsState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<SettingsView>, subscriptions: &mut Vec<Subscription>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("corp.example"));
        subscriptions.push(cx.subscribe_in(&input, window, |this: &mut SettingsView, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.add_image_domain(window, cx),
            InputEvent::Change if this.domains.bad => {
                this.domains.bad = false;
                cx.notify();
            }
            _ => {}
        }));
        Self { input, bad: false }
    }
}

impl SettingsView {
    /// «Добавить» и Enter в поле: домен — в доверенные, поле — пустое; не домен — подсказка.
    fn add_image_domain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.domains.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        match postman_core::prefs::image_domain(&text) {
            Some(d) => {
                crate::prefs::update(cx, |p| {
                    p.security.trust_image_domain(&d);
                });
                self.domains.input.update(cx, |s, cx| s.set_value("", window, cx));
                self.domains.bad = false;
            }
            None => self.domains.bad = true,
        }
        cx.notify();
    }

    /// Строка «Доверенные домены» под «Картинками из интернета»: поле с «Добавить», домены с «Убрать». При «не
    /// загружаются никогда» список не действует — так и сказано.
    pub(crate) fn image_domains(&mut self, l: &Look, cx: &mut Context<Self>) -> Div {
        let p = crate::prefs::get(cx);
        let ask = crate::security::effective(&p).images == Images::Ask;
        let mut hint = tr!(if self.domains.bad { "settings-images-domain-bad" } else { "settings-images-domains-hint" });
        if !ask {
            hint = format!("{hint} {}", tr!("settings-images-domains-off"));
        }
        let add = div().flex().items_center().gap_2().child(div().w(px(220.)).child(Input::new(&self.domains.input))).child(
            crate::buttons::button("settings-images-domain-add", tr!("settings-images-domain-add"), false, l)
                .on_click(cx.listener(|this, _, window, cx| this.add_image_domain(window, cx))),
        );
        let domains = p.security.trusted_image_domains;
        let list =
            div().flex().flex_col().pb_2().border_b_1().border_color(l.border()).when(domains.is_empty(), |d| {
                d.child(div().py_1().text_size(px(12.)).text_color(l.text2()).child(tr!("settings-images-domains-empty")))
            });
        let list = list.children(domains.into_iter().enumerate().map(|(i, d)| {
            let forget = d.clone();
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .child(crate::chrome::icon(crate::chrome::glyph::CHECK, 12.).text_color(l.text2()))
                .child(div().flex_1().min_w_0().when(!ask, |d| d.text_color(l.text2())).child(d))
                .child(link(SharedString::from(format!("settings-images-forget-{i}")), tr!("settings-images-forget"), l).on_click(cx.listener(
                    move |_, _, _, cx| {
                        crate::prefs::update(cx, |p| p.security.forget_image_domain(&forget));
                        cx.notify();
                    },
                )))
        }));
        div().flex().flex_col().child(row(tr!("settings-images-domains"), Some(hint), add, l).border_b_0()).child(list)
    }
}

/// Опись части (ADR 0005, `parts.rs`): тексты и сцены внешних картинок.
pub(crate) const PART: crate::part::Part =
    crate::part::Part { name: "images", texts: TEXTS, scenes: scenes::SCENES, reset: Some(scenes::reset), ..crate::part::Part::NONE };

const TEXTS: &[pets_i18n::Resource] = &[("ru", include_str!("../i18n/ru/images.ftl")), ("en", include_str!("../i18n/en/images.ftl"))];

/// Снимки (только по имени, `POSTMAN_SCREENS_ONLY`): письмо-рассылка с внешними картинками, «Загрузить картинки»
/// заглушкой без сети ([`Stub`]), список доверенных доменов в настройках.
pub(crate) mod scenes {
    use gpui_kit::*;
    use postman_core::prefs::Images;

    use super::Remote;
    use crate::Mailer;
    use crate::screens::{self, Scene};

    pub(crate) const SCENES: &[Scene] = &[
        // Рассылка с внешними картинками при «Спрашивать»: строка с «Загрузить картинки» и «Всегда для домена …».
        Scene::new("images-banner", banner, banner_ready).wait(newsletter).on_request(),
        // «Загрузить картинки»: баннер и две картинки-ссылки на месте подписей, строки нет.
        Scene::new("images-loaded", load, loaded).continues().on_request(),
        // Настройки → Безопасность: «Картинки из интернета — Спрашивать» и доверенные домены с «Убрать».
        Scene::new("settings-images-domains", settings, settings_ready).on_request(),
    ];

    /// Рассылка (адреса — только `example.test`): баннер, две картинки в ссылках, пиксель слежения.
    pub(crate) const NEWSLETTER: &str = include_str!("samples/newsletter.html");

    /// Своё состояние перед новой сценой (не продолжением): «не загружаются никогда», доверенных нет (как по
    /// умолчанию).
    pub(crate) fn reset(s: &Scene, _: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) {
        if s.continues {
            return;
        }
        let p = crate::prefs::get(cx).security;
        if p.images != Images::Never || !p.trusted_image_domains.is_empty() {
            crate::prefs::update(cx, |p| {
                p.security.images = Images::Never;
                p.security.trusted_image_domains.clear();
            });
        }
    }

    fn banner(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        crate::prefs::update(cx, |p| p.security.images = Images::Ask);
        m.set_section(crate::Section::Mail, cx);
        screens::select_demo(m, |b| screens::has(b, "exe"), cx)
    }

    /// Письмо открылось — тело заменить рассылкой (один раз), ленту беседы — выключить.
    fn newsletter(_: &Scene, m: &mut Mailer, cx: &mut Context<Mailer>) {
        let Some(r) = screens::reading(m, cx) else { return };
        let has = r.body.as_ref().is_some_and(|b| b.html.as_deref() == Some(NEWSLETTER));
        if has {
            return;
        }
        let (id, body) = (r.row.id.clone(), r.body.clone().unwrap_or_default());
        let body = postman_cache::BodyRow { html: Some(NEWSLETTER.into()), attachments: Vec::new(), ..body };
        m.reading.update(cx, |r, cx| {
            r.feed_on = false;
            r.show_body(&id, body, cx);
        });
    }

    fn banner_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::reading(m, cx)
            .is_some_and(|r| r.remote == Remote::Blocked && r.doc.as_ref().is_some_and(|d| d.remote_urls().len() == 3) && !m.reading.read(cx).feed_on)
            && screens::spot_bounds("images-trust").is_some()
    }

    fn load(_: &Scene, m: &mut Mailer, _: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        m.reading.update(cx, |r, cx| r.load_remote(cx));
        Ok(())
    }

    fn loaded(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::reading(m, cx).is_some_and(|r| r.remote == Remote::Done { failed: 0 } && r.images.len() >= 3)
    }

    fn settings(_: &Scene, m: &mut Mailer, window: &mut Window, cx: &mut Context<Mailer>) -> Result<(), String> {
        crate::prefs::update(cx, |p| {
            p.security.images = Images::Ask;
            p.security.trust_image_domain("corp.example");
            p.security.trust_image_domain("news.example.test");
        });
        m.open_settings(Some(crate::settings_tab::Section::Security), window, cx);
        Ok(())
    }

    fn settings_ready(_: &Scene, m: &Mailer, cx: &App) -> bool {
        screens::settings_on(m, crate::settings_tab::Section::Security, cx) && screens::spot_bounds("settings-images-forget-0").is_some()
    }
}

#[cfg(test)]
mod tests {
    // Не `use super::*`: вместе с gpui пришёл бы атрибут `gpui::test`, он перекрывает `#[test]`.
    use std::sync::{Arc, Mutex};

    use postman_core::prefs::{Images, Security};
    use postman_worker::Client as Worker;

    use super::{Budget, CACHED_LETTERS, Fetch, MAX_IMAGE, MAX_IMAGES, MAX_LETTER, Offer, PARALLEL, Stub, cached, get, https, offer, plan, remember};

    /// Выбор и политика: «никогда» и опасное письмо — без кнопок; доверенный домен — сами; иначе — спросить с доменом.
    #[test]
    fn offer_follows_choice_trust_and_threat() {
        let mut s = Security::default();
        s.trust_image_domain("corp.example");
        let from = "m.ivanova@corp.example";
        assert_eq!(offer(Images::Never, &s, from, false), Offer::Never, "никогда — и для доверенных");
        assert_eq!(offer(Images::Ask, &s, from, false), Offer::Auto);
        assert_eq!(offer(Images::Ask, &s, from, true), Offer::Never, "опасное письмо");
        assert_eq!(offer(Images::Ask, &s, "a@news.example.test", false), Offer::Ask { domain: Some("news.example.test".into()) });
        assert_eq!(offer(Images::Ask, &s, "не адрес", false), Offer::Ask { domain: None });
    }

    /// Адреса — без повторов и не больше предела; предел байтов — поровну на порцию, не больше картинки.
    #[test]
    fn plan_and_budget() {
        let many: Vec<String> = (0..MAX_IMAGES + 5).map(|i| format!("https://cdn.example.test/{i}.png")).collect();
        let mut urls: Vec<&str> = vec!["https://cdn.example.test/0.png"];
        urls.extend(many.iter().map(String::as_str));
        let p = plan(urls);
        assert_eq!(p.len(), MAX_IMAGES);
        assert_eq!((p[0].as_str(), p[1].as_str()), ("https://cdn.example.test/0.png", "https://cdn.example.test/1.png"));
        let mut b = Budget::new();
        assert_eq!(b.cap(PARALLEL), MAX_IMAGE);
        b.spend(MAX_LETTER - 1000);
        assert_eq!(b.cap(4), 250);
        assert!(!b.empty());
        b.spend(5000);
        assert!(b.empty());
        assert_eq!(b.cap(0), 0);
    }

    #[test]
    fn http_becomes_https() {
        assert_eq!(https("HTTP://cdn.example.test/a.png"), "https://cdn.example.test/a.png");
        assert_eq!(https("https://cdn.example.test/a.png"), "https://cdn.example.test/a.png");
    }

    /// Загрузчик подменяется (без сети): ошибка сети — без байтов; знакомые байты — без загрузки; память — по письмам,
    /// старые вытесняются.
    #[test]
    fn fetch_errors_and_memory() {
        struct Fail(Mutex<usize>);
        impl Fetch for Fail {
            fn fetch(&self, _: &str, _: usize) -> Result<Vec<u8>, String> {
                *self.0.lock().unwrap() += 1;
                Err("offline".into())
            }
        }
        let fail = Fail(Mutex::new(0));
        // Воркер не запускается: байтов нет — до него не доходит.
        let worker = Mutex::new(Worker::new("missing-worker.exe".into(), std::time::Duration::from_millis(10)));
        let (bytes, image) = get(&fail, &worker, "https://cdn.example.test/a.png", MAX_IMAGE, None);
        assert!(bytes.is_none() && image.is_err());
        let (bytes, _) = get(&fail, &worker, "https://cdn.example.test/a.png", MAX_IMAGE, Some(Arc::new(b"png".to_vec())));
        assert_eq!(bytes.as_deref().map(Vec::as_slice), Some(&b"png"[..]));
        assert_eq!(*fail.0.lock().unwrap(), 1, "из памяти — без сети");
        assert!(Stub.fetch("https://cdn.example.test/banner.png", 10).is_err(), "предел байтов");
        assert!(Stub.fetch("https://cdn.example.test/banner.png", MAX_IMAGE).is_ok_and(|b| b.starts_with(b"\x89PNG")));
        for i in 0..=CACHED_LETTERS {
            remember(&format!("t-{i}"), vec![(String::new(), Arc::new(vec![1]))]);
        }
        assert!(cached("t-0").is_none(), "вытеснено");
        assert!(cached(&format!("t-{CACHED_LETTERS}")).is_some());
    }
}
