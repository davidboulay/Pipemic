//! pipemicd: Pipemic mixer daemon for PipeWire.
//!
//! - Each app channel is a virtual sink ("Pipemic Music") apps play into.
//! - Each mix is a filter-chain (two builtin mixers, one gain per channel)
//!   loaded in this process. Source mixes publish an `Audio/Source`;
//!   the monitor mix plays to the default output.
//! - A registry-driven graph keeper wires channels into mixes, pushes gain
//!   changes and cuts feedback links.
//! - Config: `$XDG_CONFIG_HOME/pipemic/mixer.toml`; control socket:
//!   `$XDG_RUNTIME_DIR/pipemicd.sock` (see control.rs).
//!
//! Everything in PipeWire is owned by this process and disappears on exit.

mod control;
mod links;
mod meter;
mod mic;
mod model;

use anyhow::{bail, Context, Result};
use control::{Job, Reply, Request};
use links::{Graph, NodeMatch, Want};
use model::{check_db, Config};
use pipewire as pw;
use pw::loop_::Signal;
use pw::properties::properties;
use std::cell::RefCell;
use std::ffi::CString;
use std::rc::Rc;

fn load_module(ctx: &pw::context::ContextRc, name: &str, args: &str) -> Result<()> {
    let name_c = CString::new(name)?;
    let args_c = CString::new(args)?;
    // SAFETY: valid context pointer, NUL-terminated strings; the module is
    // owned by the context and freed with it.
    let m = unsafe {
        pipewire_sys::pw_context_load_module(ctx.as_raw_ptr(), name_c.as_ptr(), args_c.as_ptr(), std::ptr::null_mut())
    };
    if m.is_null() {
        bail!("loading {name} failed: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

/// What a request changed, for the main loop to act on.
#[derive(Default)]
struct Outcome {
    /// Mixes whose gains changed.
    gains: Vec<usize>,
    /// Config changed: write it out.
    save: bool,
    rules: bool,
    /// Mix whose output device changed.
    output: Option<usize>,
    /// Reply with the app and output lists (with every installed app).
    apps: bool,
    installed: bool,
    /// output_fallback changed.
    fallback: bool,
}

/// Apps for the picker: running ones, then (if asked) every installed app,
/// then names that only appear in rules. Each with its channel and whether
/// a rule (rather than its category) put it there.
fn app_list(cfg: &Config, graph: &Graph, installed: bool) -> serde_json::Value {
    let router = cfg.router();
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut push = |name: &str, icon: Option<&str>, keys: &[String], cats: &[String], running: bool, playing: bool| {
        if seen.iter().any(|s| s == name) {
            return;
        }
        seen.push(name.to_owned());
        let (channel, explicit) = router.route(keys, cats).map_or((None, false), |(c, e)| (Some(c.to_owned()), e));
        // The channel its category would pick, if it was taken out of it.
        let off = if router.is_excluded(keys) && channel.is_none() { router.auto(cats) } else { None };
        out.push(serde_json::json!({"name": name, "icon": icon, "keys": keys, "channel": channel,
            "explicit": explicit, "running": running, "playing": playing, "excluded_from": off}));
    };
    for a in graph.live_apps() {
        push(&a.name, a.icon.as_deref(), &a.keys, &a.categories, true, a.playing);
    }
    if installed {
        let mut all = graph.installed();
        all.sort_by_key(|e| e.name.to_lowercase());
        for e in all.into_iter().filter(|e| e.stem != "pipemic") {
            push(&e.name, Some(&e.icon), &e.keys(), &e.categories, false, false);
        }
    }
    for p in &cfg.excluded {
        push(p, None, std::slice::from_ref(p), &[], false, false);
    }
    for r in &cfg.rules {
        for p in &r.apps {
            push(p, None, std::slice::from_ref(p), &[], false, false);
        }
    }
    serde_json::Value::Array(out)
}

/// All names and the categories of an app the picker named.
fn app_identity(graph: &Graph, name: &str) -> (Vec<String>, Vec<String>) {
    if let Some(a) = graph.live_apps().into_iter().find(|a| a.name == name) {
        return (a.keys, a.categories);
    }
    if let Some(e) = graph.installed().into_iter().find(|e| e.name == name) {
        return (e.keys(), e.categories);
    }
    (vec![name.to_owned()], vec![])
}

/// Apply one control request to the config.
fn apply(cfg: &mut Config, graph: &Graph, req: Request) -> Result<Outcome> {
    let all: Vec<usize> = (0..cfg.mixes.len()).collect();
    let gains = |g: Vec<usize>| Outcome { gains: g, save: true, ..Default::default() };
    Ok(match req {
        Request::Status => Outcome::default(),
        Request::Apps { installed } => Outcome { apps: true, installed, ..Default::default() },
        Request::Settings { output_fallback } => {
            if let Some(f) = output_fallback {
                cfg.output_fallback = f;
            }
            Outcome { save: true, fallback: output_fallback.is_some(), ..Default::default() }
        }
        Request::Levels | Request::Mic(_) => unreachable!("handled on the control thread"),
        Request::Assign { app, channel } => {
            let (keys, cats) = app_identity(graph, &app);
            let was = cfg.router().route(&keys, &cats).map(|(c, e)| (c.to_owned(), e));
            cfg.assign(&app, &keys, None)?;
            // Removing an app its category put here: keep it out for good.
            if channel.is_none() && matches!(&was, Some((_, false))) {
                cfg.exclude(&app, &keys);
            }
            // Only write a rule when the app wouldn't land there by itself.
            if let Some(c) = channel.as_deref() {
                if cfg.router().auto(&cats) != Some(c) {
                    cfg.assign(&app, &keys, Some(c))?;
                }
            }
            Outcome { save: true, rules: true, apps: true, installed: true, ..Default::default() }
        }
        Request::Channel { channel, volume, mute, link } => {
            let ci = cfg.channel_index(&channel)?;
            if let Some(v) = volume {
                cfg.channels[ci].volume = check_db(v)?;
            }
            if let Some(m) = mute {
                cfg.channels[ci].mute = m;
            }
            if let Some(l) = link {
                cfg.channels[ci].link = l;
            }
            gains(all)
        }
        Request::Mix { mix, volume, mute, output } => {
            let mi = cfg.mix_index(&mix)?;
            if let Some(v) = volume {
                cfg.mixes[mi].volume = check_db(v)?;
            }
            if let Some(m) = mute {
                cfg.mixes[mi].mute = m;
            }
            let mut o = gains(vec![mi]);
            if let Some(out) = output {
                if cfg.mixes[mi].kind != model::MixKind::Monitor {
                    bail!("only a monitor mix has an output device");
                }
                cfg.mixes[mi].output = Some(out).filter(|s| !s.is_empty());
                o.output = Some(mi);
            }
            o
        }
        Request::Route { mix, channel, volume, mute } => {
            let mi = cfg.mix_index(&mix)?;
            cfg.channel_index(&channel)?;
            if let Some(v) = volume {
                check_db(v)?;
            }
            let r = cfg.route_mut(&mix, &channel);
            if let Some(v) = volume {
                r.volume = v;
            }
            if let Some(m) = mute {
                r.mute = m;
            }
            gains(vec![mi])
        }
    })
}

/// Run the loop until the server has processed everything sent so far.
fn roundtrip(main_loop: &pw::main_loop::MainLoopRc, core: &pw::core::CoreRc) {
    let Ok(pending) = core.sync(0) else { return };
    let ml = main_loop.clone();
    let _l = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                ml.quit();
            }
        })
        .register();
    main_loop.run();
}

fn main() -> Result<()> {
    pw::init();
    let cfg_path = model::config_path();
    let cfg = Config::load_or_init(&cfg_path)?;

    let main_loop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let mut signals = Vec::new();
    for sig in [Signal::SIGINT, Signal::SIGTERM] {
        let weak = main_loop.downgrade();
        signals.push(main_loop.loop_().add_signal_local(sig, move || {
            if let Some(l) = weak.upgrade() {
                l.quit();
            }
        }));
    }
    let context = pw::context::ContextRc::new(&main_loop, None).context("PipeWire context")?;
    let core = context.connect_rc(None).context("connecting to PipeWire")?;

    // If PipeWire goes away (restart, crash), exit non-zero so systemd restarts us.
    let lost = Rc::new(std::cell::Cell::new(false));
    let _core_listener = {
        let weak = main_loop.downgrade();
        let lost = lost.clone();
        core.add_listener_local()
            .error(move |id, _seq, res, msg| {
                if id == pw::core::PW_ID_CORE {
                    eprintln!("pipemicd: lost PipeWire connection ({res}: {msg})");
                    lost.set(true);
                    if let Some(l) = weak.upgrade() {
                        l.quit();
                    }
                }
            })
            .register()
    };

    // Channels: one virtual sink per app channel.
    let mut sinks = Vec::new();
    for ch in cfg.channels.iter().filter(|c| c.device.is_none()) {
        let sink = core
            .create_object::<pw::node::Node>(
                "adapter",
                &properties! {
                    "factory.name" => "support.null-audio-sink",
                    "node.name" => Config::sink_node_name(ch),
                    "node.description" => format!("Pipemic {}", ch.label),
                    "media.class" => "Audio/Sink",
                    "audio.position" => "FL,FR",
                    "monitor.channel-volumes" => "true",
                    "object.linger" => "false",
                },
            )
            .with_context(|| format!("creating channel {}", ch.id))?;
        sinks.push(sink);
    }

    // Mixes: one filter-chain each, running inside this process.
    for mi in 0..cfg.mixes.len() {
        load_module(&context, "libpipewire-module-filter-chain", &cfg.filter_chain_args(mi))
            .with_context(|| format!("mix {}", cfg.mixes[mi].id))?;
    }

    // Device channels (the mic) go through a loopback bridge: the device keeps
    // its own clock and the bridge resamples into the mixes' graph. Linked
    // directly, a USB mic follows the output's clock and XRUNs nonstop
    // (Wave:3 behind a dock: ~47/s, heard as a robot voice; 2026-11-05).
    let mut wants = Vec::new();
    for ch in &cfg.channels {
        let Some(prefix) = &ch.device else { continue };
        load_module(&context, "libpipewire-module-loopback", &Config::bridge_args(ch))
            .with_context(|| format!("bridge for {}", ch.id))?;
        wants.push(Want {
            out_node: NodeMatch::Prefix(prefix.clone()),
            out_ports: vec!["capture_MONO".to_owned(), "capture_FL".to_owned()],
            in_node: format!("{}.in", Config::bridge_node_name(ch)),
            in_port: "input_MONO".to_owned(),
        });
    }

    // Wiring: every channel into every mix (gain decides what's audible).
    for m in &cfg.mixes {
        for (i, ch) in cfg.channels.iter().enumerate() {
            for (side, pos) in ["FL", "FR"].iter().enumerate() {
                let (out_node, out_ports) = match &ch.device {
                    None => (NodeMatch::Exact(Config::sink_node_name(ch)), vec![format!("monitor_{pos}")]),
                    Some(_) => (NodeMatch::Exact(Config::bridge_node_name(ch)), vec!["output_MONO".to_owned()]),
                };
                wants.push(Want { out_node, out_ports, in_node: Config::mix_in_node_name(m), in_port: Config::mix_in_port(i, side) });
            }
        }
    }
    // Meters: every channel source and every mix output into pipemic.meter.
    let mut taps = Vec::new();
    for ch in &cfg.channels {
        taps.push((meter::Kind::Channel, ch.id.clone()));
    }
    for m in &cfg.mixes {
        taps.push((meter::Kind::Mix, m.id.clone()));
    }
    let meter = meter::Meter::start(&core, taps).context("meter stream")?;
    let app_meter = meter::Probe::start(&core, meter::APP_NODE_NAME, "Pipemic app activity", meter::APP_SLOTS).context("app meter stream")?;
    for (i, ch) in cfg.channels.iter().enumerate() {
        for (side, pos) in ["FL", "FR"].iter().enumerate() {
            let (out_node, out_ports) = match &ch.device {
                None => (NodeMatch::Exact(Config::sink_node_name(ch)), vec![format!("monitor_{pos}")]),
                Some(_) => (NodeMatch::Exact(Config::bridge_node_name(ch)), vec!["output_MONO".to_owned()]),
            };
            wants.push(Want { out_node, out_ports, in_node: meter::NODE_NAME.into(), in_port: meter::in_port(i, side) });
        }
    }
    for (j, m) in cfg.mixes.iter().enumerate() {
        for (side, pos) in ["FL", "FR"].iter().enumerate() {
            wants.push(Want {
                out_node: NodeMatch::Exact(Config::mix_out_node_name(m)),
                out_ports: vec![format!("output_{pos}"), format!("capture_{pos}")],
                in_node: meter::NODE_NAME.into(),
                in_port: meter::in_port(cfg.channels.len() + j, side),
            });
        }
    }
    let bind: Vec<String> = cfg.mixes.iter().map(Config::mix_in_node_name).collect();
    let pins = cfg
        .mixes
        .iter()
        .filter(|m| m.kind == model::MixKind::Monitor)
        .map(|m| (Config::mix_out_node_name(m), m.output.clone()))
        .collect();
    let graph = Rc::new(Graph::start(&core, wants, bind, cfg.router(), pins, cfg.output_fallback, app_meter.peaks.clone())?);

    for m in &cfg.mixes {
        eprintln!("pipemicd: mix '{}' -> {}", m.label, Config::mix_out_node_name(m));
    }

    // Control requests arrive on the main loop.
    let cfg = Rc::new(RefCell::new(cfg));
    let (tx, rx) = pw::channel::channel::<Job>();
    let sock = control::serve(tx, mic::Mic::start(), meter.levels.clone())?;
    let _rx = {
        let cfg = cfg.clone();
        let graph = graph.clone();
        rx.attach(main_loop.loop_(), move |(req, reply)| {
            let mut c = cfg.borrow_mut();
            let before = c.clone();
            let r = apply(&mut c, &graph, req).and_then(|o| {
                for &mi in &o.gains {
                    graph.set_controls(&Config::mix_in_node_name(&c.mixes[mi]), &c.mix_controls(mi))?;
                }
                if o.rules {
                    graph.set_router(c.router());
                }
                if o.fallback {
                    graph.set_output_fallback(c.output_fallback);
                }
                if let Some(mi) = o.output {
                    graph.set_pin(&Config::mix_out_node_name(&c.mixes[mi]), c.mixes[mi].output.clone());
                }
                if o.save {
                    c.save(&cfg_path)?;
                }
                Ok(o.apps.then_some(o.installed))
            });
            let _ = reply.send(match r {
                Ok(apps) => {
                    let mut rep = Reply { ok: true, config: Some(c.clone()), ..Default::default() };
                    if let Some(installed) = apps {
                        rep.apps = Some(app_list(&c, &graph, installed));
                        rep.outputs = Some(serde_json::Value::Array(
                            graph
                                .outputs()
                                .into_iter()
                                .map(|(n, d)| serde_json::json!({"name": n, "description": d}))
                                .collect(),
                        ));
                    }
                    rep
                }
                Err(e) => {
                    *c = before;
                    Reply::err(format!("{e:#}"))
                }
            });
        })
    };

    // Re-create links PipeWire failed or dropped (see Graph::heal).
    let heal_timer = {
        let graph = graph.clone();
        main_loop.loop_().add_timer(move |_| graph.heal())
    };
    heal_timer.update_timer(Some(std::time::Duration::from_secs(2)), Some(std::time::Duration::from_secs(2))).into_result()?;

    eprintln!("pipemicd: control socket {}; running (Ctrl-C to stop)", sock.display());
    main_loop.run();
    let _ = std::fs::remove_file(&sock);
    if lost.get() {
        bail!("PipeWire connection lost");
    }
    eprintln!("pipemicd: stopping");
    graph.release_streams();
    roundtrip(&main_loop, &core);
    drop(meter);
    drop(app_meter);
    drop(sinks);
    drop(signals);
    Ok(())
}
