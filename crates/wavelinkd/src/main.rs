//! wavelinkd: Wave Link mixer daemon for PipeWire.
//!
//! Slice 1: publish the output mixes as virtual sources (what OBS / Zoom
//! record from). The nodes are owned by this process and disappear when it
//! exits. Channels, crosspoints and app routing come next.

use anyhow::{Context, Result};
use pipewire as pw;
use pw::loop_::Signal;
use pw::properties::properties;

/// Mixes as in Wave Link 3's default `config.json`.
const MIXES: &[(&str, &str)] = &[
    ("personal", "Personal Mix"),
    ("stream", "Stream Mix"),
    ("chat", "Chat Mix"),
];

fn main() -> Result<()> {
    pw::init();
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

    let mut nodes = Vec::new();
    for (id, label) in MIXES {
        let node = core
            .create_object::<pw::node::Node>(
                "adapter",
                &properties! {
                    "factory.name" => "support.null-audio-sink",
                    "node.name" => format!("wavelink.mix.{id}"),
                    "node.description" => format!("Wave Link {label}"),
                    "media.class" => "Audio/Source/Virtual",
                    "audio.position" => "FL,FR",
                    "monitor.channel-volumes" => "true",
                    "object.linger" => "false",
                },
            )
            .with_context(|| format!("creating mix node {id}"))?;
        nodes.push(node);
        eprintln!("wavelinkd: mix '{label}' -> wavelink.mix.{id}");
    }

    eprintln!("wavelinkd: running (Ctrl-C to stop)");
    main_loop.run();
    eprintln!("wavelinkd: stopping, removing mixes");
    drop(nodes);
    drop(signals);
    Ok(())
}
