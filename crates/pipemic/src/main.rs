//! Pipemic mixer window. Talks to pipemicd over its control socket
//! (`$XDG_RUNTIME_DIR/pipemicd.sock`, JSON lines). Polls status 4×/s so
//! changes made elsewhere (pipemicctl, the mic's touch pad) show up, the app
//! list every 2 s, and level meters ~30×/s.
//!
//! Layout: INPUTS = one strip per channel (header, meter, app box, one
//! fader per mix with its mute, link toggle); OUTPUTS = one row per mix
//! (output device, meter, mute, volume).

mod icons;
mod mic_art;
mod settings;

use icons::{AppIcon, Icons};
use iced::widget::{
    button, canvas, column, container, image, mouse_area, opaque, pick_list, row, scrollable, slider, stack, svg, text,
    text_input, toggler, vertical_slider, Space,
};
use settings::{Prefs, UpdateState};
use iced::{border, time, Alignment, Color, Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Pipemic")
        .subscription(App::subscription)
        .theme(|_: &App| theme())
        .window(iced::window::Settings {
            // Dev aid: PIPEMIC_SIZE=WxH opens at that size (for layout screenshots).
            size: std::env::var("PIPEMIC_SIZE")
                .ok()
                .and_then(|v| v.split_once('x').and_then(|(w, h)| Some(iced::Size::new(w.parse().ok()?, h.parse().ok()?))))
                .unwrap_or(iced::Size::new(1160.0, 780.0)),
            min_size: Some(iced::Size::new(640.0, 560.0)),
            // Wayland app_id / X11 WM_CLASS: matches pipemic.desktop so the
            // taskbar shows the right icon and pins work.
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "pipemic".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

// ---- layout breakpoints (logical px) ----------------------------------------

/// Strips narrower than this stack their header and use smaller toggles.
const COMPACT_STRIP: f32 = 140.0;
/// Below this content width the Device sidebar becomes a bar under the strips.
const SIDEBAR_MIN: f32 = 980.0;
/// Below this content width output rows drop the hint text and fixed widths.
const COMPACT_OUTPUTS: f32 = 1060.0;

/// Rough width of a label in the UI font (semibold), for choosing a layout
/// before iced measures it. Errs on the wide side.
fn est_text_width(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.62
}

/// Short form of a channel label for very narrow strips.
fn short_label(s: &str) -> String {
    match s {
        "Microphone" => "Mic".into(),
        _ => s.split_whitespace().next().unwrap_or(s).into(),
    }
}

// ---- look ----------------------------------------------------------------
//
// Dark, layered surfaces (window < card < well), Apple's dark-mode system
// colours for accents, one accent per channel carried into its faders, red
// only for "off", weight (not size) for hierarchy.

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}
const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}
const BG: Color = rgb(0x0f, 0x10, 0x12);
const CARD: Color = rgb(0x1c, 0x1d, 0x21);
const WELL: Color = rgb(0x13, 0x14, 0x17);
const RAIL: Color = rgb(0x2e, 0x30, 0x36);
const HAIRLINE: Color = rgba(0xff, 0xff, 0xff, 0.06);
const DIM: Color = rgb(0x8e, 0x8e, 0x93);
const TEXT: Color = rgb(0xf2, 0xf2, 0xf7);
const MUTED: Color = rgb(0xff, 0x45, 0x3a);
const ON: Color = rgb(0x30, 0xd1, 0x58);
const ACCENT: Color = rgb(0x0a, 0x84, 0xff);
const METER_OK: Color = rgb(0x30, 0xd1, 0x58);
const METER_HOT: Color = rgb(0xff, 0xd6, 0x0a);
const METER_CLIP: Color = rgb(0xff, 0x45, 0x3a);
const TOOLTIP: Color = rgb(0x2c, 0x2d, 0x32);

const SEMIBOLD: iced::Font = iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT };

fn theme() -> Theme {
    Theme::custom(
        "Pipemic".to_string(),
        iced::theme::Palette { background: BG, text: TEXT, primary: ACCENT, success: ON, warning: METER_HOT, danger: MUTED },
    )
}

/// Accent per channel: header badge and its faders' fill.
fn channel_color(id: &str) -> Color {
    match id {
        "mic" => rgb(0x0a, 0x84, 0xff),
        "music" => rgb(0xff, 0x37, 0x5f),
        "browser" => rgb(0xbf, 0x5a, 0xf2),
        "voicechat" => rgb(0xff, 0xd6, 0x0a),
        "system" | "other" => rgb(0x64, 0xd2, 0xff),
        _ => DIM,
    }
}

/// Daemon gain range for the Wave:3 (dB).
const GAIN_MAX: f32 = 40.0;

/// Section heading: small, semibold, secondary colour.
fn heading<'a>(s: &'a str) -> iced::widget::Text<'a> {
    text(s).size(11).font(SEMIBOLD).color(DIM)
}

fn panel<'a>(content: impl Into<Element<'a, Message>>, color: Color, radius: f32) -> container::Container<'a, Message> {
    container(content).style(move |_| container::Style {
        background: Some(color.into()),
        border: border::rounded(radius),
        ..Default::default()
    })
}

/// Raised surface: card colour, hairline edge, soft shadow.
fn card<'a>(content: impl Into<Element<'a, Message>>) -> container::Container<'a, Message> {
    container(content).style(|_| container::Style {
        background: Some(CARD.into()),
        border: iced::Border { color: HAIRLINE, width: 1.0, radius: 14.0.into() },
        shadow: iced::Shadow { color: rgba(0, 0, 0, 0.35), offset: iced::Vector::new(0.0, 6.0), blur_radius: 18.0 },
        ..Default::default()
    })
}

fn icon<'a>(src: &'static str, size: f32, color: Color) -> Element<'a, Message> {
    svg(icons::glyph(src)).width(size).height(size).style(move |_, _| svg::Style { color: Some(color) }).into()
}

fn app_icon<'a>(i: &AppIcon, size: f32) -> Element<'a, Message> {
    match i {
        AppIcon::Svg(h) => svg(h.clone()).width(size).height(size).into(),
        AppIcon::Raster(h) => image(h.clone()).width(size).height(size).into(),
    }
}

/// Peak (linear) → bar fraction on a -60..0 dB scale.
fn meter_frac(peak: f32) -> f32 {
    if peak <= 0.001 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn meter_color(peak: f32) -> Color {
    if peak >= 0.98 {
        METER_CLIP
    } else if meter_frac(peak) > 0.95 {
        METER_HOT
    } else {
        METER_OK
    }
}

/// One horizontal bar that fills its width by level.
fn hbar<'a>(peak: f32, height: f32) -> Element<'a, Message> {
    let f = meter_frac(peak);
    let parts = (f * 1000.0).round() as u16;
    let c = meter_color(peak);
    let mut r = row![];
    if parts > 0 {
        r = r.push(panel(Space::new(), c, height / 2.0).width(Length::FillPortion(parts)).height(height));
    }
    if parts < 1000 {
        r = r.push(Space::new().width(Length::FillPortion(1000 - parts)));
    }
    panel(r, WELL, height / 2.0).width(Length::Fill).height(height).into()
}

/// Stereo meter: two thin bars stacked.
fn hmeter<'a>(lr: (f32, f32)) -> Element<'a, Message> {
    column![hbar(lr.0, 3.0), hbar(lr.1, 3.0)].spacing(2).width(Length::Fill).into()
}

/// Fader: the filled part of the track carries the accent; a white pill
/// handle; muted dims the fill.
fn fader_style(accent: Color, muted: bool) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |_, status| {
        let fill = if muted { rgba(0x8e, 0x8e, 0x93, 0.35) } else { accent };
        let handle = match status {
            slider::Status::Dragged => rgb(0xd8, 0xd8, 0xde),
            _ => if muted { rgb(0xb0, 0xb0, 0xb6) } else { Color::WHITE },
        };
        slider::Style {
            rail: slider::Rail { backgrounds: (fill.into(), RAIL.into()), width: 6.0, border: border::rounded(3) },
            handle: slider::Handle {
                shape: slider::HandleShape::Rectangle { width: 14, border_radius: 7.0.into() },
                background: handle.into(),
                border_width: 2.0,
                border_color: CARD,
            },
        }
    }
}

/// Quiet button: transparent until hovered; `active` tints it with `color`.
/// Pressed darkens immediately (feedback on press, not release).
fn flat_button(active: bool, color: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = match (active, status) {
            (true, button::Status::Pressed) => Color { a: 0.26, ..color },
            (true, _) => Color { a: 0.16, ..color },
            (false, button::Status::Hovered) => rgba(0xff, 0xff, 0xff, 0.07),
            (false, button::Status::Pressed) => rgba(0xff, 0xff, 0xff, 0.12),
            (false, _) => Color::TRANSPARENT,
        };
        button::Style { background: Some(bg.into()), text_color: TEXT, border: border::rounded(8), ..Default::default() }
    }
}

/// Filled secondary button (Close, Check for updates…).
fn soft_button(_: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => rgb(0x3a, 0x3b, 0x41),
        button::Status::Pressed => rgb(0x26, 0x27, 0x2c),
        _ => rgb(0x30, 0x31, 0x37),
    };
    button::Style { background: Some(bg.into()), text_color: TEXT, border: border::rounded(8), ..Default::default() }
}

/// The big mic mute: red and filled while muted, quiet otherwise.
fn mute_style(muted: bool, t: &Theme, st: button::Status) -> button::Style {
    if muted {
        let bg = if matches!(st, button::Status::Pressed) { rgb(0xd6, 0x3a, 0x30) } else { MUTED };
        button::Style { background: Some(bg.into()), text_color: Color::WHITE, border: border::rounded(10), ..Default::default() }
    } else {
        let mut b = soft_button(t, st);
        b.border = border::rounded(10);
        b
    }
}

/// Filled primary button (Update now).
fn primary_button(_: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => rgb(0x32, 0x98, 0xff),
        button::Status::Pressed => rgb(0x08, 0x6d, 0xd6),
        _ => ACCENT,
    };
    button::Style { background: Some(bg.into()), text_color: Color::WHITE, border: border::rounded(8), ..Default::default() }
}

fn tooltip_box<'a>(hint: String) -> container::Container<'a, Message> {
    container(text(hint).size(12)).padding([6, 10]).style(|_| container::Style {
        background: Some(TOOLTIP.into()),
        border: iced::Border { color: HAIRLINE, width: 1.0, radius: 8.0.into() },
        shadow: iced::Shadow { color: rgba(0, 0, 0, 0.4), offset: iced::Vector::new(0.0, 4.0), blur_radius: 12.0 },
        ..Default::default()
    })
}

/// Mute toggle drawn as the mix's glyph, struck through and red when
/// muted, with a hover hint saying what it does.
fn mute_toggle<'a>(glyph: &'static str, muted: bool, hint: String, msg: Message) -> Element<'a, Message> {
    mute_toggle_sized(glyph, muted, hint, msg, 22.0, 7)
}

fn mute_toggle_sized<'a>(glyph: &'static str, muted: bool, hint: String, msg: Message, size: f32, pad: u16) -> Element<'a, Message> {
    let c = if muted { MUTED } else { TEXT };
    let mut layers = iced::widget::Stack::new().push(icon(glyph, size, c));
    if muted {
        layers = layers.push(icon(icons::SLASH, size, c));
    }
    let b = button(layers).padding(pad).style(flat_button(muted, MUTED)).on_press(msg);
    iced::widget::tooltip(b, tooltip_box(hint), iced::widget::tooltip::Position::Top).gap(6).into()
}

/// Live / Muted capsule.
fn status_pill<'a>(muted: bool) -> Element<'a, Message> {
    let c = if muted { MUTED } else { ON };
    container(
        row![
            container(Space::new()).width(6).height(6).style(move |_| container::Style {
                background: Some(c.into()),
                border: border::rounded(3),
                ..Default::default()
            }),
            text(if muted { "Muted" } else { "Live" }).size(11).font(SEMIBOLD).color(c),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([3, 10])
    .style(move |_| container::Style { background: Some(Color { a: 0.14, ..c }.into()), border: border::rounded(10), ..Default::default() })
    .into()
}

/// The dialog surface: a heavier card with a deeper shadow.
fn sheet<'a>(content: impl Into<Element<'a, Message>>) -> container::Container<'a, Message> {
    container(content).style(|_| container::Style {
        background: Some(CARD.into()),
        border: iced::Border { color: HAIRLINE, width: 1.0, radius: 18.0.into() },
        shadow: iced::Shadow { color: rgba(0, 0, 0, 0.55), offset: iced::Vector::new(0.0, 18.0), blur_radius: 48.0 },
        ..Default::default()
    })
}

/// Inset group of settings rows (well colour, rounded).
fn group<'a>(content: iced::widget::Column<'a, Message>) -> Element<'a, Message> {
    panel(content.spacing(12), WELL, 12.0).padding([14, 16]).width(Length::Fill).into()
}

fn separator<'a>() -> Element<'a, Message> {
    container(Space::new()).width(Length::Fill).height(1).style(|_| container::background(HAIRLINE)).into()
}

fn switch_style(_: &Theme, status: iced::widget::toggler::Status) -> iced::widget::toggler::Style {
    use iced::widget::toggler::Status as S;
    let on = match status {
        S::Active { is_toggled } | S::Hovered { is_toggled } => is_toggled,
        S::Disabled { is_toggled } => is_toggled,
    };
    iced::widget::toggler::Style {
        background: (if on { ON } else { RAIL }).into(),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Color::WHITE.into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(TEXT),
        border_radius: None,
        padding_ratio: 0.1,
    }
}

/// Label on the left, switch on the right.
fn switch_row<'a>(label: &'a str, on: bool, msg: impl Fn(bool) -> Message + 'a) -> Element<'a, Message> {
    row![text(label).size(13).width(Length::Fill), toggler(on).size(20).style(switch_style).on_toggle(msg)]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
}

fn info_row<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    row![text(label).size(13).width(Length::Fill), text(value).size(13).color(DIM)].spacing(12).into()
}

/// Segmented control: a well with the selected segment raised.
fn segmented<'a>(items: &[(&'a str, u8)], selected: u8, msg: impl Fn(u8) -> Message) -> Element<'a, Message> {
    let mut r = row![].spacing(2);
    for &(label, i) in items {
        let on = i == selected;
        r = r.push(
            button(text(label).size(12).font(if on { SEMIBOLD } else { iced::Font::DEFAULT }).center())
                .padding([6, 14])
                .style(move |_, st| {
                    let bg = if on {
                        rgb(0x3a, 0x3b, 0x42)
                    } else if matches!(st, button::Status::Hovered) {
                        rgba(0xff, 0xff, 0xff, 0.05)
                    } else {
                        Color::TRANSPARENT
                    };
                    button::Style {
                        background: Some(bg.into()),
                        text_color: if on { TEXT } else { DIM },
                        border: border::rounded(7),
                        ..Default::default()
                    }
                })
                .on_press(msg(i)),
        );
    }
    panel(r, WELL, 9.0).padding(3).into()
}

fn search_style(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: WELL.into(),
        border: iced::Border { color: if focused { ACCENT } else { HAIRLINE }, width: 1.0, radius: 10.0.into() },
        icon: DIM,
        placeholder: DIM,
        value: TEXT,
        selection: Color { a: 0.35, ..ACCENT },
    }
}

fn picker_style(_: &Theme, status: pick_list::Status) -> pick_list::Style {
    let hover = matches!(status, pick_list::Status::Hovered | pick_list::Status::Opened { .. });
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: DIM,
        handle_color: DIM,
        background: (if hover { rgb(0x26, 0x27, 0x2c) } else { WELL }).into(),
        border: iced::Border { color: HAIRLINE, width: 1.0, radius: 8.0.into() },
    }
}

/// A dialog over a dimmed backdrop; clicking the backdrop sends `close`.
fn modal<'a>(card: Element<'a, Message>, close: Message) -> Element<'a, Message> {
    opaque(
        mouse_area(container(opaque(card)).center(Length::Fill).style(|_| container::background(rgba(0, 0, 0, 0.55))))
            .on_press(close),
    )
}

fn db_label(vol: f32) -> String {
    if vol <= -60.0 {
        "-∞".into()
    } else {
        format!("{vol:.0} dB")
    }
}

// ---- socket client -------------------------------------------------------

fn socket_path() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(Into::into)
        .unwrap_or_else(std::env::temp_dir)
        .join("pipemicd.sock")
}

fn call(req: Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket_path()).map_err(|_| "pipemicd is not running".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(4))).ok();
    s.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("error").to_string())
    }
}

async fn send(req: Value) -> Result<Value, String> {
    // Local socket, sub-millisecond except for mic writes (one USB round trip).
    call(req)
}

// ---- state ---------------------------------------------------------------

/// Something with a volume (dB) and a mute.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Mix(String),
    Route { mix: String, channel: String },
    MicGain,
    /// Another Wave:3 level: "headphone_volume" (dB) or "monitor" (% mic).
    MicParam(&'static str),
}

impl Target {
    fn request(&self, volume: Option<f32>, mute: Option<bool>) -> Value {
        let mut v = match self {
            Target::Mix(m) => json!({"cmd": "mix", "mix": m}),
            Target::Route { mix, channel } => json!({"cmd": "route", "mix": mix, "channel": channel}),
            Target::MicGain | Target::MicParam(_) => json!({"cmd": "mic"}),
        };
        if let Some(x) = volume {
            let key = match self {
                Target::MicGain => "gain",
                Target::MicParam(k) => k,
                _ => "volume",
            };
            v[key] = json!(x);
        }
        if let Some(m) = mute {
            v[if *self == Target::MicParam("headphone_volume") { "headphone_mute" } else { "mute" }] = json!(m);
        }
        v
    }

    /// A USB write on the mic: send on release, not while dragging.
    fn is_usb(&self) -> bool {
        matches!(self, Target::MicGain | Target::MicParam(_))
    }
}

/// An entry of the Personal output picker.
#[derive(Debug, Clone, PartialEq)]
struct Output {
    /// node.name; "" = follow the default output.
    name: String,
    label: String,
}

impl std::fmt::Display for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    AppsTick,
    MeterTick,
    Status(Result<Value, String>),
    Apps(Result<Value, String>),
    Levels(Result<Value, String>),
    Sent(Result<Value, String>),
    Slide(Target, f32),
    Release,
    Mute(Target, bool),
    Link(String, bool),
    MicFlag(&'static str, bool),
    /// Open/close a strip's panel (app picker, or mic settings).
    Toggle(String),
    Assign(String, Option<String>),
    SetOutput(String, Output),
    Search(String),
    ShowSettings(bool),
    AutoCheck(bool),
    Autostart(bool),
    OutputFallback(bool),
    CheckUpdates,
    UpdateNow,
    UpdateDone(UpdateState),
    CheckFirmware,
    FirmwareDone(Result<wave_catalog::Firmware, String>),
    ShowDevice(bool),
    DeviceTab(u8),
    Knob(&'static str),
    /// Dev aid: PIPEMIC_SCREENSHOT=path saves the window as PNG ~3 s after start.
    Shoot,
    Shot(iced::window::Screenshot),
}

#[derive(Default)]
struct App {
    config: Value,
    mic: Value,
    levels: Value,
    apps: Vec<Value>,
    outputs: Vec<Output>,
    icons: Icons,
    app_icons: HashMap<String, Option<AppIcon>>,
    error: Option<String>,
    /// Held fader(s): local values shown instead of polled state. More than
    /// one when the channel's faders are linked.
    dragging: Vec<(Target, f32)>,
    /// A levels request is in flight; don't stack more.
    metering: bool,
    /// Channel whose panel is open.
    open: Option<String>,
    /// App picker filter.
    search: String,
    prefs: Option<Prefs>,
    autostart: bool,
    show_settings: bool,
    update: Option<UpdateState>,
    show_device: bool,
    /// 0 = input & monitoring, 1 = device info
    device_tab: u8,
    /// Mic firmware update check (Device info tab).
    fw: FwCheck,
}

#[derive(Debug, Clone, Default)]
enum FwCheck {
    #[default]
    Idle,
    Checking,
    Done(Result<wave_catalog::Firmware, String>),
}

async fn tokio_sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn save_png(path: &std::ffi::OsStr, shot: &iced::window::Screenshot) -> Result<(), Box<dyn std::error::Error>> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(f, shot.size.width, shot.size.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&shot.rgba)?;
    Ok(())
}

/// Run blocking work (git, the installer) off the UI thread.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("worker panicked")
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let prefs = Prefs::load();
        let mut tasks = vec![
            Task::perform(send(json!({"cmd": "status"})), Message::Status),
            Task::perform(send(json!({"cmd": "apps"})), Message::Apps),
        ];
        if prefs.auto_check {
            tasks.push(Task::perform(blocking(settings::check), Message::UpdateDone));
        }
        if std::env::var_os("PIPEMIC_SCREENSHOT").is_some() {
            tasks.push(Task::perform(tokio_sleep(3000), |_| Message::Shoot));
        }
        let view = std::env::var("PIPEMIC_VIEW").unwrap_or_default();
        if view == "device-info" {
            tasks.push(Task::done(Message::CheckFirmware));
        }
        let app = Self {
            show_device: view == "device" || view == "device-info",
            device_tab: (view == "device-info") as u8,
            show_settings: view == "settings",
            open: view.strip_prefix("picker:").map(String::from), prefs: Some(prefs), autostart: settings::autostart(), update: Some(UpdateState::Idle), ..Self::default() };
        (app, Task::batch(tasks))
    }

    fn apps_request(&self) -> Task<Message> {
        // With installed apps: the strips show every app a channel holds.
        Task::perform(send(json!({"cmd": "apps", "installed": true})), Message::Apps)
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            time::every(Duration::from_secs(2)).map(|_| Message::AppsTick),
            time::every(Duration::from_millis(33)).map(|_| Message::MeterTick),
        ])
    }

    fn take_reply(&mut self, v: &Value) {
        if !v["config"].is_null() {
            self.config = v["config"].clone();
        }
        if !v["mic"].is_null() {
            self.mic = v["mic"].clone();
        }
        if let Some(a) = v["apps"].as_array() {
            self.apps = a.clone();
            for app in &self.apps {
                let Some(icon) = app["icon"].as_str().filter(|i| !i.is_empty()) else { continue };
                if !self.app_icons.contains_key(icon) {
                    let i = self.icons.get(icon);
                    self.app_icons.insert(icon.to_owned(), i);
                }
            }
        }
        if let Some(o) = v["outputs"].as_array() {
            self.outputs = std::iter::once(Output { name: String::new(), label: "Default output".into() })
                .chain(o.iter().map(|x| Output {
                    name: x["name"].as_str().unwrap_or("").into(),
                    label: x["description"].as_str().unwrap_or("?").into(),
                }))
                .collect();
        }
        self.error = None;
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Tick => return Task::perform(send(json!({"cmd": "status"})), Message::Status),
            Message::AppsTick => return self.apps_request(),
            Message::MeterTick => {
                if !self.metering && self.error.is_none() {
                    self.metering = true;
                    return Task::perform(send(json!({"cmd": "levels"})), Message::Levels);
                }
            }
            Message::Levels(r) => {
                self.metering = false;
                self.levels = r.map(|v| v["levels"].clone()).unwrap_or(Value::Null);
            }
            Message::Status(r) | Message::Sent(r) | Message::Apps(r) => match r {
                Ok(v) => self.take_reply(&v),
                Err(e) => self.error = Some(e),
            },
            Message::Slide(t, v) => {
                let mut moved = vec![(t.clone(), v)];
                // Linked channel: the other mix faders follow by the same delta.
                if let Target::Route { mix, channel } = &t {
                    if self.linked(channel) {
                        let (from, _) = self.level(&t);
                        for m in self.mix_ids() {
                            if &m != mix {
                                let other = Target::Route { mix: m, channel: channel.clone() };
                                let (ov, _) = self.level(&other);
                                moved.push((other, (ov + v - from).clamp(-60.0, 0.0)));
                            }
                        }
                    }
                }
                let changed = self.dragging.first().is_none_or(|(_, old)| (old - v).abs() > f32::EPSILON);
                self.dragging = moved.clone();
                // Mic gain is a USB write: only send on release.
                if changed && !t.is_usb() {
                    return Task::batch(moved.into_iter().map(|(t, v)| Task::perform(send(t.request(Some(v), None)), Message::Sent)));
                }
            }
            Message::Release => {
                let held = std::mem::take(&mut self.dragging);
                return Task::batch(held.into_iter().map(|(t, v)| Task::perform(send(t.request(Some(v), None)), Message::Sent)));
            }
            Message::Mute(t, m) => return Task::perform(send(t.request(None, Some(m))), Message::Sent),
            Message::Link(c, on) => {
                return Task::perform(send(json!({"cmd": "channel", "channel": c, "link": on})), Message::Sent)
            }
            Message::MicFlag(k, on) => {
                let mut req = json!({"cmd": "mic"});
                req[k] = json!(on);
                return Task::perform(send(req), Message::Sent);
            }
            Message::Toggle(c) => {
                self.open = if self.open.as_ref() == Some(&c) { None } else { Some(c) };
                self.search.clear();
                return self.apps_request();
            }
            Message::Search(q) => self.search = q,
            Message::ShowSettings(on) => self.show_settings = on,
            Message::AutoCheck(on) => {
                if let Some(p) = &mut self.prefs {
                    p.auto_check = on;
                    p.save();
                }
            }
            Message::Autostart(on) => match settings::set_autostart(on) {
                Ok(()) => self.autostart = on,
                Err(e) => self.error = Some(e),
            },
            Message::OutputFallback(on) => {
                return Task::perform(send(json!({"cmd": "settings", "output_fallback": on})), Message::Sent)
            }
            Message::CheckUpdates => {
                self.update = Some(UpdateState::Busy("Checking…"));
                return Task::perform(blocking(settings::check), Message::UpdateDone);
            }
            Message::UpdateNow => {
                self.update = Some(UpdateState::Busy("Updating… (building, this takes a minute)"));
                return Task::perform(blocking(settings::update), Message::UpdateDone);
            }
            Message::UpdateDone(u) => self.update = Some(u),
            Message::CheckFirmware => {
                self.fw = FwCheck::Checking;
                return Task::perform(blocking(wave_catalog::latest_wave3), Message::FirmwareDone);
            }
            Message::FirmwareDone(r) => self.fw = FwCheck::Done(r),
            Message::ShowDevice(on) => self.show_device = on,
            Message::DeviceTab(t) => self.device_tab = t,
            Message::Shoot => {
                return iced::window::latest().and_then(iced::window::screenshot).map(Message::Shot);
            }
            Message::Shot(shot) => {
                if let Some(path) = std::env::var_os("PIPEMIC_SCREENSHOT") {
                    if let Err(e) = save_png(&path, &shot) {
                        eprintln!("pipemic: screenshot failed: {e}");
                    }
                }
            }
            Message::Knob(k) => return Task::perform(send(json!({"cmd": "mic", "knob": k})), Message::Sent),
            Message::Assign(app, channel) => {
                return Task::perform(send(json!({"cmd": "assign", "app": app, "channel": channel})), Message::Apps)
            }
            Message::SetOutput(mix, o) => {
                return Task::perform(send(json!({"cmd": "mix", "mix": mix, "output": o.name})), Message::Sent)
            }
        }
        Task::none()
    }

    // ---- model helpers ---------------------------------------------------

    fn mix_ids(&self) -> Vec<String> {
        self.config["mix"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect()).unwrap_or_default()
    }

    fn channel(&self, id: &str) -> Value {
        self.config["channel"].as_array().and_then(|a| a.iter().find(|c| c["id"] == id).cloned()).unwrap_or(Value::Null)
    }

    fn linked(&self, channel: &str) -> bool {
        self.channel(channel)["link"] == true
    }

    /// Level as shown: a held fader's local value, else the daemon's state.
    fn level(&self, t: &Target) -> (f32, bool) {
        let (vol, mute) = self.saved(t);
        match self.dragging.iter().find(|(dt, _)| dt == t) {
            Some((_, v)) => (*v, mute),
            None => (vol, mute),
        }
    }

    /// (volume dB, mute) from the last status the daemon sent.
    fn saved(&self, t: &Target) -> (f32, bool) {
        let v = match t {
            Target::Mix(m) => {
                self.config["mix"].as_array().and_then(|a| a.iter().find(|x| x["id"] == *m).cloned()).unwrap_or(Value::Null)
            }
            Target::Route { mix, channel } => self.config["route"]
                .as_array()
                .and_then(|a| a.iter().find(|r| r["mix"] == *mix && r["channel"] == *channel).cloned())
                .unwrap_or(json!({"volume": 0.0, "mute": false})),
            Target::MicGain => return (self.mic["gain"].as_f64().unwrap_or(0.0) as f32, self.mic["mute"] == true),
            Target::MicParam("headphone_volume") => {
                return (self.mic["headphone_volume"].as_f64().unwrap_or(0.0) as f32, self.mic["headphone_mute"] == true)
            }
            Target::MicParam(k) => return (self.mic[*k].as_f64().unwrap_or(0.0) as f32, false),
        };
        (v["volume"].as_f64().unwrap_or(0.0) as f32, v["mute"] == true)
    }

    /// Meter reading (L, R) for a channel or mix id.
    fn peak(&self, kind: &str, id: &str) -> (f32, f32) {
        let v = &self.levels[kind][id];
        (v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32)
    }

    /// Apps a channel holds (by type or by your choice), playing ones first.
    fn apps_in(&self, channel: &str) -> Vec<&Value> {
        let mut v: Vec<&Value> = self.apps.iter().filter(|a| a["channel"] == channel).collect();
        v.sort_by_key(|a| a["running"] != true);
        v
    }

    /// Channels that take apps by type; the others (Other) only hold what you add.
    fn is_auto(&self, channel: &str) -> bool {
        self.channel(channel)["categories"].as_array().is_some_and(|c| !c.is_empty())
    }

    fn icon_of(&self, app: &Value) -> Option<AppIcon> {
        app["icon"].as_str().and_then(|i| self.app_icons.get(i).cloned().flatten())
    }

    fn channel_label(&self, id: &str) -> String {
        self.channel(id)["label"].as_str().unwrap_or(id).to_string()
    }

    // ---- view ------------------------------------------------------------

    fn mix_glyph(m: &Value) -> &'static str {
        if m["kind"] == "monitor" {
            icons::HEADPHONES
        } else {
            icons::OUTGOING
        }
    }

    fn mix_hint_for(&self, cid: &str, m: &Value, muted: bool) -> String {
        Self::mute_hint(m, &self.channel_label(cid), muted)
    }

    fn mute_hint(m: &Value, channel: &str, muted: bool) -> String {
        match (m["kind"] == "monitor", muted) {
            (true, false) => format!("You hear {channel}. Click to mute it for you."),
            (true, true) => format!("You don't hear {channel}. Click to hear it."),
            (false, false) => format!("Others hear {channel}. Click to stop sending it."),
            (false, true) => format!("{channel} isn't sent to others. Click to send it."),
        }
    }

    /// One channel strip. `width` is the strip's outer width; below
    /// `COMPACT_STRIP` the header stacks and the controls tighten so nothing
    /// spills into the next strip.
    /// `label_w` is the widest label's estimated width, so every strip picks
    /// the same header form.
    fn strip<'a>(&'a self, c: &Value, mixes: &'a [Value], width: f32, label_w: f32) -> Element<'a, Message> {
        let compact = width < COMPACT_STRIP;
        let inner_w = width - 24.0;
        let cid = c["id"].as_str().unwrap_or("").to_string();
        let is_mic = c["device"].is_string();
        let label = c["label"].as_str().unwrap_or("?").to_string();
        let color = channel_color(&cid);
        let pad_muted = is_mic && self.mic["mute"] == true;
        let open = self.open.as_deref() == Some(cid.as_str());
        let tap = if is_mic { Message::ShowDevice(true) } else { Message::Toggle(cid.clone()) };

        let badge = container(icon(icons::channel_glyph(&cid), 16.0, Color::WHITE))
            .center(30)
            .style(move |_| container::Style {
                background: Some((if pad_muted { MUTED } else { color }).into()),
                border: border::rounded(9),
                ..Default::default()
            });
        // Pick the header form that fits: badge + label + chevron, then
        // without the chevron, then badge above a (possibly shortened) label.
        let avail = inner_w - 8.0 - 30.0 - 10.0;
        let wide_w = label_w;
        let header_body: Element<'a, Message> = if avail < wide_w {
            let label = if est_text_width(&label, 12.0) > inner_w - 8.0 { short_label(&label) } else { label };
            column![
                badge,
                container(text(label).size(12).font(SEMIBOLD).wrapping(iced::widget::text::Wrapping::None))
                    .center_x(Length::Fill)
                    .clip(true),
            ]
            .spacing(6)
            .align_x(Alignment::Center)
            .width(Length::Fill)
            .into()
        } else {
            row![
                badge,
                container(text(label).size(14).font(SEMIBOLD).wrapping(iced::widget::text::Wrapping::None))
                    .width(Length::Fill)
                    .clip(true),
            ]
            .push((avail - 24.0 >= wide_w).then(|| icon(icons::CHEVRON, 14.0, if open { TEXT } else { DIM })))
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        };
        let header = button(header_body)
        .padding([4, 4])
        .width(Length::Fill)
        .style(flat_button(false, TEXT))
        .on_press(tap.clone());

        // App box: who plays into this channel (or the mic itself).
        let inner: Element<'a, Message> = if is_mic {
            if self.mic["connected"] != true {
                text(if compact { "Not connected" } else { "Wave:3 not connected" }).size(11).color(DIM).center().into()
            } else if pad_muted {
                row![icon(icons::MIC, 20.0, MUTED), text("Muted").size(12).font(SEMIBOLD).color(MUTED)]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .into()
            } else {
                let mut r = row![icon(icons::MIC, 20.0, TEXT)].spacing(6).align_y(Alignment::Center);
                if inner_w >= 90.0 {
                    r = r.push(text("Wave:3").size(12).color(DIM));
                }
                r.into()
            }
        } else {
            let apps = self.apps_in(&cid);
            let add = container(text("+").size(16).font(SEMIBOLD).color(DIM)).center(26).style(|_| container::Style {
                border: iced::Border { color: RAIL, width: 1.0, radius: 13.0.into() },
                ..Default::default()
            });
            if apps.is_empty() {
                let mut r = row![add].spacing(8).align_y(Alignment::Center);
                if inner_w >= 110.0 {
                    r = r.push(text("Add apps").size(12).color(DIM));
                }
                r.into()
            } else {
                // Fit as many icons as the box allows (22 px + 7 gap each),
                // keeping room for "+N" and, when wide enough, the add circle.
                let show_add = !compact;
                let room = inner_w - 16.0 - if show_add { 33.0 } else { 0.0 };
                let fit = ((room + 7.0) / 29.0).floor().max(1.0) as usize;
                let shown = if apps.len() > fit { fit.saturating_sub(1).max(1) } else { apps.len() };
                let mut r = row![].spacing(7).align_y(Alignment::Center);
                for a in apps.iter().take(shown) {
                    let name = a["name"].as_str().unwrap_or("");
                    let el: Element<'a, Message> = match self.icon_of(a) {
                        Some(i) => app_icon(&i, 22.0),
                        None => text(name.chars().next().unwrap_or('?').to_uppercase().to_string()).size(13).font(SEMIBOLD).into(),
                    };
                    let tip = if a["running"] == true { format!("{name} (playing)") } else { name.to_string() };
                    r = r.push(iced::widget::tooltip(el, tooltip_box(tip), iced::widget::tooltip::Position::Top).gap(6));
                }
                if apps.len() > shown {
                    r = r.push(text(format!("+{}", apps.len() - shown)).size(11).color(DIM));
                }
                if show_add {
                    r = r.push(add);
                }
                r.into()
            }
        };
        let app_box = mouse_area(panel(container(inner).center(Length::Fill), WELL, 10.0).width(Length::Fill).height(48)).on_press(tap);

        let _ = open;
        // Two mute toggles (26 px each in compact) plus the link button (21 px).
        let body = self.faders(&cid, mixes, color, compact, !compact || inner_w >= 76.0);

        card(column![header, hmeter(self.peak("channel", &cid)), app_box, body].spacing(12).padding(12))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn faders<'a>(&'a self, cid: &str, mixes: &'a [Value], accent: Color, compact: bool, show_link: bool) -> Element<'a, Message> {
        let mut sliders = row![].spacing(if compact { 8 } else { 30 });
        let mut toggles = row![].spacing(if compact { 0 } else { 2 }).align_y(Alignment::Center);
        let (glyph, pad) = if compact { (18.0, 4) } else { (22.0, 7) };
        for (i, m) in mixes.iter().enumerate() {
            let t = Target::Route { mix: m["id"].as_str().unwrap_or("").to_string(), channel: cid.to_string() };
            let (vol, mute) = self.level(&t);
            let tt = t.clone();
            sliders = sliders.push(
                column![
                    text(db_label(vol)).size(11).color(if mute { DIM } else { TEXT }).wrapping(iced::widget::text::Wrapping::None),
                    vertical_slider(-60.0..=0.0, vol, move |v| Message::Slide(tt.clone(), v))
                        .step(1.0_f32)
                        .on_release(Message::Release)
                        .height(Length::Fill)
                        .width(28)
                        .style(fader_style(accent, mute)),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            );
            if i == 1 && show_link {
                let linked = self.linked(cid);
                let b = button(icon(icons::LINK, if compact { 13.0 } else { 15.0 }, if linked { ON } else { DIM }))
                    .padding(pad)
                    .style(flat_button(linked, ON))
                    .on_press(Message::Link(cid.to_string(), !linked));
                let hint = if linked { "Faders move together. Click to unlink." } else { "Click to move both faders together." };
                toggles = toggles.push(iced::widget::tooltip(b, tooltip_box(hint.into()), iced::widget::tooltip::Position::Top).gap(6));
            }
            let hint = Self::mix_hint_for(self, cid, m, mute);
            toggles = toggles.push(mute_toggle_sized(Self::mix_glyph(m), mute, hint, Message::Mute(t, !mute), glyph, pad));
        }
        column![container(sliders.height(Length::Fill)).center_x(Length::Fill), container(toggles).center_x(Length::Fill)]
            .spacing(10)
            .height(Length::Fill)
            .into()
    }

    /// Every app (running first, then installed), filtered by the search
    /// box. Tap one to put it in this channel; tap a checked one to take it
    /// out (an automatic app then stays out until it's put back).
    fn app_picker<'a>(&'a self, cid: &str) -> Element<'a, Message> {
        let q = self.search.to_lowercase();
        let auto = self.is_auto(cid);
        let label = self.channel_label(cid);
        let color = channel_color(cid);
        let mut here_list = column![].spacing(2);
        let mut rest_list = column![].spacing(2);
        let (mut n_here, mut n_rest) = (0, 0);
        for a in &self.apps {
            let name = a["name"].as_str().unwrap_or("?").to_string();
            if !q.is_empty() && !name.to_lowercase().contains(&q) {
                continue;
            }
            let here = a["channel"] == cid;
            // Taken out of this channel by hand: listed here, switched off.
            let off = a["excluded_from"] == cid;
            // A manual channel (Other) only takes apps that aren't placed anywhere yet.
            if !auto && !here && a["channel"].is_string() {
                continue;
            }
            if !here && !off && n_rest >= 120 {
                continue;
            }
            let explicit = a["explicit"] == true;
            let pic: Element<'a, Message> = match self.icon_of(a) {
                Some(i) => app_icon(&i, 22.0),
                None => container(text(name.chars().next().unwrap_or('?').to_uppercase().to_string()).size(12).font(SEMIBOLD).color(DIM))
                    .center(22)
                    .into(),
            };
            let mut line = row![pic, text(name.clone()).size(13).width(Length::Fill).wrapping(iced::widget::text::Wrapping::None)]
                .spacing(10)
                .align_y(Alignment::Center);
            if a["playing"] == true {
                line = line.push(text("Playing").size(11).color(ON));
            }
            if here || off {
                // In this channel: a switch, so taking an app out is obvious.
                line = line.push(text(if explicit { "Added" } else { "Automatic" }).size(11).color(DIM));
                let on = here;
                let (n, c) = (name.clone(), cid.to_string());
                line = line.push(toggler(on).size(18).style(switch_style).on_toggle(move |v| Message::Assign(n.clone(), v.then(|| c.clone()))));
                let hint = if here { "Turn off to keep this app out of this channel" } else { "Turn on to put this app back in this channel" };
                let row_el = container(line).width(Length::Fill).padding([7, 10]);
                here_list = here_list.push(iced::widget::tooltip(row_el, tooltip_box(hint.into()), iced::widget::tooltip::Position::Top).gap(6));
                n_here += 1;
                continue;
            }
            if let Some(c) = a["channel"].as_str() {
                let other = channel_color(c);
                line = line.push(
                    container(text(self.channel_label(c)).size(11).color(other))
                        .padding([2, 8])
                        .style(move |_| container::Style { background: Some(Color { a: 0.14, ..other }.into()), border: border::rounded(8), ..Default::default() }),
                );
            }
            let b = button(line).width(Length::Fill).padding([7, 10]).style(flat_button(false, ON)).on_press(Message::Assign(name, Some(cid.to_string())));
            let b = iced::widget::tooltip(b, tooltip_box("Click to add to this channel".into()), iced::widget::tooltip::Position::Top).gap(6);
            n_rest += 1;
            rest_list = rest_list.push(b);
        }
        let mut list = column![].spacing(6);
        if n_here > 0 {
            list = list
                .push(heading("IN THIS CHANNEL"))
                .push(text("Switch an app off to keep it out of this channel.").size(11).color(DIM))
                .push(here_list).push(Space::new().height(6));
        }
        list = list.push(heading("ALL APPS")).push(rest_list);
        if n_here + n_rest == 0 {
            list = list.push(text("No matching apps.").size(12).color(DIM));
        }
        let badge = container(icon(icons::channel_glyph(cid), 16.0, Color::WHITE)).center(30).style(move |_| container::Style {
            background: Some(color.into()),
            border: border::rounded(9),
            ..Default::default()
        });
        let body = column![
            row![
                badge,
                column![
                    text(format!("Apps in {label}")).size(18).font(SEMIBOLD),
                    text(if auto {
                        "Apps of this type come here by themselves. Tap any app to move it here."
                    } else {
                        "Only the apps you add come here. Apps in another channel aren't listed."
                    })
                    .size(12)
                    .color(DIM),
                ]
                .spacing(2)
                .width(Length::Fill),
                button(text("Done").size(13).font(SEMIBOLD)).padding([7, 16]).style(soft_button).on_press(Message::Toggle(cid.to_string())),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            text_input("Search apps", &self.search).on_input(Message::Search).size(13).padding([8, 12]).style(search_style),
            scrollable(list.padding(iced::Padding::ZERO.right(12))).height(Length::Fill),
        ]
        .spacing(16);
        modal(sheet(body.padding(24)).width(560).height(620).into(), Message::Toggle(cid.to_string()))
    }

    /// Where the knob's LED ring sits (0..1): it shows whatever the knob controls.
    fn ring_level(&self) -> f32 {
        match self.mic["knob"].as_str() {
            Some("headphone") => (self.level(&Target::MicParam("headphone_volume")).0 + 60.0) / 60.0,
            Some("mix") => self.level(&Target::MicParam("monitor")).0 / 100.0,
            _ => self.level(&Target::MicGain).0 / GAIN_MAX,
        }
    }

    fn mic_art<'a>(&'a self) -> Element<'a, Message> {
        let knob = match self.mic["knob"].as_str() {
            Some("headphone") => mic_art::Knob::Headphone,
            Some("mix") => mic_art::Knob::Mix,
            _ => mic_art::Knob::Gain,
        };
        canvas(mic_art::MicArt { level: self.ring_level(), knob, muted: self.mic["mute"] == true })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Right-hand panel: a drawing of the mic with its LED ring; opens the
    /// Wave:3 sheet. Recessed (well colour, hairline) so it reads as a
    /// different kind of thing than the raised channel strips.
    fn device_panel<'a>(&'a self) -> Element<'a, Message> {
        let connected = self.mic["connected"] == true;
        let body: Element<'a, Message> = if connected {
            let muted = self.mic["mute"] == true;
            column![
                self.mic_art(),
                column![text("Wave:3").size(15).font(SEMIBOLD), status_pill(muted)].spacing(6).align_x(Alignment::Center),
                row![
                    button(
                        container(
                            row![
                                icon(icons::MIC, 16.0, if muted { Color::WHITE } else { TEXT }),
                                text(if muted { "Unmute" } else { "Mute" }).size(14).font(SEMIBOLD)
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        )
                        .center_x(Length::Fill),
                    )
                    .width(Length::Fill)
                    .padding([10, 0])
                    .style(move |t, st| mute_style(muted, t, st))
                    .on_press(Message::Mute(Target::MicGain, !muted)),
                    iced::widget::tooltip(
                        button(icon(icons::GEAR, 18.0, TEXT)).padding(10).style(soft_button).on_press(Message::ShowDevice(true)),
                        tooltip_box("Mic settings".into()),
                        iced::widget::tooltip::Position::Top,
                    )
                    .gap(6),
                ]
                .spacing(8),
            ]
            .spacing(14)
            .into()
        } else {
            container(
                column![
                    icon(icons::PLUG, 34.0, DIM),
                    text("Plug in your Wave:3").size(14).font(SEMIBOLD).center(),
                    text("This app is made for the Elgato Wave:3.").size(11).color(DIM).center(),
                ]
                .spacing(10)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        };
        container(column![heading("DEVICE"), body].spacing(12).padding(14))
            .width(196)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(WELL.into()),
                border: iced::Border { color: HAIRLINE, width: 1.0, radius: 14.0.into() },
                ..Default::default()
            })
            .into()
    }

    /// Narrow-window version of the Device sidebar: one bar under the strips.
    fn device_bar<'a>(&'a self) -> Element<'a, Message> {
        let connected = self.mic["connected"] == true;
        let muted = self.mic["mute"] == true;
        let badge = container(icon(icons::MIC, 16.0, if muted { MUTED } else { TEXT })).center(34).style(|_| container::Style {
            background: Some(RAIL.into()),
            border: border::rounded(10),
            ..Default::default()
        });
        let content: Element<'a, Message> = if connected {
            row![
                badge,
                text("Wave:3").size(14).font(SEMIBOLD),
                status_pill(muted),
                Space::new().width(Length::Fill),
                button(
                    row![
                        icon(icons::MIC, 15.0, if muted { Color::WHITE } else { TEXT }),
                        text(if muted { "Unmute" } else { "Mute" }).size(13).font(SEMIBOLD)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding([8, 16])
                .style(move |t, st| mute_style(muted, t, st))
                .on_press(Message::Mute(Target::MicGain, !muted)),
                iced::widget::tooltip(
                    button(icon(icons::GEAR, 16.0, TEXT)).padding(9).style(soft_button).on_press(Message::ShowDevice(true)),
                    tooltip_box("Mic settings".into()),
                    iced::widget::tooltip::Position::Top,
                )
                .gap(6),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                container(icon(icons::PLUG, 16.0, DIM)).center(34),
                column![
                    text("Plug in your Wave:3").size(13).font(SEMIBOLD),
                    text("This app is made for the Elgato Wave:3.").size(11).color(DIM),
                ]
                .spacing(2),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
        };
        card(container(content).padding([8, 14])).width(Length::Fill).into()
    }

    /// The Wave:3 sheet: drawing, mute and level on the left; input &
    /// monitoring controls or device info on the right.
    fn device_sheet<'a>(&'a self) -> Element<'a, Message> {
        let close = button(text("Done").size(13).font(SEMIBOLD)).padding([7, 16]).style(soft_button).on_press(Message::ShowDevice(false));
        if self.mic["connected"] != true {
            let body = column![
                row![text("Wave:3").size(22).font(SEMIBOLD), Space::new().width(Length::Fill), close].align_y(Alignment::Center),
                text("Plug in your Wave:3 to see its settings.").size(13).color(DIM),
            ]
            .spacing(16);
            return modal(sheet(body.padding(24)).width(460).into(), Message::ShowDevice(false));
        }
        let muted = self.mic["mute"] == true;
        let mute_btn = button(
            container(
                row![
                    icon(icons::MIC, 18.0, if muted { Color::WHITE } else { TEXT }),
                    text(if muted { "Unmute" } else { "Mute" }).size(14).font(SEMIBOLD)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .center_x(Length::Fill),
        )
        .width(Length::Fill)
        .padding(11)
        .style(move |t, st| mute_style(muted, t, st))
        .on_press(Message::Mute(Target::MicGain, !muted));
        let left = column![
            text("Wave:3").size(28).font(SEMIBOLD),
            text(self.mic["serial"].as_str().unwrap_or("").to_string()).size(12).color(DIM),
            container(self.mic_art()).height(Length::Fill),
            mute_btn,
            column![heading("INPUT LEVEL"), hbar(self.peak("channel", "mic").0, 6.0)].spacing(6),
        ]
        .spacing(10)
        .width(300);

        let content: Element<'a, Message> = if self.device_tab == 0 { self.mic_controls() } else { self.mic_info() };
        let right = column![
            row![segmented(&[("Input & monitoring", 0), ("Device info", 1)], self.device_tab, Message::DeviceTab), Space::new().width(Length::Fill), close]
                .align_y(Alignment::Center),
            content
        ]
        .spacing(18)
        .width(Length::Fill);

        let body = row![left, right].spacing(32).height(Length::Fill);
        modal(sheet(body.padding(28)).width(880).height(620).into(), Message::ShowDevice(false))
    }

    fn mic_slider<'a>(&'a self, label: &'static str, t: Target, range: std::ops::RangeInclusive<f32>, step: f32, unit: &'static str) -> Element<'a, Message> {
        let (v, muted) = self.level(&t);
        let tt = t.clone();
        column![
            row![text(label).size(13).width(Length::Fill), text(format!("{v:.1}{unit}")).size(13).color(DIM)],
            slider(range, v, move |x| Message::Slide(tt.clone(), x))
                .step(step)
                .on_release(Message::Release)
                .style(fader_style(ACCENT, muted && t != Target::MicGain)),
        ]
        .spacing(10)
        .into()
    }

    fn mic_controls<'a>(&'a self) -> Element<'a, Message> {
        let hp_muted = self.mic["headphone_mute"] == true;
        let knob = self.mic["knob"].as_str().unwrap_or("mic");
        let knob_idx = match knob {
            "headphone" => 1,
            "mix" => 2,
            _ => 0,
        };
        scrollable(
            column![
                heading("INPUT"),
                group(column![
                    self.mic_slider("Mic gain", Target::MicGain, 0.0..=GAIN_MAX, 0.5, " dB"),
                    separator(),
                    switch_row("Low cut filter", self.mic["lowcut"] == true, |on| Message::MicFlag("lowcut", on)),
                    separator(),
                    switch_row("Clipguard", self.mic["clipguard"] == true, |on| Message::MicFlag("clipguard", on)),
                ]),
                heading("HEADPHONES ON THE MIC"),
                group(column![
                    row![
                        container(self.mic_slider("Volume", Target::MicParam("headphone_volume"), -60.0..=0.0, 0.5, " dB"))
                            .width(Length::Fill),
                        mute_toggle(
                            icons::HEADPHONES,
                            hp_muted,
                            if hp_muted { "Headphones muted. Click to unmute.".into() } else { "Click to mute the headphones.".into() },
                            Message::Mute(Target::MicParam("headphone_volume"), !hp_muted),
                        ),
                    ]
                    .spacing(12)
                    .align_y(Alignment::End),
                    separator(),
                    self.mic_slider("Monitor mix (computer ↔ mic)", Target::MicParam("monitor"), 0.0..=100.0, 5.0, " % mic"),
                ]),
                heading("THE KNOB CONTROLS"),
                segmented(&[("Mic gain", 0), ("Headphones", 1), ("Monitor mix", 2)], knob_idx, |i| {
                    Message::Knob(["mic", "headphone", "mix"][i as usize])
                }),
            ]
            .spacing(10)
            .padding(iced::Padding::ZERO.right(12)),
        )
        .height(Length::Fill)
        .into()
    }

    fn mic_info<'a>(&'a self) -> Element<'a, Message> {
        let info = &self.mic["info"];
        let get = |k: &str| info[k].as_str().unwrap_or("?").to_string();
        column![
            group(column![
                info_row("Serial number", self.mic["serial"].as_str().unwrap_or("?").to_string()),
                separator(),
                info_row("Firmware", get("firmware")),
                self.firmware_check(&get("firmware")),
                separator(),
                info_row("Hardware", get("hardware")),
                separator(),
                info_row("Bootloader", get("bootloader")),
                separator(),
                info_row("API", get("api")),
            ]),
            text("Installing firmware from Linux isn't supported yet. Update the firmware with Elgato Wave Link on a Mac or Windows PC.")
                .size(11)
                .color(DIM),
        ]
        .spacing(12)
        .into()
    }

    /// "Check for updates" button and result, under the Firmware row.
    fn firmware_check<'a>(&self, installed: &str) -> Element<'a, Message> {
        let checking = matches!(self.fw, FwCheck::Checking);
        let known = installed != "?";
        let status: Option<(String, Color)> = match &self.fw {
            FwCheck::Idle => None,
            FwCheck::Checking => Some(("Checking Elgato's catalog…".into(), DIM)),
            FwCheck::Done(Err(e)) => Some((format!("Couldn't check: {e}"), MUTED)),
            FwCheck::Done(Ok(f)) if !known => Some((format!("Latest firmware: {}.", f.version), DIM)),
            FwCheck::Done(Ok(f)) if wave_catalog::is_newer(&f.version, installed) => {
                Some((format!("Firmware {} is available.", f.version), ACCENT))
            }
            FwCheck::Done(Ok(f)) => Some((format!("Up to date ({} is the latest).", f.version), ON)),
        };
        let mut r = row![button(text("Check for updates").size(12))
            .padding([5, 12])
            .style(soft_button)
            .on_press_maybe((!checking).then_some(Message::CheckFirmware))]
        .spacing(10)
        .align_y(Alignment::Center);
        if let Some((msg, color)) = status {
            r = r.push(text(msg).size(12).color(color));
        }
        r.into()
    }

    fn settings_view<'a>(&'a self) -> Element<'a, Message> {
        let version = format!(
            "{} ({})",
            env!("CARGO_PKG_VERSION"),
            if settings::BUILD_COMMIT.is_empty() { "unknown build" } else { &settings::BUILD_COMMIT[..7.min(settings::BUILD_COMMIT.len())] }
        );
        let state = self.update.clone().unwrap_or(UpdateState::Idle);
        let (status, can_update): (String, bool) = match &state {
            UpdateState::Idle => (String::new(), false),
            UpdateState::Busy(s) => (s.to_string(), false),
            UpdateState::UpToDate => ("You're up to date.".into(), false),
            UpdateState::Available(n) => (format!("{n} update{} available.", if *n == 1 { "" } else { "s" }), true),
            UpdateState::Updated => ("Updated. Close and reopen the app to use the new version.".into(), false),
            UpdateState::Failed(e) => (format!("Couldn't check: {e}"), false),
        };
        let busy = matches!(state, UpdateState::Busy(_));
        let mut actions = row![button(text("Check for updates").size(13))
            .padding([7, 14])
            .style(soft_button)
            .on_press_maybe((!busy).then_some(Message::CheckUpdates))]
        .spacing(8)
        .align_y(Alignment::Center);
        if can_update {
            actions = actions.push(
                button(text("Update now").size(13).font(SEMIBOLD))
                    .padding([7, 14])
                    .style(primary_button)
                    .on_press_maybe((!busy).then_some(Message::UpdateNow)),
            );
        }
        let auto = self.prefs.as_ref().is_some_and(|p| p.auto_check);
        let fallback = self.config["output_fallback"] != false;
        let mut updates = column![info_row("Version", version), separator(), actions].spacing(12);
        if !status.is_empty() {
            updates = updates.push(text(status).size(12).color(DIM));
        }
        updates = updates.push(separator()).push(switch_row("Check for updates automatically", auto, Message::AutoCheck));
        let body = column![
            row![
                text("Preferences").size(20).font(SEMIBOLD),
                Space::new().width(Length::Fill),
                button(text("Done").size(13).font(SEMIBOLD)).padding([7, 16]).style(soft_button).on_press(Message::ShowSettings(false))
            ]
            .align_y(Alignment::Center),
            heading("UPDATES"),
            group(updates),
            heading("GENERAL"),
            group(column![
                switch_row("Open at login", self.autostart, Message::Autostart),
                separator(),
                switch_row("Use the default output when the chosen one is unplugged", fallback, Message::OutputFallback),
            ]),
        ]
        .spacing(10);
        modal(sheet(body.padding(24)).width(500).into(), Message::ShowSettings(false))
    }

    fn output_row<'a>(&'a self, m: &Value, compact: bool) -> Element<'a, Message> {
        let mid = m["id"].as_str().unwrap_or("").to_string();
        let label = m["label"].as_str().unwrap_or("?").to_string();
        let t = Target::Mix(mid.clone());
        let (vol, mute) = self.level(&t);
        let monitor = m["kind"] == "monitor";

        let device: Element<'a, Message> = if monitor {
            let current = m["output"].as_str().unwrap_or("");
            let selected = self.outputs.iter().find(|o| o.name == current).cloned();
            let mid2 = mid.clone();
            pick_list(self.outputs.clone(), selected, move |o| Message::SetOutput(mid2.clone(), o))
                .placeholder("Default output")
                .text_size(13)
                .padding([6, 10])
                .width(Length::Fill)
                .style(picker_style)
                .into()
        } else if compact {
            hmeter(self.peak("mix", &mid))
        } else {
            text("Pick “Pipemic Outgoing” as the mic in calls and streams").size(12).color(DIM).into()
        };
        let tt = t.clone();
        let vol_slider = slider(-60.0..=0.0, vol, move |v| Message::Slide(tt.clone(), v))
            .step(1.0_f32)
            .on_release(Message::Release)
            .width(if compact { Length::FillPortion(3) } else { Length::Fixed(200.0) })
            .style(fader_style(ACCENT, mute));
        let title = column![
            text(label).size(14).font(SEMIBOLD),
            text(if monitor { "What you hear" } else { "What others hear" }).size(11).color(DIM),
        ]
        .spacing(2);
        // In a narrow window the Outgoing hint moves into the title's tooltip.
        let title: Element<'a, Message> = if compact && !monitor {
            iced::widget::tooltip(title, tooltip_box("Pick “Pipemic Outgoing” as the mic in calls and streams".into()), iced::widget::tooltip::Position::Top)
                .gap(6)
                .into()
        } else {
            title.into()
        };

        card(
            row![
                container(icon(if monitor { icons::HEADPHONES } else { icons::OUTGOING }, 18.0, TEXT)).center(34).style(|_| container::Style {
                    background: Some(RAIL.into()),
                    border: border::rounded(10),
                    ..Default::default()
                }),
                container(title).width(if compact { 120 } else { 130 }),
                container(device).width(Length::FillPortion(3)),
            ]
            .push((!compact).then(|| container(hmeter(self.peak("mix", &mid))).width(Length::FillPortion(2))))
            .extend([
                mute_toggle(
                    icons::SPEAKER,
                    mute,
                    if mute { "Muted. Click to unmute.".into() } else { "Click to mute this output.".into() },
                    Message::Mute(t, !mute),
                ),
                vol_slider.into(),
                text(db_label(vol)).size(12).color(DIM).width(44).into(),
            ])
            .spacing(if compact { 12 } else { 16 })
            .align_y(Alignment::Center)
            .padding([10, 14]),
        )
        .width(Length::Fill)
        .into()
    }

    fn view(&self) -> Element<'_, Message> {
        let status: Element<'_, Message> = match &self.error {
            Some(e) => row![
                container(Space::new()).width(8).height(8).style(|_| container::Style {
                    background: Some(MUTED.into()),
                    border: border::rounded(4),
                    ..Default::default()
                }),
                text(e.clone()).size(12).color(DIM)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
            None => Space::new().into(),
        };
        let channels: &[Value] = self.config["channel"].as_array().map_or(&[], |v| v.as_slice());
        let mixes: &[Value] = self.config["mix"].as_array().map_or(&[], |v| v.as_slice());

        let gear = iced::widget::tooltip(
            button(icon(icons::GEAR, 18.0, DIM)).padding(7).style(flat_button(false, TEXT)).on_press(Message::ShowSettings(true)),
            tooltip_box("Preferences".into()),
            iced::widget::tooltip::Position::Left,
        );

        // Lay out for the space we actually have: the Device sidebar folds
        // into a bar and strips/outputs tighten as the window narrows.
        let body = iced::widget::responsive(move |size| {
            let w = size.width;
            let sidebar = w >= SIDEBAR_MIN;
            let n = channels.len().max(1) as f32;
            let strips_w = if sidebar { w - 196.0 - 12.0 } else { w };
            let strip_w = (strips_w - 12.0 * (n - 1.0)) / n;

            let mut strips = row![].spacing(12).height(Length::Fill);
            let label_w = channels
                .iter()
                .map(|c| est_text_width(c["label"].as_str().unwrap_or(""), 14.0))
                .fold(0.0, f32::max);
            for c in channels {
                strips = strips.push(self.strip(c, mixes, strip_w, label_w));
            }
            let inputs: Element<'_, Message> = if sidebar {
                row![strips, self.device_panel()].spacing(12).height(Length::Fill).into()
            } else {
                column![strips, self.device_bar()].spacing(12).height(Length::Fill).into()
            };

            let compact = w < COMPACT_OUTPUTS;
            let mut outputs = column![].spacing(10);
            for m in mixes {
                outputs = outputs.push(self.output_row(m, compact));
            }
            column![inputs, heading("OUTPUTS"), outputs].spacing(12).into()
        });

        let main = container(
            column![
                row![heading("INPUTS"), Space::new().width(Length::Fill), status, gear].spacing(12).align_y(Alignment::Center),
                body,
            ]
            .spacing(12),
        )
        .padding([16, 20])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::background(BG));
        if self.show_settings {
            stack![main, self.settings_view()].into()
        } else if self.show_device {
            stack![main, self.device_sheet()].into()
        } else if let Some(cid) = self.open.as_deref() {
            stack![main, self.app_picker(cid)].into()
        } else {
            main.into()
        }
    }
}
