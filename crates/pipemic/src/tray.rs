//! Tray icon (StatusNotifierItem) and single-instance handling.
//!
//! Pipemic keeps running when its window is closed: the tray icon reopens
//! it, mutes the mic, or quits. Launching `pipemic` again while it runs
//! asks the running copy to show its window (over a socket in
//! `$XDG_RUNTIME_DIR`) instead of starting a second one.

use iced::futures::channel::mpsc::Sender;
use iced::futures::SinkExt;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub enum Event {
    Show,
    /// A `wavelink://` link from the marketplace ("Open with Wave Link").
    Open(String),
    ToggleMute,
    Quit,
}

/// What the tray shows; the app pushes updates through [`TrayHandle`].
pub struct Tray {
    pub muted: bool,
    pub connected: bool,
    tx: Sender<Event>,
}

impl Tray {
    fn send(&mut self, e: Event) {
        let _ = self.tx.try_send(e);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "pipemic".into()
    }
    fn title(&self) -> String {
        "Pipemic".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }
    fn icon_name(&self) -> String {
        if self.connected && self.muted { "pipemic-muted" } else { "pipemic" }.into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let state = match (self.connected, self.muted) {
            (false, _) => "Wave:3 not connected",
            (true, true) => "Mic muted",
            (true, false) => "Mic live",
        };
        ksni::ToolTip { title: "Pipemic".into(), description: state.into(), ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Event::Show);
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Pipemic".into(),
                activate: Box::new(|t: &mut Self| t.send(Event::Show)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.muted { "Unmute mic" } else { "Mute mic" }.into(),
                enabled: self.connected,
                activate: Box::new(|t: &mut Self| t.send(Event::ToggleMute)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Event::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Cloneable handle the app keeps to update the icon. The tray lives on
/// its own thread (ksni's blocking API can't run inside iced's runtime), so
/// updates go over a channel.
#[derive(Clone)]
pub struct TrayHandle(std::sync::mpsc::Sender<(bool, bool)>);

impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrayHandle")
    }
}

impl TrayHandle {
    pub fn set(&self, connected: bool, muted: bool) {
        let _ = self.0.send((connected, muted));
    }
}

/// What the background subscription emits.
#[derive(Debug, Clone)]
pub enum Out {
    Ready(TrayHandle),
    Event(Event),
}

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("pipemic-ui.sock")
}

static LISTENER: OnceLock<Mutex<Option<UnixListener>>> = OnceLock::new();

/// Called first thing in `main`. If another Pipemic is running, ask it to
/// show its window (or open `link`) and return `false` (this process
/// should exit).
pub fn claim_single_instance(link: Option<&str>) -> bool {
    let path = socket_path();
    if let Ok(mut s) = UnixStream::connect(&path) {
        let msg = match link {
            Some(l) => format!("open {}\n", l.replace('\n', "")),
            None => "show\n".into(),
        };
        let _ = s.write_all(msg.as_bytes());
        return false;
    }
    let _ = std::fs::remove_file(&path); // stale socket from a crash
    if let Ok(l) = UnixListener::bind(&path) {
        let _ = LISTENER.set(Mutex::new(Some(l)));
    }
    true
}

/// Background stream: starts the tray and listens for "show" from new
/// launches. Runs for the app's lifetime.
pub fn run() -> impl iced::futures::Stream<Item = Out> {
    iced::stream::channel(16, async |mut out: Sender<Out>| {
        let (etx, mut erx) = iced::futures::channel::mpsc::channel::<Event>(16);

        if let Some(l) = LISTENER.get().and_then(|m| m.lock().ok()?.take()) {
            let mut tx = etx.clone();
            std::thread::spawn(move || {
                for s in l.incoming().flatten() {
                    let mut line = String::new();
                    let _ = s.take(2048).read_to_string(&mut line);
                    let ev = match line.trim().strip_prefix("open ") {
                        Some(url) => Event::Open(url.trim().to_string()),
                        None => Event::Show,
                    };
                    let _ = tx.try_send(ev);
                }
            });
        }

        let tray = Tray { muted: false, connected: false, tx: etx };
        let (utx, urx) = std::sync::mpsc::channel();
        let (ok_tx, ok_rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let mut ok_tx = Some(ok_tx);
            let r = (|| {
                let h: Handle<Tray> = tray.spawn()?;
                if let Some(t) = ok_tx.take() {
                    let _ = t.send(true);
                }
                for (connected, muted) in urx {
                    h.update(|t| {
                        t.connected = connected;
                        t.muted = muted;
                    });
                }
                Ok::<(), ksni::Error>(())
            })();
            if let Err(e) = r {
                eprintln!("pipemic: no tray icon ({e})");
            }
            if let Some(t) = ok_tx.take() {
                let _ = t.send(false);
            }
        });
        if ok_rx.await == Ok(true) {
            let _ = out.send(Out::Ready(TrayHandle(utx))).await;
        }

        use iced::futures::StreamExt;
        while let Some(e) = erx.next().await {
            if out.send(Out::Event(e)).await.is_err() {
                break;
            }
        }
    })
}
