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
- `wavelinkd` slice 1: creates `wavelink.mix.{personal,stream,chat}` virtual sources (Audio/Source/Virtual, null-audio-sink); they're removed on exit. Next slice: channels + channel×mix volumes.

## Still open on the Mac side
- The WebSocket API on 127.0.0.1:1884 rejects plain handshakes (HTTP 400). The requirement is still to be decompiled with REA. This isn't needed until the daemon is built.
- The LED bytes (13–15) haven't been confirmed.
