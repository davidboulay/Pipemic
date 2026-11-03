//! wavelinkd: Wave Link mixer daemon for PipeWire.
//!
//! - Each app channel is a virtual sink ("Wave Link Music") apps play into.
//! - Each mix is a filter-chain (two builtin mixers, one gain per channel)
//!   loaded in this process. Source mixes publish an `Audio/Source`;
//!   the monitor mix plays to the default output.
//! - A registry-driven graph keeper wires channels into mixes, pushes gain
//!   changes and cuts feedback links.
//! - Config: `$XDG_CONFIG_HOME/wavelink/mixer.toml`; control socket:
//!   `$XDG_RUNTIME_DIR/wavelinkd.sock` (see control.rs).
//!
//! Everything in PipeWire is owned by this process and disappears on exit.

mod control;
mod links;
mod model;

use anyhow::{bail, Context, Result};
use control::{Job, Reply, Request};
use links::{AppRule, Graph, NodeMatch, Want};
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

/// Apply one control request to the config; returns the mixes whose gains changed.
fn apply(cfg: &mut Config, req: Request) -> Result<Vec<usize>> {
    let all: Vec<usize> = (0..cfg.mixes.len()).collect();
    Ok(match req {
        Request::Status => vec![],
        Request::Channel { channel, volume, mute } => {
            let ci = cfg.channel_index(&channel)?;
            if let Some(v) = volume {
                cfg.channels[ci].volume = check_db(v)?;
            }
            if let Some(m) = mute {
                cfg.channels[ci].mute = m;
            }
            all
        }
        Request::Mix { mix, volume, mute } => {
            let mi = cfg.mix_index(&mix)?;
            if let Some(v) = volume {
                cfg.mixes[mi].volume = check_db(v)?;
            }
            if let Some(m) = mute {
                cfg.mixes[mi].mute = m;
            }
            vec![mi]
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
            vec![mi]
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

    // Channels: one virtual sink per app channel.
    let mut sinks = Vec::new();
    for ch in cfg.channels.iter().filter(|c| c.device.is_none()) {
        let sink = core
            .create_object::<pw::node::Node>(
                "adapter",
                &properties! {
                    "factory.name" => "support.null-audio-sink",
                    "node.name" => Config::sink_node_name(ch),
                    "node.description" => format!("Wave Link {}", ch.label),
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

    // Wiring: every channel into every mix (gain decides what's audible).
    let mut wants = Vec::new();
    for m in &cfg.mixes {
        for (i, ch) in cfg.channels.iter().enumerate() {
            for (side, pos) in ["FL", "FR"].iter().enumerate() {
                let (out_node, out_ports) = match &ch.device {
                    None => (NodeMatch::Exact(Config::sink_node_name(ch)), vec![format!("monitor_{pos}")]),
                    Some(prefix) => {
                        (NodeMatch::Prefix(prefix.clone()), vec!["capture_MONO".to_owned(), format!("capture_{pos}")])
                    }
                };
                wants.push(Want { out_node, out_ports, in_node: Config::mix_in_node_name(m), in_port: Config::mix_in_port(i, side) });
            }
        }
    }
    let bind: Vec<String> = cfg.mixes.iter().map(Config::mix_in_node_name).collect();
    let rules: Vec<AppRule> = cfg
        .rules
        .iter()
        .map(|r| {
            let ch = &cfg.channels[cfg.channel_index(&r.channel).expect("validated")];
            AppRule { sink: Config::sink_node_name(ch), apps: r.apps.clone() }
        })
        .collect();
    let graph = Rc::new(Graph::start(&core, wants, bind, rules)?);

    for m in &cfg.mixes {
        eprintln!("wavelinkd: mix '{}' -> {}", m.label, Config::mix_out_node_name(m));
    }

    // Control requests arrive on the main loop.
    let cfg = Rc::new(RefCell::new(cfg));
    let (tx, rx) = pw::channel::channel::<Job>();
    let sock = control::serve(tx)?;
    let _rx = {
        let cfg = cfg.clone();
        let graph = graph.clone();
        rx.attach(main_loop.loop_(), move |(req, reply)| {
            let mut c = cfg.borrow_mut();
            let before = c.clone();
            let r = apply(&mut c, req).and_then(|changed| {
                for &mi in &changed {
                    graph.set_controls(&Config::mix_in_node_name(&c.mixes[mi]), &c.mix_controls(mi))?;
                }
                if !changed.is_empty() {
                    c.save(&cfg_path)?;
                }
                Ok(())
            });
            let _ = reply.send(match r {
                Ok(()) => Reply { ok: true, error: None, config: Some(c.clone()) },
                Err(e) => {
                    *c = before;
                    Reply { ok: false, error: Some(format!("{e:#}")), config: None }
                }
            });
        })
    };

    eprintln!("wavelinkd: control socket {}; running (Ctrl-C to stop)", sock.display());
    main_loop.run();
    eprintln!("wavelinkd: stopping");
    graph.release_streams();
    roundtrip(&main_loop, &core);
    let _ = std::fs::remove_file(&sock);
    drop(sinks);
    drop(signals);
    Ok(())
}
