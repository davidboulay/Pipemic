//! Pipemic mixer window. Talks to pipemicd over its control socket
//! (`$XDG_RUNTIME_DIR/pipemicd.sock`, JSON lines). Polls status 4×/s so
//! changes made elsewhere (pipemicctl, the mic's touch pad) show up, the app
//! list every 2 s, and level meters ~30×/s.
//!
//! Layout: INPUTS = one strip per channel (header, meter, app box, one
//! fader per mix with its mute, link toggle); OUTPUTS = one row per mix
//! (output device, meter, mute, volume).

mod addons;
mod icons;
mod mic_art;
mod settings;
mod tray;

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
    // Already running (window closed, icon in the tray)? Show that one.
    if !tray::claim_single_instance(link_arg().as_deref()) {
        return Ok(());
    }
    // The mixer runs while Pipemic does: start it (Quit stops it).
    daemon_service("start");
    // A daemon, not a single-window app: closing the window leaves Pipemic
    // in the tray; the tray (or launching it again) reopens the window.
    iced::daemon(App::new, App::update, App::view)
        .title("Pipemic")
        .subscription(App::subscription)
        .theme(theme())
        .run()
}

/// `systemctl --user <action> pipemicd.service`, waiting for it to finish.
/// Stopping pipemicd removes its devices and sends apps back to the
/// default speakers and mic; starting it brings the saved setup back.
fn daemon_service(action: &str) {
    let _ = std::process::Command::new("systemctl").args(["--user", action, "pipemicd.service"]).status();
}

/// A marketplace link passed on the command line (`pipemic %u`).
fn link_arg() -> Option<String> {
    std::env::args().skip(1).find(|a| a.starts_with("wavelink://") || a.starts_with("wavelink-v2://"))
}

fn window_settings() -> iced::window::Settings {
    iced::window::Settings {
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
    }
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
    Tray(tray::Out),
    WindowClosed(iced::window::Id),
    /// Open (Some(channel)) or close the effects sheet.
    ShowFx(Option<String>),
    AddonsDone(Result<Vec<wave_catalog::marketplace::Addon>, String>),
    Downloads(Vec<addons::LocalPreset>),
    /// Put an EQ preset file on a channel.
    /// (channel, file, name to show)
    EqImport(String, std::path::PathBuf, Option<String>),
    EqEnabled(String, bool),
    EqDone(Result<Value, String>),
    /// Install an add-on (signed in), else open its page and apply its file
    /// once it's downloaded.
    Get { name: String, url: String, variant: Option<String> },
    /// Stop waiting for an add-on to arrive in Downloads.
    CancelWait,
    /// A signed-in download finished; `channel` = apply it there.
    Installed {
        variant: String,
        channel: Option<String>,
        url: Option<String>,
        result: Result<(String, std::path::PathBuf), wave_catalog::marketplace::InstallError>,
    },
    IncomingInfo(String, Result<(String, Option<String>), String>),
    SignIn,
    SignedIn(Result<bool, String>),
    SignOut,
    ThumbsDone(Vec<(String, Vec<u8>)>),
    /// Move every file of this preset (library and Downloads) to the trash.
    DeleteLocal(String),
    RefreshAddons,
    ToastDone(u64),
    FileDropped(std::path::PathBuf),
    /// "Open with Wave Link" on the marketplace site.
    MarketLink(String),
    /// Close the "add-on added" popup.
    IncomingOk,
    /// The mixer window gained / lost focus.
    Focus(bool),
}

/// An add-on opened from the marketplace site: waiting for its file, then
/// added to the preset library.
#[derive(Debug, Clone)]
struct Incoming {
    /// Variant id from the link.
    variant: String,
    /// Name and thumbnail key, once looked up.
    name: Option<String>,
    state: IncomingState,
}

#[derive(Debug, Clone, PartialEq)]
enum IncomingState {
    /// Downloading through the signed-in session.
    Downloading,
    /// Waiting for the user's own download (not signed in, or the website
    /// wouldn't hand the file to the session).
    Waiting,
    Added(String),
    Failed(String),
}

/// An add-on the user went to download: applied to `channel` when its
/// file shows up in Downloads.
#[derive(Debug, Clone)]
struct Pending {
    channel: String,
    name: String,
    since: std::time::SystemTime,
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
    /// The mixer window, if open.
    window: Option<iced::window::Id>,
    /// Tray icon, once registered. Without one, closing the window quits.
    tray: Option<tray::TrayHandle>,
    /// What the tray shows (connected, muted), to update it only on change.
    tray_state: Option<(bool, bool)>,
    /// Channel whose effects sheet is open.
    fx: Option<String>,
    /// Marketplace add-ons (fetched when the effects sheet first opens).
    addons: AddonList,
    /// EQ presets found in the Downloads folder.
    downloads: Vec<addons::LocalPreset>,
    /// Last effects error, shown in the sheet.
    fx_error: Option<String>,
    /// When the marketplace list was last fetched.
    addons_at: Option<std::time::Instant>,
    /// Add-on id -> thumbnail.
    thumbs: HashMap<String, image::Handle>,
    pending: Option<Pending>,
    /// Short confirmation ("Old Radio added to Other") and its id.
    toast: Option<(u64, String)>,
    incoming: Option<Incoming>,
    /// The mixer window has keyboard focus.
    focused: bool,
    /// Signed in to the Elgato marketplace (pipemic-login session).
    signed_in: bool,
    /// The sign-in window is open.
    signing_in: bool,
}

#[derive(Debug, Clone, Default)]
enum AddonList {
    #[default]
    Idle,
    Loading,
    Done(Result<Vec<wave_catalog::marketplace::Addon>, String>),
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
        if let Some(l) = link_arg() {
            tasks.push(Task::done(Message::MarketLink(l)));
        }
        if let Some(c) = view.strip_prefix("fx:") {
            tasks.push(Task::done(Message::ShowFx(Some(c.to_string()))));
        }
        let app = Self {
            show_device: view == "device" || view == "device-info",
            device_tab: (view == "device-info") as u8,
            show_settings: view == "settings",
            open: view.strip_prefix("picker:").map(String::from), signed_in: addons::signed_in(), prefs: Some(prefs), autostart: settings::autostart(), update: Some(UpdateState::Idle), ..Self::default() };
        let mut app = app;
        if app.autostart {
            // Refresh older entries (Exec=pipemic) so login starts in the tray.
            let _ = settings::set_autostart(true);
        }
        // `--background` (used by autostart) starts with just the tray icon.
        if !std::env::args().any(|a| a == "--background") {
            let (id, open) = iced::window::open(window_settings());
            app.window = Some(id);
            tasks.push(open.discard());
        }
        (app, Task::batch(tasks))
    }

    fn apps_request(&self) -> Task<Message> {
        // With installed apps: the strips show every app a channel holds.
        Task::perform(send(json!({"cmd": "apps", "installed": true})), Message::Apps)
    }

    fn subscription(&self) -> Subscription<Message> {
        let background = Subscription::batch([
            Subscription::run(tray::run).map(Message::Tray),
            iced::window::close_events().map(Message::WindowClosed),
            iced::event::listen_with(|e, _, _| match e {
                iced::Event::Window(iced::window::Event::FileDropped(p)) => Some(Message::FileDropped(p)),
                iced::Event::Window(iced::window::Event::Focused) => Some(Message::Focus(true)),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Focus(false)),
                _ => None,
            }),
        ]);
        if self.window.is_none() {
            // Hidden: just keep the tray's mute state current.
            return Subscription::batch([background, time::every(Duration::from_secs(1)).map(|_| Message::Tick)]);
        }
        Subscription::batch([
            background,
            time::every(Duration::from_millis(250)).map(|_| Message::Tick),
            time::every(Duration::from_secs(2)).map(|_| Message::AppsTick),
            time::every(Duration::from_millis(33)).map(|_| Message::MeterTick),
            // New and updated marketplace add-ons.
            time::every(Duration::from_secs(3600)).map(|_| Message::RefreshAddons),
        ])
    }

    /// Push mic state to the tray icon when it changes.
    fn sync_tray(&mut self) {
        let state = (self.mic["connected"] == true, self.mic["mute"] == true);
        if let Some(t) = &self.tray {
            if self.tray_state != Some(state) {
                self.tray_state = Some(state);
                t.set(state.0, state.1);
            }
        }
    }

    /// Put the window in front of the browser. On Wayland an app can't
    /// raise its own window (winit's focus request is a no-op), but a new
    /// window is shown on top: so reopen it unless it already has focus.
    fn bring_forward(&mut self) -> Task<Message> {
        match self.window {
            Some(id) if !self.focused => {
                self.window = None;
                Task::batch([iced::window::close(id), self.show_window()])
            }
            _ => self.show_window(),
        }
    }

    fn show_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window {
            return iced::window::gain_focus(id);
        }
        let (id, open) = iced::window::open(window_settings());
        self.window = Some(id);
        // Fresh state on reopen so it's current straight away.
        Task::batch([
            open.discard(),
            Task::perform(send(json!({"cmd": "status"})), Message::Status),
            self.apps_request(),
        ])
    }

    fn take_reply(&mut self, v: &Value) {
        if !v["config"].is_null() {
            self.config = v["config"].clone();
        }
        if !v["mic"].is_null() {
            self.mic = v["mic"].clone();
            self.sync_tray();
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
            Message::AppsTick => {
                let mut t = vec![self.apps_request()];
                if self.fx.is_some() || self.pending.is_some() || self.waiting_link() {
                    t.push(Task::perform(blocking(addons::scan_downloads), Message::Downloads));
                }
                return Task::batch(t);
            }
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
            Message::Tray(tray::Out::Ready(h)) => {
                self.tray = Some(h);
                self.tray_state = None;
                self.sync_tray();
            }
            Message::Tray(tray::Out::Event(e)) => match e {
                tray::Event::Show => return self.show_window(),
                tray::Event::Open(url) => return self.update(Message::MarketLink(url)),
                tray::Event::ToggleMute => {
                    let muted = self.mic["mute"] == true;
                    return self.update(Message::Mute(Target::MicGain, !muted));
                }
                tray::Event::Quit => {
                    // Quit means off: back to the plain default devices.
                    daemon_service("stop");
                    return iced::exit();
                }
            },
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                    self.dragging.clear();
                    if self.tray.is_none() {
                        daemon_service("stop");
                        return iced::exit(); // nowhere to come back from
                    }
                }
            }
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
            Message::ShowFx(c) => {
                self.fx_error = None;
                self.fx = c;
                if self.fx.is_none() {
                    return Task::none();
                }
                let stale = self.addons_at.is_none_or(|t| t.elapsed() > Duration::from_secs(1800));
                let mut t = vec![Task::perform(blocking(addons::scan_downloads), Message::Downloads)];
                if stale || matches!(self.addons, AddonList::Done(Err(_))) {
                    t.push(self.update(Message::RefreshAddons));
                }
                return Task::batch(t);
            }
            Message::RefreshAddons => {
                if matches!(self.addons, AddonList::Loading) {
                    return Task::none();
                }
                // Keep showing the old list while refreshing.
                if !matches!(self.addons, AddonList::Done(Ok(_))) {
                    self.addons = AddonList::Loading;
                }
                return Task::perform(blocking(wave_catalog::marketplace::wave_link_addons), Message::AddonsDone);
            }
            Message::AddonsDone(r) => {
                let mut t = Task::none();
                match r {
                    Ok(list) => {
                        self.addons_at = Some(std::time::Instant::now());
                        let want: Vec<(String, String)> = list
                            .iter()
                            .filter(|a| a.runs_on_linux() && !self.thumbs.contains_key(&a.id))
                            .filter_map(|a| Some((a.id.clone(), a.thumbnail.clone()?)))
                            .collect();
                        if !want.is_empty() {
                            t = Task::perform(blocking(move || addons::thumbnails(want)), Message::ThumbsDone);
                        }
                        self.addons = AddonList::Done(Ok(list));
                        self.resolve_incoming();
                    }
                    // A failed refresh keeps the list we have.
                    Err(e) => {
                        if !matches!(self.addons, AddonList::Done(Ok(_))) {
                            self.addons = AddonList::Done(Err(e));
                        }
                    }
                }
                return t;
            }
            Message::ThumbsDone(v) => {
                for (id, bytes) in v {
                    self.thumbs.insert(id, image::Handle::from_bytes(bytes));
                }
            }
            Message::Downloads(d) => {
                self.downloads = d;
                self.resolve_incoming();
                // The add-on the user went to get has arrived: apply it.
                if let Some(p) = self.pending.clone() {
                    let fresh = self.downloads.iter().find(|l| l.is(&p.name) && l.modified + Duration::from_secs(5) >= p.since);
                    if let Some(l) = fresh {
                        self.pending = None;
                        let (c, path) = (p.channel.clone(), addons::add_to_library(&l.path).unwrap_or_else(|_| l.path.clone()));
                        let msg = format!("{} added to {}", l.name, self.channel_label(&p.channel));
                        return Task::perform(send(json!({"cmd": "eq", "channel": c, "import": path, "name": p.name})), Message::EqDone)
                            .chain(self.toast(msg));
                    } else if p.since.elapsed().is_ok_and(|e| e > Duration::from_secs(900)) {
                        self.pending = None; // gave up waiting
                    }
                }
            }
            Message::Get { name, url, variant } => {
                if let (true, Some(v)) = (self.signed_in, variant) {
                    let channel = self.fx.clone();
                    self.fx_error = None;
                    self.toast_now(format!("Installing {name}…"));
                    return Task::perform(blocking(move || addons::install_variant(&v)), move |result| Message::Installed {
                        variant: String::new(),
                        channel: channel.clone(),
                        url: Some(url.clone()),
                        result,
                    });
                }
                if let Some(c) = self.fx.clone() {
                    self.pending = Some(Pending { channel: c, name, since: std::time::SystemTime::now() });
                }
                if let Err(e) = addons::open_url(&url) {
                    self.fx_error = Some(e);
                    self.pending = None;
                }
            }
            Message::CancelWait => {
                self.pending = None;
            }
            Message::DeleteLocal(name) => {
                if let Err(e) = addons::trash_preset(&name) {
                    self.fx_error = Some(e);
                }
                return Task::perform(blocking(addons::scan_downloads), Message::Downloads);
            }
            Message::MarketLink(url) => {
                let show = self.bring_forward();
                let Some(variant) = addons::preset_link(&url) else {
                    self.incoming = Some(Incoming {
                        variant: String::new(),
                        name: None,
                        state: IncomingState::Failed("This add-on is a plugin. Pipemic can use EQ presets only.".into()),
                    });
                    return show;
                };
                let state = if self.signed_in { IncomingState::Downloading } else { IncomingState::Waiting };
                self.incoming = Some(Incoming { variant: variant.clone(), name: None, state });
                self.fx = None;
                let v = variant.clone();
                let mut t = vec![
                    show,
                    Task::perform(blocking(addons::scan_downloads), Message::Downloads),
                    Task::perform(blocking(move || wave_catalog::marketplace::variant_info(&v)), {
                        let v = variant.clone();
                        move |r| Message::IncomingInfo(v.clone(), r)
                    }),
                ];
                if self.signed_in {
                    let v = variant.clone();
                    t.push(Task::perform(blocking(move || addons::install_variant(&v)), move |result| Message::Installed {
                        variant: variant.clone(),
                        channel: None,
                        url: None,
                        result,
                    }));
                }
                if !matches!(self.addons, AddonList::Done(Ok(_))) {
                    t.push(self.update(Message::RefreshAddons));
                }
                return Task::batch(t);
            }
            Message::IncomingOk => self.incoming = None,
            Message::IncomingInfo(variant, r) => {
                let Ok((name, thumb)) = r else { return Task::none() };
                let Some(i) = self.incoming.as_mut().filter(|i| i.variant == variant) else { return Task::none() };
                i.name = Some(name);
                self.resolve_incoming();
                if let Some(url) = thumb {
                    let key = format!("variant:{variant}");
                    if !self.thumbs.contains_key(&key) {
                        return Task::perform(blocking(move || addons::thumbnails(vec![(key, url)])), Message::ThumbsDone);
                    }
                }
            }
            Message::Installed { variant, channel, url, result } => {
                use wave_catalog::marketplace::InstallError;
                let rescan = Task::perform(blocking(addons::scan_downloads), Message::Downloads);
                let from_link = !variant.is_empty();
                // The website no longer accepted the session (even renewed).
                self.signed_in = addons::signed_in();
                match (from_link, result) {
                    (true, r) => {
                        let Some(i) = self.incoming.as_mut().filter(|i| i.variant == variant) else { return rescan };
                        i.state = match r {
                            // The marketplace name reads better than the one inside the file.
                            Ok((name, _)) => IncomingState::Added(i.name.clone().unwrap_or(name)),
                            Err(InstallError::Other(e)) => IncomingState::Failed(e),
                            Err(_) => IncomingState::Waiting,
                        };
                        self.resolve_incoming();
                        return rescan;
                    }
                    (false, Ok((name, path))) => {
                        self.toast = None;
                        let Some(c) = channel else { return rescan };
                        let msg = format!("{name} added to {}", self.channel_label(&c));
                        return Task::batch([
                            rescan,
                            Task::perform(send(json!({"cmd": "eq", "channel": c, "import": path, "name": name})), Message::EqDone).chain(self.toast(msg)),
                        ]);
                    }
                    (false, Err(InstallError::Other(e))) => {
                        self.toast = None;
                        self.fx_error = Some(e);
                    }
                    // Not signed in any more, or not in the library yet: get it on the website.
                    (false, Err(_)) => {
                        self.toast = None;
                        if let (Some(u), Some(c)) = (url, channel) {
                            let name = self.addon_url_name(&u).unwrap_or_default();
                            self.pending = Some(Pending { channel: c, name, since: std::time::SystemTime::now() });
                            if let Err(e) = addons::open_url(&u) {
                                self.fx_error = Some(e);
                            }
                        }
                    }
                }
                return rescan;
            }
            Message::SignIn => {
                if self.signing_in {
                    return Task::none();
                }
                self.signing_in = true;
                return Task::perform(blocking(addons::sign_in), Message::SignedIn);
            }
            Message::SignedIn(r) => {
                self.signing_in = false;
                self.signed_in = addons::signed_in();
                if let Err(e) = r {
                    self.fx_error = Some(e);
                }
                // A link was waiting for the sign-in: install it now.
                if let (true, Some(i)) = (self.signed_in, self.incoming.as_mut()) {
                    if i.state == IncomingState::Waiting && !i.variant.is_empty() {
                        i.state = IncomingState::Downloading;
                        let variant = i.variant.clone();
                        let v = variant.clone();
                        return Task::perform(blocking(move || addons::install_variant(&v)), move |result| Message::Installed {
                            variant: variant.clone(),
                            channel: None,
                            url: None,
                            result,
                        });
                    }
                }
            }
            Message::SignOut => {
                addons::sign_out();
                self.signed_in = false;
            }
            Message::Focus(f) => self.focused = f,
            Message::ToastDone(id) => {
                if self.toast.as_ref().is_some_and(|(i, _)| *i == id) {
                    self.toast = None;
                }
            }
            Message::EqImport(c, path, name) => {
                let req = json!({"cmd": "eq", "channel": c, "import": path, "name": name});
                return Task::perform(send(req), Message::EqDone);
            }
            Message::EqEnabled(c, on) => return Task::perform(send(json!({"cmd": "eq", "channel": c, "enabled": on})), Message::EqDone),
            Message::EqDone(r) => match r {
                Ok(v) => {
                    self.fx_error = None;
                    self.take_reply(&v);
                }
                Err(e) => self.fx_error = Some(e),
            },
            Message::FileDropped(p) => {
                if let Some(c) = self.fx.clone() {
                    return self.update(Message::EqImport(c, p, None));
                }
            }
            Message::SetOutput(mix, o) => {
                return Task::perform(send(json!({"cmd": "mix", "mix": mix, "output": o.name})), Message::Sent)
            }
        }
        Task::none()
    }

    /// Waiting for the file of an add-on opened from the marketplace?
    fn waiting_link(&self) -> bool {
        self.incoming.as_ref().is_some_and(|i| i.state == IncomingState::Waiting)
    }

    /// Show a toast that stays until replaced.
    fn toast_now(&mut self, msg: String) {
        let id = self.toast.as_ref().map_or(0, |(i, _)| i + 1);
        self.toast = Some((id, msg));
    }

    /// Name of the marketplace add-on with this product page.
    fn addon_url_name(&self, url: &str) -> Option<String> {
        let AddonList::Done(Ok(list)) = &self.addons else { return None };
        list.iter().find(|a| a.url == url).map(|a| a.name.clone())
    }

    /// The marketplace add-on an incoming link points at.
    fn incoming_addon(&self) -> Option<&wave_catalog::marketplace::Addon> {
        let i = self.incoming.as_ref()?;
        let AddonList::Done(Ok(list)) = &self.addons else { return None };
        list.iter().find(|a| a.variant_ids.contains(&i.variant))
    }

    /// Its file is in Downloads (or the library): add it to the library.
    fn resolve_incoming(&mut self) {
        if !self.waiting_link() {
            return;
        }
        let name = self.incoming.as_ref().and_then(|i| i.name.clone()).or_else(|| self.incoming_addon().map(|a| a.name.clone()));
        let Some(name) = name else { return };
        let Some(local) = self.downloads.iter().find(|l| l.is(&name)).cloned() else { return };
        let state = match addons::add_to_library(&local.path) {
            Ok(_) => IncomingState::Added(name),
            Err(e) => IncomingState::Failed(e),
        };
        if let Some(i) = &mut self.incoming {
            i.state = state;
        }
    }

    /// Popup for "Open with Wave Link": wait for the download, then say
    /// it's added and where to use it.
    fn incoming_view<'a>(&'a self, i: &'a Incoming) -> Element<'a, Message> {
        let addon = self.incoming_addon();
        let name = i.name.clone().or_else(|| addon.map(|a| a.name.clone())).unwrap_or_else(|| "your preset".into());
        let handle = self.thumbs.get(&format!("variant:{}", i.variant)).or_else(|| addon.and_then(|a| self.thumbs.get(&a.id)));
        let thumb: Element<'a, Message> = match handle {
            Some(h) => image(h.clone()).width(128).height(64).border_radius(8).into(),
            None => Self::thumb_blank(64.0),
        };
        let (title, body, done): (String, String, bool) = match &i.state {
            IncomingState::Downloading => (format!("Installing {name}…"), "Downloading it from the Elgato marketplace.".into(), false),
            IncomingState::Waiting if self.signed_in => (
                format!("Getting {name}"),
                "The marketplace didn't hand it over yet. Click Download on the Elgato page; Pipemic picks the file up from your Downloads folder.".into(),
                false,
            ),
            IncomingState::Waiting => (
                format!("Getting {name}"),
                "Sign in to Elgato once and Pipemic installs add-ons by itself. Or click Download on the Elgato page; Pipemic picks the file up from your Downloads folder.".into(),
                false,
            ),
            IncomingState::Added(n) => (
                format!("{n} was added"),
                "Now add it to a channel: click the effects button under a channel's apps, then Use.".into(),
                true,
            ),
            IncomingState::Failed(e) => ("Couldn't add this add-on".into(), e.clone(), true),
        };
        let action = button(text(if done { "OK" } else { "Cancel" }).size(13).font(SEMIBOLD))
            .padding([7, 18])
            .style(if done { primary_button } else { soft_button })
            .on_press(Message::IncomingOk);
        let body = column![
            row![thumb, column![text(title).size(18).font(SEMIBOLD), text(body).size(12).color(DIM)].spacing(6).width(Length::Fill)]
                .spacing(16)
                .align_y(Alignment::Center),
            row![Space::new().width(Length::Fill)]
                .push((i.state == IncomingState::Waiting && !self.signed_in).then(|| {
                    button(text(if self.signing_in { "Signing in…" } else { "Sign in to Elgato" }).size(13).font(SEMIBOLD))
                        .padding([7, 18])
                        .style(primary_button)
                        .on_press_maybe((!self.signing_in).then_some(Message::SignIn))
                }))
                .push(action)
                .spacing(8),
        ]
        .spacing(20);
        // Closes with its button only, so a stray click can't hide it.
        opaque(
            container(opaque(sheet(body.padding(24)).width(520)))
                .center(Length::Fill)
                .style(|_| container::background(rgba(0, 0, 0, 0.55))),
        )
    }

    /// Show a short confirmation for a few seconds.
    fn toast(&mut self, msg: String) -> Task<Message> {
        let id = self.toast.as_ref().map_or(0, |(i, _)| i + 1);
        self.toast = Some((id, msg));
        Task::perform(tokio_sleep(5000), move |_| Message::ToastDone(id))
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

        card(column![header, hmeter(self.peak("channel", &cid)), app_box, self.fx_chip(c, compact), body].spacing(12).padding(12))
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

    /// Elgato account: sign in once so Get / "Open with Wave Link" install
    /// by themselves.
    fn account_row<'a>(&'a self) -> Element<'a, Message> {
        let (msg, b): (&str, Element<'a, Message>) = if self.signed_in {
            (
                "Signed in to Elgato: Get installs presets directly.",
                button(text("Sign out").size(12)).padding([5, 12]).style(soft_button).on_press(Message::SignOut).into(),
            )
        } else {
            (
                "Sign in to Elgato once and Get installs presets without the website's Download button.",
                button(text(if self.signing_in { "Signing in…" } else { "Sign in" }).size(12).font(SEMIBOLD))
                    .padding([5, 14])
                    .style(primary_button)
                    .on_press_maybe((!self.signing_in).then_some(Message::SignIn))
                    .into(),
            )
        };
        group(column![row![text(msg).size(12).color(DIM).width(Length::Fill), b].spacing(10).align_y(Alignment::Center)])
    }

    /// Marketplace add-on a preset file came from, if listed.
    fn addon_for(&self, l: &addons::LocalPreset) -> Option<&wave_catalog::marketplace::Addon> {
        let AddonList::Done(Ok(list)) = &self.addons else { return None };
        list.iter().find(|a| a.runs_on_linux() && l.is(&a.name))
    }

    /// Marketplace add-on a preset name belongs to, if listed.
    fn addon_named(&self, name: &str) -> Option<&wave_catalog::marketplace::Addon> {
        let AddonList::Done(Ok(list)) = &self.addons else { return None };
        let probe = addons::LocalPreset { name: name.to_string(), path: Default::default(), version: None, modified: std::time::UNIX_EPOCH };
        list.iter().find(|a| a.runs_on_linux() && probe.is(&a.name))
    }

    /// Is the marketplace version newer than the downloaded one?
    fn newer(latest: Option<&str>, have: Option<&str>) -> bool {
        matches!((latest, have), (Some(l), Some(h)) if wave_catalog::is_newer(l, h))
    }

    /// Marketplace thumbnails are 2:1; `size` is the height.
    fn thumb_blank<'a>(size: f32) -> Element<'a, Message> {
        container(icon(icons::EQ, size * 0.45, DIM))
            .center_y(size)
            .center_x(size * 2.0)
            .style(|_| container::Style { background: Some(RAIL.into()), border: border::rounded(8), ..Default::default() })
            .into()
    }

    /// Thumbnail of the marketplace add-on a preset came from.
    fn thumb_for<'a>(&'a self, name: &str, size: f32) -> Element<'a, Message> {
        match self.addon_named(name).and_then(|a| self.thumbs.get(&a.id)) {
            Some(h) => image(h.clone()).width(size * 2.0).height(size).border_radius(8).into(),
            None => Self::thumb_blank(size),
        }
    }

    /// Under a strip's apps: the channel's EQ (name, on/off) or "Effects".
    fn fx_chip<'a>(&'a self, c: &Value, compact: bool) -> Element<'a, Message> {
        let cid = c["id"].as_str().unwrap_or("").to_string();
        let eq = &c["eq"];
        let has = eq.is_object();
        let on = has && eq["enabled"] != false;
        let label = if !has {
            if compact { "Effects".to_string() } else { "Add effect".to_string() }
        } else if compact {
            "EQ".to_string()
        } else {
            eq["name"].as_str().unwrap_or("EQ").to_string()
        };
        let content = row![
            icon(icons::EQ, 14.0, if on { ON } else { DIM }),
            text(label).size(12).color(if has { TEXT } else { DIM }).wrapping(iced::widget::text::Wrapping::None),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let hint = match (has, on) {
            (false, _) => "Add an equalizer preset to this channel".to_string(),
            (true, true) => format!("Equalizer “{}” is on. Click to change it.", eq["name"].as_str().unwrap_or("")),
            (true, false) => format!("Equalizer “{}” is off. Click to change it.", eq["name"].as_str().unwrap_or("")),
        };
        let b = button(container(content).clip(true)).width(Length::Fill).padding([5, 8]).style(soft_button).on_press(Message::ShowFx(Some(cid)));
        iced::widget::tooltip(b, tooltip_box(hint), iced::widget::tooltip::Position::Top).gap(6).into()
    }

    /// A channel's effects: its EQ, presets already downloaded, and the
    /// marketplace add-ons that work on Linux (with a link to get them).
    fn fx_sheet<'a>(&'a self, cid: &str) -> Element<'a, Message> {
        let c = self.channel(cid);
        let label = self.channel_label(cid);
        let color = channel_color(cid);
        let eq = &c["eq"];
        let current = eq["name"].as_str().map(String::from);
        let badge = container(icon(icons::channel_glyph(cid), 16.0, Color::WHITE)).center(30).style(move |_| container::Style {
            background: Some(color.into()),
            border: border::rounded(9),
            ..Default::default()
        });

        // Current equalizer: one switch (bypass to compare); picking another
        // preset below replaces it.
        let eq_group: Element<'a, Message> = if eq.is_object() {
            let on = eq["enabled"] != false;
            let mut bands = column![].spacing(4);
            for b in eq["band"].as_array().into_iter().flatten() {
                let off = b["on"] == false;
                bands = bands.push(text(format!("{}{}", addons::band_label(b), if off { " (off)" } else { "" })).size(12).color(DIM));
            }
            let c1 = cid.to_string();
            let name = current.clone().unwrap_or_default();
            group(column![
                row![
                    self.thumb_for(&name, 64.0),
                    column![
                        text(name.clone()).size(14).font(SEMIBOLD),
                        text(if on { "On" } else { "Off: the sound passes unchanged" }).size(11).color(if on { ON } else { DIM }),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    toggler(on).size(18).style(switch_style).on_toggle(move |v| Message::EqEnabled(c1.clone(), v)),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
                bands,
            ]
            .spacing(10))
        } else {
            group(column![text("No equalizer on this channel. Pick a preset below.").size(12).color(DIM)])
        };

        // Downloaded presets.
        let mut local = column![].spacing(2);
        for p in &self.downloads {
            let addon = self.addon_for(p);
            // The marketplace name when known (the one inside the file can differ).
            let shown = addon.map_or_else(|| p.name.clone(), |a| a.name.clone());
            let using = current.as_deref().is_some_and(|c| c == shown || c == p.name);
            let update = addon.is_some_and(|a| Self::newer(a.version.as_deref(), p.version.as_deref()));
            let mut info = column![text(shown.clone()).size(13)].spacing(2).width(Length::Fill);
            if update {
                info = info.push(text("Update available in the marketplace").size(11).color(ACCENT));
            }
            let thumb: Element<'a, Message> = match addon.and_then(|a| self.thumbs.get(&a.id)) {
                Some(h) => image(h.clone()).width(96).height(48).border_radius(8).into(),
                None => Self::thumb_blank(48.0),
            };
            let mut line = row![thumb, info].spacing(10).align_y(Alignment::Center);
            line = if using {
                line.push(text("In use").size(11).color(ON))
            } else {
                line.push(
                    button(text("Use").size(12).font(SEMIBOLD))
                        .padding([5, 14])
                        .style(primary_button)
                        .on_press(Message::EqImport(cid.to_string(), p.path.clone(), Some(shown.clone()))),
                )
            };
            let del = button(text("Delete").size(12).color(DIM)).padding([5, 10]).style(flat_button(false, MUTED)).on_press(Message::DeleteLocal(p.name.clone()));
            line = line.push(iced::widget::tooltip(del, tooltip_box("Move this preset's files to the trash".into()), iced::widget::tooltip::Position::Left).gap(6));
            local = local.push(container(line).padding([6, 10]));
        }
        let local_hint = if self.downloads.is_empty() {
            "No presets yet. Get one below: it's applied here as soon as it's downloaded.".to_string()
        } else {
            format!("Added presets, and EQ presets found in {}. You can also drop a preset file on this window.", addons::downloads_dir().display())
        };

        // Marketplace.
        let waiting = self.pending.as_ref().filter(|p| p.channel == cid).map(|p| p.name.clone());
        let market: Element<'a, Message> = match &self.addons {
            AddonList::Idle | AddonList::Loading => text("Loading the marketplace…").size(12).color(DIM).into(),
            AddonList::Done(Err(e)) => text(format!("Couldn't load the marketplace: {e}")).size(12).color(MUTED).into(),
            AddonList::Done(Ok(list)) => {
                let mut col = column![].spacing(2);
                let mut shown = 0;
                let mut items: Vec<_> = list.iter().filter(|a| a.runs_on_linux()).collect();
                items.sort_by_key(|a| a.name.to_lowercase());
                for a in items {
                    let local = self.downloads.iter().find(|l| l.is(&a.name));
                    let update = local.is_some_and(|l| Self::newer(a.version.as_deref(), l.version.as_deref()));
                    // Installed ones live in the section above; they stay here only to offer an update.
                    if local.is_some() && !update && waiting.as_deref() != Some(a.name.as_str()) {
                        continue;
                    }
                    shown += 1;
                    let info = column![
                        text(a.name.clone()).size(13).font(SEMIBOLD),
                        text(if a.summary.is_empty() { a.org.clone() } else { format!("{} · {}", a.org, a.summary) }).size(11).color(DIM),
                    ]
                    .spacing(2)
                    .width(Length::Fill);
                    let get = |label: &'static str| -> Element<'a, Message> {
                        let b = button(text(label).size(12)).padding([5, 14]).style(soft_button).on_press(Message::Get { name: a.name.clone(), url: a.url.clone(), variant: a.variant_ids.first().cloned() });
                        iced::widget::tooltip(
                            b,
                            tooltip_box(
                                if self.signed_in {
                                    "Adds it to your Elgato library if it's free, then installs it here.".into()
                                } else {
                                    "Opens the product page. Download it there; Pipemic applies it here when it arrives.".into()
                                },
                            ),
                            iced::widget::tooltip::Position::Left,
                        )
                        .gap(6)
                        .into()
                    };
                    let in_use = local.is_some() && current.as_deref().is_some_and(|c| c == a.name || local.is_some_and(|l| l.name == c));
                    let action: Element<'a, Message> = if waiting.as_deref() == Some(a.name.as_str()) {
                        row![
                            text("Waiting for the download…").size(11).color(ACCENT),
                            button(text("Cancel").size(12)).padding([5, 14]).style(soft_button).on_press(Message::CancelWait),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .into()
                    } else if update {
                        get("Update")
                    } else if in_use {
                        text("In use").size(11).color(ON).into()
                    } else if let Some(l) = local {
                        button(text("Use").size(12).font(SEMIBOLD))
                            .padding([5, 14])
                            .style(primary_button)
                            .on_press(Message::EqImport(cid.to_string(), l.path.clone(), Some(a.name.clone())))
                            .into()
                    } else {
                        get("Get")
                    };
                    let thumb: Element<'a, Message> = match self.thumbs.get(&a.id) {
                        Some(h) => image(h.clone()).width(128).height(64).border_radius(8).into(),
                        None => Self::thumb_blank(64.0),
                    };
                    col = col.push(
                        container(row![thumb, container(info).clip(true).width(Length::Fill), action].spacing(12).align_y(Alignment::Center))
                            .padding([6, 10]),
                    );
                }
                if shown == 0 {
                    col = col.push(
                        container(text("You have every marketplace preset. New ones show up here when they're published.").size(12).color(DIM))
                            .padding([6, 10]),
                    );
                }
                col.into()
            }
        };

        let mut list = column![heading("EQUALIZER"), eq_group, heading("YOUR PRESETS"), text(local_hint).size(11).color(DIM)].spacing(8);
        if !self.downloads.is_empty() {
            list = list.push(group(local));
        }
        list = list
            .push(Space::new().height(4))
            .push(heading("ELGATO MARKETPLACE"))
            .push(text("EQ presets work on Linux. Plugins (Windows/macOS only) are not listed. Checked for new ones every hour.").size(11).color(DIM))
            .push(self.account_row())
            .push(group(column![market]));

        let mut body = column![row![
            badge,
            column![
                text(format!("Effects on {label}")).size(18).font(SEMIBOLD),
                text("Shapes this channel's sound before it reaches your mixes.").size(12).color(DIM),
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        // Notices about add-ons ("… added to Microphone") show here, not behind the sheet.
        .push(self.toast.as_ref().map(|(_, m)| text(m.clone()).size(12).color(ON)))
        .push(button(text("Done").size(13).font(SEMIBOLD)).padding([7, 16]).style(soft_button).on_press(Message::ShowFx(None)))
        .spacing(12)
        .align_y(Alignment::Center)]
        .spacing(16);
        if let Some(e) = &self.fx_error {
            body = body.push(text(e.clone()).size(12).color(MUTED));
        }
        body = body.push(scrollable(list.padding(iced::Padding::ZERO.right(12))).height(Length::Fill));
        modal(sheet(body.padding(24)).width(600).height(660).into(), Message::ShowFx(None))
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

    fn view(&self, _window: iced::window::Id) -> Element<'_, Message> {
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
                row![heading("INPUTS"), Space::new().width(Length::Fill)]
                    .push(self.toast.as_ref().filter(|_| self.fx.is_none()).map(|(_, m)| text(m.clone()).size(12).color(ON)))
                    .extend([status, gear.into()])
                    .spacing(12)
                    .align_y(Alignment::Center),
                body,
            ]
            .spacing(12),
        )
        .padding([16, 20])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::background(BG));
        if let Some(i) = &self.incoming {
            stack![main, self.incoming_view(i)].into()
        } else if self.show_settings {
            stack![main, self.settings_view()].into()
        } else if let Some(cid) = self.fx.as_deref() {
            stack![main, self.fx_sheet(cid)].into()
        } else if self.show_device {
            stack![main, self.device_sheet()].into()
        } else if let Some(cid) = self.open.as_deref() {
            stack![main, self.app_picker(cid)].into()
        } else {
            main.into()
        }
    }
}
