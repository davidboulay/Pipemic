//! wavelinkd: Wave Link mixer daemon for PipeWire.
//!
//! - Each app channel is a virtual sink ("Wave Link Music") apps play into.
//! - Each mix is a filter-chain (two builtin mixers, one gain per channel)
//!   loaded in this process. Stream/Chat publish an `Audio/Source`;
//!   Personal plays to the default output.
//! - A registry-driven linker wires channel outputs to every mix input.
//!
//! Everything is owned by this process and disappears when it exits.

mod links;
mod model;

use anyhow::{bail, Context, Result};
use links::{Linker, NodeMatch, Want};
use model::{Model, Source};
use pipewire as pw;
use pw::loop_::Signal;
use pw::properties::properties;
use std::ffi::CString;

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

fn main() -> Result<()> {
    pw::init();
    let model = Model::default_layout();

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
    for ch in &model.channels {
        if let Source::Virtual = ch.source {
            let sink = core
                .create_object::<pw::node::Node>(
                    "adapter",
                    &properties! {
                        "factory.name" => "support.null-audio-sink",
                        "node.name" => Model::sink_node_name(ch),
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
    }

    // Mixes: one filter-chain each, running inside this process.
    for mi in 0..model.mixes.len() {
        load_module(&context, "libpipewire-module-filter-chain", &model.filter_chain_args(mi))
            .with_context(|| format!("mix {}", model.mixes[mi].id))?;
    }

    // Wiring: every channel into every mix (gain decides what's audible).
    let mut wants = Vec::new();
    for m in &model.mixes {
        for (i, ch) in model.channels.iter().enumerate() {
            for (side, pos) in ["FL", "FR"].iter().enumerate() {
                let (out_node, out_ports) = match ch.source {
                    Source::Virtual => (NodeMatch::Exact(Model::sink_node_name(ch)), vec![format!("monitor_{pos}")]),
                    Source::Device { node_prefix } => (
                        NodeMatch::Prefix(node_prefix.to_owned()),
                        vec!["capture_MONO".to_owned(), format!("capture_{pos}")],
                    ),
                };
                wants.push(Want {
                    out_node,
                    out_ports,
                    in_node: Model::mix_in_node_name(m),
                    in_port: Model::mix_in_port(i, side),
                });
            }
        }
    }
    let _linker = Linker::start(&core, wants)?;

    for m in &model.mixes {
        eprintln!("wavelinkd: mix '{}' -> {}", m.label, Model::mix_out_node_name(m));
    }
    eprintln!("wavelinkd: {} channels; running (Ctrl-C to stop)", model.channels.len());
    main_loop.run();
    eprintln!("wavelinkd: stopping");
    drop(sinks);
    drop(signals);
    Ok(())
}
