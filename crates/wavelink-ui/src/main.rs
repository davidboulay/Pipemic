//! Wave Link mixer window. Talks to wavelinkd over its control socket
//! (`$XDG_RUNTIME_DIR/wavelinkd.sock`, JSON lines) and polls its status
//! 4×/s so changes made elsewhere (wavectl, the mic's touch pad) show up.

use iced::widget::{button, column, container, row, scrollable, slider, text, toggler, Space};
use iced::{time, Alignment, Element, Length, Subscription, Task, Theme};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("Wave Link")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::Dark)
        .window_size((900.0, 560.0))
        .run()
}

// ---- socket client -------------------------------------------------------

fn socket_path() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(Into::into)
        .unwrap_or_else(std::env::temp_dir)
        .join("wavelinkd.sock")
}

fn call(req: Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(socket_path()).map_err(|_| "wavelinkd is not running".to_string())?;
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
    Channel(String),
    Mix(String),
    Route { mix: String, channel: String },
    MicGain,
}

impl Target {
    fn request(&self, volume: Option<f32>, mute: Option<bool>) -> Value {
        let mut v = match self {
            Target::Channel(c) => json!({"cmd": "channel", "channel": c}),
            Target::Mix(m) => json!({"cmd": "mix", "mix": m}),
            Target::Route { mix, channel } => json!({"cmd": "route", "mix": mix, "channel": channel}),
            Target::MicGain => json!({"cmd": "mic"}),
        };
        if let Some(x) = volume {
            v[if *self == Target::MicGain { "gain" } else { "volume" }] = json!(x);
        }
        if let Some(m) = mute {
            v["mute"] = json!(m);
        }
        v
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Status(Result<Value, String>),
    Sent(Result<Value, String>),
    Slide(Target, f32),
    Release,
    Mute(Target, bool),
    MicFlag(&'static str, bool),
}

#[derive(Default)]
struct App {
    config: Value,
    mic: Value,
    error: Option<String>,
    /// While a fader is held, show its local value instead of polled state.
    dragging: Option<(Target, f32)>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        (Self::default(), Task::perform(send(json!({"cmd": "status"})), Message::Status))
    }

    fn subscription(&self) -> Subscription<Message> {
        time::every(Duration::from_millis(250)).map(|_| Message::Tick)
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Tick => return Task::perform(send(json!({"cmd": "status"})), Message::Status),
            Message::Status(r) | Message::Sent(r) => match r {
                Ok(v) => {
                    if !v["config"].is_null() {
                        self.config = v["config"].clone();
                    }
                    if !v["mic"].is_null() {
                        self.mic = v["mic"].clone();
                    }
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            },
            Message::Slide(t, v) => {
                let changed = self.dragging.as_ref().is_none_or(|(_, old)| (old - v).abs() > f32::EPSILON);
                self.dragging = Some((t.clone(), v));
                // Mic gain is a USB write: only send on release.
                if changed && t != Target::MicGain {
                    return Task::perform(send(t.request(Some(v), None)), Message::Sent);
                }
            }
            Message::Release => {
                if let Some((t, v)) = self.dragging.take() {
                    return Task::perform(send(t.request(Some(v), None)), Message::Sent);
                }
            }
            Message::Mute(t, m) => return Task::perform(send(t.request(None, Some(m))), Message::Sent),
            Message::MicFlag(k, on) => {
                let mut req = json!({"cmd": "mic"});
                req[k] = json!(on);
                return Task::perform(send(req), Message::Sent);
            }
        }
        Task::none()
    }

    // ---- view ------------------------------------------------------------

    /// Level as shown: the held fader's local value, else the daemon's state.
    fn level(&self, t: &Target) -> (f32, bool) {
        let (vol, mute) = self.saved(t);
        match &self.dragging {
            Some((dt, v)) if dt == t => (*v, mute),
            _ => (vol, mute),
        }
    }

    /// (volume dB, mute) from the last status the daemon sent.
    fn saved(&self, t: &Target) -> (f32, bool) {
        let find = |arr: &str, id: &str| {
            self.config[arr].as_array().and_then(|a| a.iter().find(|x| x["id"] == id).cloned()).unwrap_or(Value::Null)
        };
        let v = match t {
            Target::Channel(c) => find("channel", c),
            Target::Mix(m) => find("mix", m),
            Target::Route { mix, channel } => self.config["route"]
                .as_array()
                .and_then(|a| a.iter().find(|r| r["mix"] == *mix && r["channel"] == *channel).cloned())
                .unwrap_or(json!({"volume": 0.0, "mute": false})),
            Target::MicGain => return (self.mic["gain"].as_f64().unwrap_or(0.0) as f32, self.mic["mute"] == true),
        };
        (v["volume"].as_f64().unwrap_or(0.0) as f32, v["mute"] == true)
    }

    fn fader(&self, t: Target, width: f32) -> Element<'_, Message> {
        let (vol, mute) = self.level(&t);
        let (range, label) = if t == Target::MicGain {
            (0.0..=40.0, format!("{vol:.1}"))
        } else {
            (-60.0..=0.0, if vol <= -60.0 { "-inf".into() } else { format!("{vol:.1}") })
        };
        let tt = t.clone();
        let s = slider(range, vol, move |v| Message::Slide(tt.clone(), v)).step(0.5_f32).on_release(Message::Release).width(width);
        let m = button(text(if mute { "M" } else { "m" }).size(13))
            .style(if mute { button::danger } else { button::secondary })
            .on_press(Message::Mute(t, !mute));
        row![s, text(label).size(13).width(40), m].spacing(6).align_y(Alignment::Center).into()
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("Wave Link").size(22),
            Space::new().width(Length::Fill),
            text(self.error.clone().unwrap_or_else(|| "connected to wavelinkd".into())).size(13),
        ]
        .align_y(Alignment::Center);

        let empty = vec![];
        let channels = self.config["channel"].as_array().unwrap_or(&empty);
        let mixes = self.config["mix"].as_array().unwrap_or(&empty);
        const LABEL: f32 = 110.0;
        const CELL: f32 = 130.0;

        let mut grid = column![].spacing(10);
        let mut head = row![text("").width(LABEL), text("Channel").width(CELL + 80.0)].spacing(12);
        for m in mixes {
            head = head.push(text(m["label"].as_str().unwrap_or("?").to_string()).width(CELL + 80.0));
        }
        grid = grid.push(head);
        for c in channels {
            let cid = c["id"].as_str().unwrap_or("").to_string();
            let mut r = row![
                text(c["label"].as_str().unwrap_or("?").to_string()).width(LABEL),
                self.fader(Target::Channel(cid.clone()), CELL)
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            for m in mixes {
                let mid = m["id"].as_str().unwrap_or("").to_string();
                r = r.push(self.fader(Target::Route { mix: mid, channel: cid.clone() }, CELL));
            }
            grid = grid.push(r);
        }
        let mut master = row![text("Mix volume").width(LABEL), Space::new().width(CELL + 80.0)].spacing(12).align_y(Alignment::Center);
        for m in mixes {
            master = master.push(self.fader(Target::Mix(m["id"].as_str().unwrap_or("").to_string()), CELL));
        }
        grid = grid.push(master);

        let mic: Element<'_, Message> = if self.mic["connected"] == true {
            row![
                text("Wave:3 gain").width(LABEL),
                self.fader(Target::MicGain, 200.0),
                toggler(self.mic["lowcut"] == true).label("Low cut").on_toggle(|on| Message::MicFlag("lowcut", on)),
                toggler(self.mic["clipguard"] == true).label("Clipguard").on_toggle(|on| Message::MicFlag("clipguard", on)),
            ]
            .spacing(16)
            .align_y(Alignment::Center)
            .into()
        } else {
            text("Wave:3 not connected").into()
        };

        container(scrollable(column![header, grid, text("Microphone").size(18), mic].spacing(18)))
            .padding(20)
            .into()
    }
}
