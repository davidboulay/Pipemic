# Handoff: laptop speaker tuning goes silent while pipemicd runs (2026-11-07)

Focus for the next session: **find what pipemicd does that silences the user's speaker tuning filter, and fix it.**
Project background is in `AGENTS.md` and `HANDOFF.md`. Don't repeat that here.

## State right now
- **`pipemicd` is stopped** (`systemctl --user stop pipemicd`), on purpose, so the user has sound: apps play
  straight to the default output. Start it again only when testing or after a fix: `systemctl --user start pipemicd`.
- The laptop is undocked, with no CalDigit and no Wave:3. The default sink is `xps_speaker_tuning`, the user's own filter.
- `main` is at v0.3.2 (released). The working tree is clean apart from this file.
- Each restart of the PipeWire stack makes Chrome lose its audio until it is **fully** quit (the WhatsApp
  `--app` window keeps Chrome's main process alive). Tell the user before restarting PipeWire.
- **Don't flip pipemicd on and off while the user is listening.** They hear it as sound cutting in and out.
  Warn them first, or test while they're not using audio.

## The user's speaker tuning (not part of Pipemic)
- `xps-speaker-tuning.service` (user unit) runs `pipewire -c xps-speaker-tuning.conf`. Config:
  `~/.config/pipewire/xps-speaker-tuning.conf` plus `~/.config/pipewire/xps-speaker-tuning.conf.d/90-tuning.conf`.
  It's a filter-chain made of 13 biquads, a `linear` trim and an LSP `limiter_stereo` (LV2). Sink `xps_speaker_tuning`
  (`priority.session/driver = 713`). Output stream `xps_speaker_tuning_output` (`node.passive = true`,
  `target.object` = raw `…HiFi__Speaker__sink`, `node.dont-move`, `node.dont-fallback`, `node.linger`).
- The user's WirePlumber script `~/.local/share/wireplumber/scripts/audio-default-policy.lua` ranks
  BT > CalDigit > `xps_speaker_tuning` > raw speaker. It only picks defaults and never mutes or links anything.
- On the dock the CalDigit (plain hardware) is the default, so this filter isn't in the path. That's why the bug
  only showed up today.

## Verified (measured, not guessed)
Method: play a short 880 Hz beep with `pw-play --target <node>` while recording the raw speaker sink's monitor
(`pw-record --target alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink -P stream.capture.sink=true`)
and compare peak values. The user confirmed by ear that the raw speaker works and that the tuning filter and Pipemic were silent.

| Situation | Peak at speaker |
|---|---|
| Beep straight to the raw speaker sink | ~7000 (works) |
| Beep to `xps_speaker_tuning`, **pipemicd running**, xps is the default | **0** |
| Same, **pipemicd stopped** | ~15000 (works) |
| pipemicd running, but raw speaker made default (Personal then plays to the raw speaker) | xps ~4800 (works) |
| pipemicd stopped, a `pw-loopback` playing into xps (stand-in for the Personal mix) | works |
| pipemicd stopped, a loopback with `node.force-quantum=1024` in the speaker's graph | works |
| pipemicd v0.3.0, v0.3.1 and v0.3.2 built and run by hand | **all 0**: not a regression from the 2026-11-06 changes |

The same graph under other node names (`bis`) worked, and silence only happened while it was the default sink.
Ruled out: the LSP limiter, `node.passive`, `priority.driver`, the `module-rt` host config, every biquad type,
the trim, and saved WirePlumber stream volumes (all 1.0, unmuted).

While pipemicd runs, `pw-top` shows both `xps_speaker_tuning` nodes as `R` followers of the speaker driver with
**WAIT/BUSY 0.0**, i.e. never actually processed. All Pipemic nodes are also followers of the speaker driver.

## Leading hypothesis
The silence needs **pipemicd running and the Personal mix linked into `xps_speaker_tuning`** (xps is the
default, so Personal follows it). A plain loopback linked into xps does not cause it. So suspect something in how
the Personal mix's filter-chain playback stream (in-process `libpipewire-module-filter-chain`, see
`Config::filter_chain_args` in `crates/pipemicd/src/model.rs`) combines with a second filter-chain downstream.
For example, two filter-chain link-groups chained back to back could leave the downstream one unscheduled. Also
remember WirePlumber refused to link Personal → xps itself ("No input node for filter-chain-…") until the saved
stream target took over.

## Suggested next experiments (in order, cheapest first)
1. With pipemicd running and the **raw speaker as default**, hand-link Personal into xps
   (`pw-link pipemic.mix.personal:output_FL xps_speaker_tuning:playback_FL`, and the same for FR), then beep into xps.
   Silent = the Personal → xps link alone triggers it.
2. Reverse it: xps default, pipemicd running, **unlink** Personal from xps (`pw-link -d …`) and beep. Sound back = confirmed.
3. If confirmed, compare the Personal playback props with a working `pw-loopback`'s (`pw-dump`, look at
   `node.group`, `node.link-group`, `node.passive`, `node.latency`, `media.class`), and try running xps as an
   in-server module instead, to see whether two filter-chain processes in series are the trigger.
4. Possible fixes depending on the cause: point Personal at the raw device when the default is itself a
   filter-chain, adjust the mix's playback props, or report upstream (PipeWire 1.6.10).

## Related and still open (in `HANDOFF.md`)
- pipemicd SIGSEGV inside `libspa-filter-graph` (builtin plugin), seen twice on 2026-11-06. Possibly the same
  family of filter-chain issues.
- WirePlumber was restarted twice at 13:29 today by something other than us. That again left every ALSA node
  unconfigured (0 ports, `PortConfig none`). `systemctl --user restart pipewire pipewire-pulse wireplumber`, then
  `systemctl --user restart xps-speaker-tuning`, fixed it. Who restarts WirePlumber is unknown.
- `~/.local/state/wireplumber/stream-properties` now has `Pipemic Personal → target xps_speaker_tuning` saved
  (it came from a manual `pw-metadata` test). Harmless, but it explains why WirePlumber links Personal to xps.

## Suggested skills
- `run`: launch Pipemic and drive it to confirm a fix in the real app.
- `code-review`: review the pipemicd change before opening the PR.
- After the work: update `HANDOFF.md` (dated bullets), reinstall with `./packaging/install-user.sh`, and restart
  `pipemicd` so the user isn't left with it stopped.
