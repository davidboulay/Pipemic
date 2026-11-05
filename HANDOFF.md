# Handoff: Pipemic (Wave Link alternative for Linux, Rust)

> 2026-11-04: renamed to **Pipemic** (binaries `pipemic`, `pipemicd`, `pipemicctl`; crates under `crates/pipemic*`; PipeWire node names `pipemic.*`; config `~/.config/pipemic/`; repo `davidboulay/pipemic`). Older notes below still use the old names.

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
  - Mixes: one in-process filter-chain each (two builtin `mixer` nodes L/R, max 8 channels). Outgoing is an `Audio/Source` (`wavelink.mix.stream`, shown as "Wave Link Outgoing"). (2026-11-04: Chat mix dropped at the user's request; two mixes only, "my side" and "the other side".) Personal is a playback stream that WirePlumber routes to the default output.
  - A registry-driven linker wires every channel into every mix and relinks when ports reappear.
  - Config: `$XDG_CONFIG_HOME/wavelink/mixer.toml` (written with defaults on first run, saved on every change). Volumes in dB (-60..0) at three levels: channel, mix master, and route (channel × mix), plus a mute at each level. Defaults: mic muted in Personal, voice chat muted in Chat.
  - Control: JSON lines on `$XDG_RUNTIME_DIR/wavelinkd.sock` (see `control.rs`). CLI: `wavectl mix`, `wavectl mix channel music -6`, `wavectl mix master stream mute`, `wavectl mix route stream music -20`.
  - Feedback guard: any link from a `wavelink.mix.*` output into a `wavelink.*` node is destroyed (tested with a manual pw-link).
  - Verified with a 440 Hz tone: route -20 dB → 1/10 amplitude, plus channel -6 dB → 1/20, master mute → 0.
  - App routing: `[[rule]] channel = "music" apps = ["spotify"]` in mixer.toml. Matches application.name / process.binary / node.name / portal app id, case-insensitive with `*` wildcard. The daemon sets `target.object` (Spa:Id serial) in the `default` metadata, once per stream, so moving an app by hand afterwards still sticks. On exit it clears those targets; verified that Spotify returned to the CalDigit output and that WirePlumber saved no target. Defaults: Spotify → Music; Google Chrome / chrome / google-chrome* → Browser.
- Open bug: during the user's own test, Spotify was moved to Music and then ended up back on the default output while the daemon was still running. `target.node = -1` was found in the metadata. The cause isn't identified yet; KDE or pipewire-pulse are suspects.
- Wave:3 quirk (seen twice on 2026-11-03): after a replug or profile change, the source can sit idle (rate 0) or deliver zeros. Toggling the card profile off and on fixes it. Separately, both the Wave:3 card and the laptop card were found switched "Off" (cause unknown; not our code).
- systemd user unit `systemd/wavelinkd.service` (WantedBy=pipewire.service, Restart=on-failure); installed in ~/.config/systemd/user, binaries in ~/.local/bin.
- Mic in the daemon (`mic.rs`): a USB thread polls msg 0 every 200 ms (pad mute shows up live), applies `{"cmd":"mic",...}` requests and reconnects after a replug. wave-proto now claims interface 3 per operation (with retry on Busy), so `wavectl get/set` work alongside the daemon. CLI: `wavectl mix mic gain 30 | mute | unmute | lowcut on | clipguard off`. Verified on the device: gain 38→37.5→38 and lowcut on/off, both read back byte-exact. User-verified 2026-11-03: CLI controls, replug reconnect, pad mute shown live.
- UI: `crates/wavelink-ui` (binary `wavelink`, iced 0.14). The user chose a standalone Rust app over libcosmic or a Plasma widget, for the widest distro coverage. It shows a channel × mix fader grid with mutes, mix volumes and a Wave:3 panel (gain, low cut, clipguard), and polls status 4×/s. User-verified working. It isn't installed yet (no .desktop entry).
- Next, in the agreed order: (1) install `wavelink` + .desktop entry + icon; (2) design pass: vertical strips like Wave Link, live level meters; (3) mic noise suppression (RNNoise / DeepFilterNet) replacing Voice Focus; (4) Stream Deck / WebSocket API (needs Mac RE first).

## Still open on the Mac side
- **Firmware update (needed for an "Update firmware" button).** Run one Wave:3 firmware update in Wave Link while capturing:
  1. the URL Wave Link downloads the firmware (and its version list) from, plus the downloaded file itself;
  2. the file format: is it a plain DFU image that `dfu-util` accepts, or wrapped or signed?
  3. the USB traffic: how Wave Link switches the mic into update mode (a vendor message on interface 3, or a DFU_DETACH on interface 4), the DFU transfer and the reboot.
  Interface 4 is a standard DFU interface (class FE/01); the bootloader is 0.3.7. The Linux app must fetch the firmware from Elgato at update time and never ship it.
- **Upside-down mode.** Toggle it in Wave Link (Settings → General) while watching msg 0, and confirm it is byte 14 `leds_flip`. If it is, the Linux UI can offer it.
- **Other Elgato mics (Wave:1, Wave Neo, Wave XLR, Wave:3 MK.2).** The parameter paths are in `docs/waveapi-all-param-paths.tsv`, but each model's USB IDs and message layout still need capturing before the app can control it. The app supports the Wave:3 only until then.
- The WebSocket API on 127.0.0.1:1884 rejects plain handshakes (HTTP 400). The requirement is still to be decompiled with REA. This isn't needed until the daemon is built.
- The LED bytes (13–15) haven't been confirmed.

## Firmware update (2026-11-04, Mac)
- Reverse-engineered the update flow statically: see `docs/wave3-dfu.md`. It's XMOS-style DFU on the DFU interface: 0xF2 into DFU mode, 0xF4 to select image 0, standard DNLOAD/GETSTATUS with the suffix stripped, then manifest. Image: `edge.elgato.com/.../wave_3_firmware_1_2_2.dfu`, the same version the device already runs.
- Next on Linux: run `lsusb -v` to get the DFU descriptor, find the DFU-mode PID after 0xF2, and check whether it leaves DFU mode on its own. Don't attempt a download until those are known.

## Mic mute sync (2026-11-05, Linux)
- Bug: Meet said "Microphone muted by system". The Wave:3 PipeWire source was muted while the mic itself wasn't (no red LED); the daemon never synced the two. Earlier, the same mic delivered pure zeros after a replug (known quirk, fixed by a profile off/on).
- Fix (`mic.rs`, `MuteSync`): every 1 s the daemon compares the hardware mute with the source's PipeWire mute (via `pactl`). On a new node or daemon start the hardware wins; if only the PipeWire side changed since the last sync (desktop mic-mute key), it's written to the mic so the LED follows.
- Verified: daemon stopped, source muted in PipeWire, daemon started → unmuted within ~4 s. Desktop→mic direction is covered by unit tests only; not yet tried on the device.
- Open: auto-detect the "zeros after replug" quirk and toggle the card profile.

## Tray icon (2026-11-05, Linux)
- `pipemic` is now an iced *daemon*: closing the window leaves it in the tray (ksni StatusNotifierItem, `crates/pipemic/src/tray.rs`). Tray: click = open, menu = Open / Mute mic / Quit. Icon `pipemic-muted` (red, slashed) while the Wave:3 is muted.
- Single instance via `$XDG_RUNTIME_DIR/pipemic-ui.sock`: a second launch asks the running one to show its window and exits.
- Autostart runs `pipemic --background` (tray only); old entries are rewritten on start.
- No tray host (e.g. GNOME without the AppIndicator extension): closing the window quits, as before.
- ksni's blocking API panics inside iced's tokio runtime, so the tray runs on its own thread and gets state over a channel.
- Verified: tray registered with KDE, icon/tooltip follow mic state, second launch doesn't duplicate. Not yet tried by the user: close → tray → reopen.
