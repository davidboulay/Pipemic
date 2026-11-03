# Handoff: Wave Link → Rust/Linux (from the MacBook session, 2026-11-03)

Goal: a Linux reimplementation of Elgato Wave Link 3, written in Rust.

## Read these first
- `docs/wave3-protocol.md`: the Wave:3 USB control protocol. It's decoded, and offsets 0–12 are confirmed on the device.
- `docs/wave3-schema-api5.3.txt`: every Wave:3 parameter with its range and step.
- `docs/wavelink3-architecture.md`: the mixer data model (`config.json`), the engine design, the WebSocket API and how each part maps to PipeWire.
- `docs/waveapi-all-param-paths.tsv`: parameter paths for all Wave models (XLR, Dock, Neo, MK2).
- `tools/*.c`: libusb probes for macOS. They should build on Linux too, after changing the include path.

## Constraints
- **REA and Ghidra stay on the MacBook only.** Don't install or sync them here. Any further binary analysis happens on the Mac.
- The Wave:3 is VID `0x0fd9`, PID `0x0070`. On Linux, claim **interface 3** (vendor, no kernel driver) and put ifnum = 3 in wIndex (`0x3303`). Interface 0 belongs to `snd-usb-audio`.
- Writes have to be read-modify-write of the whole message. Config bytes 2–3 are unknown and must be written back unchanged.

## Next tasks (Phase 3)
1. Create a Cargo workspace in this folder with:
   - `wave-proto`: a library using `rusb`, with typed `Config` / `Status` / `Version` messages and encode/decode for 8.8 fixed point.
   - `wavectl`: a command-line tool with `wavectl get` / `wavectl set gain 21.5` / `wavectl watch`.
2. Add a udev rule (`/etc/udev/rules.d/70-elgato-wave.rules`) so a non-root user can open the device.
3. Test with the Wave:3 plugged into this machine:
   - Read the version (it should report API 5.3, firmware 1.2.2, serial SERIAL000001).
   - Then read config and confirm that `snd-usb-audio` keeps streaming while interface 3 is claimed.
   - Only then try a write (gain, then put it back).
4. Afterwards: a `wavelinkd` PipeWire daemon (channels/mixes from `config.json`), and a libcosmic UI and applet.

## Phase 3 status (2026-11-03, Linux)
- Done: Cargo workspace (`crates/wave-proto`, `crates/wavectl`), `udev/70-elgato-wave.rules` (installed in /etc).
- Verified on the device: version read, config read, streaming unaffected by the claim, gain write + restore read back exactly. Details in `docs/wave3-protocol.md` § Still to verify.
- Once, right after the rule was installed, the mic captured pure zeros (also with wavectl not running); a replug fixed it. Cause unknown; check this if it happens again.
- msg 1 decoded (raw touch signal; a tap toggles config byte 4).
- LED test: byte 13 = 1 is stored but has no visible effect (two tries; restored to 0). Bytes 13–15 stay unconfirmed and aren't needed.
- Repo: github.com/davidboulay/elgato-wave-link-linux (private).
- `wavelinkd` slice 2 (verified 2026-11-03):
  - Channels: virtual sinks `wavelink.channel.{system,music,browser,voicechat}` plus the Wave:3 mic (matched by node.name prefix).
  - Mixes: one in-process filter-chain each (two builtin `mixer` nodes L/R, max 8 channels). Stream and Chat are `Audio/Source` (`wavelink.mix.stream`, `wavelink.mix.chat`). Personal is a playback stream that WirePlumber routes to the default output.
  - A registry-driven linker wires every channel into every mix and relinks when ports reappear.
  - Config: `$XDG_CONFIG_HOME/wavelink/mixer.toml` (written with defaults on first run, saved on every change). Volumes in dB (-60..0) at three levels: channel, mix master, and route (channel × mix), plus a mute at each level. Defaults: mic muted in Personal, voice chat muted in Chat.
  - Control: JSON lines on `$XDG_RUNTIME_DIR/wavelinkd.sock` (see `control.rs`). CLI: `wavectl mix`, `wavectl mix channel music -6`, `wavectl mix master stream mute`, `wavectl mix route stream music -20`.
  - Feedback guard: any link from a `wavelink.mix.*` output into a `wavelink.*` node is destroyed (tested with a manual pw-link).
  - Verified with a 440 Hz tone: route -20 dB → 1/10 amplitude, plus channel -6 dB → 1/20, master mute → 0.
  - App routing: `[[rule]] channel = "music" apps = ["spotify"]` in mixer.toml. Matches application.name / process.binary / node.name / portal app id, case-insensitive with `*` wildcard. The daemon sets `target.object` (Spa:Id serial) in the `default` metadata, once per stream, so moving an app by hand afterwards still sticks. On exit it clears those targets; verified that Spotify returned to the CalDigit output and that WirePlumber saved no target. Defaults: Spotify → Music; Google Chrome / chrome / google-chrome* → Browser.
- Open bug: during the user's own test, Spotify was moved to Music and then ended up back on the default output while the daemon was still running. `target.node = -1` was found in the metadata. The cause isn't identified yet; KDE or pipewire-pulse are suspects.
- Wave:3 quirk (seen twice on 2026-11-03): after a replug or profile change, the source can sit idle (rate 0) or deliver zeros. Toggling the card profile off and on fixes it. Separately, both the Wave:3 card and the laptop card were found switched "Off" (cause unknown; not our code).
- Next: systemd user unit + `wavectl mix` showing rules; then wave-proto inside the daemon (mic mute/gain in the mixer).

## Still open on the Mac side
- The WebSocket API on 127.0.0.1:1884 rejects plain handshakes (HTTP 400). The requirement is still to be decompiled with REA. This isn't needed until the daemon is built.
- The LED bytes (13–15) haven't been confirmed.
