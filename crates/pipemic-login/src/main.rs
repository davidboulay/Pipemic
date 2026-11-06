//! pipemic-login: shows Elgato's own sign-in page in a small window. Once
//! the marketplace website has a session, its cookies are written to
//! `$XDG_DATA_HOME/pipemic/elgato-session.json` (mode 0600) and the window
//! closes. Pipemic uses that session to download add-ons the user installs
//! from the marketplace ("Open with Wave Link"), like the website's own
//! Download button. The password never passes through Pipemic.
//!
//! The window has its own browser profile in `$XDG_DATA_HOME/pipemic/webview`
//! (WebKit's private mode drops the website's sign-in handshake cookies, so
//! signing in never completed there). Signing out in Pipemic deletes it.
//! Exit code 0 = signed in, 1 = closed without signing in.
//!
//! `--refresh`: no window. Loads the site with the kept profile so its own
//! code renews the 15-minute access cookie (the refresh cookie lasts 30
//! days), saves the session and exits 0; exit 3 if that didn't happen
//! within 25 s (the user has to sign in again).

use std::path::PathBuf;
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const SITE: &str = "https://marketplace.elgato.com";
/// Account page: sends a signed-out visitor to the sign-in form.
const START: &str = "https://marketplace.elgato.com/account";
/// The website's user cookie; with an access or refresh token cookie it
/// means a session.
const USER_COOKIE: &str = "kc_user";
const TOKEN_COOKIES: [&str; 2] = ["__Host-kc_at", "__Secure-kc_rt"];

fn data_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("pipemic")
}

fn session_path() -> PathBuf {
    data_dir().join("elgato-session.json")
}

fn save(cookies: &[(String, String)]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = session_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::json!({
        "cookies": cookies.iter().map(|(n, v)| serde_json::json!({"name": n, "value": v})).collect::<Vec<_>>(),
    });
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(body.to_string().as_bytes())?;
    std::fs::rename(&tmp, &path)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn main() {
    let refresh = std::env::args().any(|a| a == "--refresh");
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Sign in to Elgato · Pipemic")
        .with_inner_size(LogicalSize::new(520.0, 760.0))
        .with_visible(!refresh)
        .build(&event_loop)
        .expect("window");

    let profile = data_dir().join("webview");
    let _ = std::fs::create_dir_all(&profile);
    let mut context = wry::WebContext::new(Some(profile));
    let builder = WebViewBuilder::new_with_web_context(&mut context).with_url(START);
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("gtk box")).expect("webview")
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).expect("webview");

    let started = Instant::now();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(700));
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => std::process::exit(1),
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if refresh && started.elapsed() > Duration::from_secs(25) {
                    std::process::exit(3);
                }
                // Back on the marketplace with a session: keep it and close.
                let url = webview.url().unwrap_or_default();
                if std::env::var_os("PIPEMIC_LOGIN_DEBUG").is_some() {
                    let names: Vec<String> = match webview.cookies_for_url(SITE) {
                        Ok(c) => c.iter().map(|c| c.name().to_string()).collect(),
                        Err(e) => vec![format!("error: {e}")],
                    };
                    let all: Vec<String> = webview.cookies().map(|c| c.iter().map(|c| format!("{}@{}", c.name(), c.domain().unwrap_or("?"))).collect()).unwrap_or_default();
                    let mine: Vec<&String> = all.iter().filter(|c| c.contains("marketplace.elgato.com")).collect();
                    eprintln!("debug: url={} site-cookies={names:?} marketplace={mine:?}", url.split('?').next().unwrap_or(""));
                }
                let on_site = url.starts_with(SITE);
                if !on_site || started.elapsed() < Duration::from_secs(1) {
                    return;
                }
                // Every marketplace cookie, whatever its path (the refresh
                // token is scoped below the site root).
                let Ok(all) = webview.cookies() else { return };
                // A live access cookie: an expired one left from an earlier
                // sign-in doesn't count.
                let fresh = all.iter().any(|c| {
                    c.name() == "__Host-kc_at"
                        && c.expires().and_then(|e| e.datetime()).is_some_and(|d| d.unix_timestamp() > now() + 60)
                });
                let cookies: Vec<(String, String)> = all
                    .iter()
                    .filter(|c| c.domain().is_some_and(|d| d.trim_start_matches('.') == "marketplace.elgato.com"))
                    .map(|c| (c.name().to_string(), c.value().to_string()))
                    .collect();
                let has = |n: &str| cookies.iter().any(|(k, _)| k == n);
                if has(USER_COOKIE) && TOKEN_COOKIES.iter().any(|n| has(n)) && fresh {
                    match save(&cookies) {
                        Ok(()) => std::process::exit(0),
                        Err(e) => {
                            eprintln!("pipemic-login: saving the session failed: {e}");
                            std::process::exit(2)
                        }
                    }
                }
            }
            _ => {}
        }
    });
}
