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

## Marketplace add-ons research (2026-11-05, Mac)
- Add-ons are standard plugins: VST3 on Windows, Audio Units (`.pkg`) on macOS, nothing tied to Wave Link. Pip-Boy and Equalizer are JUCE 7 with no licence check. EQ "presets" are plain XML for the Elgato EQ (8 bands; linear gain 0.25–4; Hz; Q).
- Public catalogue API: `mp-gateway.elgato.com/products?application=wave-link` (53 products, 12 free plugins without a licence).
- Details: `docs/addons-research.md`, `docs/addon-pipboy-dsp.md`. Linux next steps: `docs/addons-handoff.md` (yabridge prototype, measure the EQ type mapping, then design the effect rack).

## Mic missing from Outgoing after replug (2026-11-05, Linux)
- Seen: after a Wave:3 replug at 18:34 the mic reached Personal only; no Outgoing, no mic meter. PipeWire had
  hit its 1024-fd limit (`Too many open files`, 18:17–18:34) and WirePlumber restarted, so link creation failed.
- Fixed (PR #6): `Graph::heal` re-creates any wanted link not live after 3 s (2 s timer). Verified after reinstall.
- Still open: the fd pressure. ~90 ALSA-plugin streams stuck with links in `init` (Betterbird 51, plasmashell 14,
  powerdevil 12, polkit agent 12) never close. Not Pipemic's streams, but worth watching.

## Robot voice on calls (2026-11-05, Linux)
- Cause: linked straight into the mixes, the Wave:3 followed the CalDigit output clock and XRUNed ~47/s
  (`/proc/asound/card1/pcm0c/sub0/status` flipping RUNNING/XRUN). With pipemicd stopped: 0 XRUNs.
  The `54-wave3-never-driver.conf` headroom tweak was not enough.
- Fix (PR #7): mic goes through a loopback bridge `pipemic.dev.mic` (own clock, resampled). Verified: 0 new XRUNs.
- Also seen: after replug the Wave:3 sent zeros until the card profile was toggled off/on (known quirk).

## Add-ons on Linux (2026-11-06, Linux)
- PipeWire 1.6 filter-chain: builtin, LADSPA, LV2, ebur128, sofa. No VST3, so plugins need our own host.
- yabridge 5.1.1 + wine-staging 11.21 load Elgato EQ and Pip-Boy (headless). Wine's `wineusb` was disabled in
  `~/.wine` because it enumerates the Wave:3 and left orphan `winedevice.exe` processes.
- Elgato EQ measured: types are in UI order (0 HP … 4 LP), every type is an RBJ biquad (HP/LP 12 dB/oct), gain is
  linear, `Visible` doesn't disable a band. "Old Radio" matches an RBJ cascade within 0.05 dB. EQ preset import is unblocked.
- Pip-Boy: default preset and parameter list confirmed; presets 1–8 and reverb internals still unverified
  (see `docs/addon-pipboy-dsp.md` §4).
- Mic unaffected (checked `wpctl status` after each Wine run).
- Decision (user, 2026-11-06): no Wine. Pipemic supports only add-ons that run natively: EQ presets now; single
  "default"-variant plugins if their bundle turns out to hold a Linux build (unknown, needs one download, e.g.
  Simple Noise Gate). Mac add-ons are Audio Units and can't run on Linux. The marketplace site has no OS filter,
  so Pipemic will list compatible add-ons itself (public catalogue API) with a "Get" link to each product page.
- Channel EQ engine (branch `addons-eq`): every channel now passes through `pipemic.fx.<id>` (filter-chain, 8
  `bq_raw` biquads per side, 48 kHz) before mixes and meters. Pipemic computes RBJ coefficients and pushes them live.
  `pipemicctl mix eq <channel> import <preset.xml> | on | off | clear`.
- Gotcha found live: `bq_raw` starts as an all-zero filter (silence) unless the coefficients are also given in
  `config = { coefficients = [...] }`; it only re-reads controls when they change. A first build without it muted
  every channel until the old daemon was restored. Fixed and verified.
- Verified on the device: tones through Other match the expected RBJ curves within 0.01 dB (test EQ, imported
  "Old Radio", EQ off = 0.0 dB); mic flows through `pipemic.fx.mic` into Outgoing. Restarting pipemicd makes Spotify
  pause (its sink disappears for a moment).
- UI (same branch): each strip has an effects chip ("Add effect" / preset name, green when on). It opens
  "Effects on <channel>": the EQ (on/off switch, bands, Remove), EQ presets found in Downloads (+ one subfolder
  level) with "Use", drag-and-drop of a preset file, and the marketplace's EQ presets (`wave_catalog::marketplace`)
  with "Get" opening the product page. Not yet tried by the user.
- Dev aid: `PIPEMIC_VIEW=fx:<channel>`. To screenshot beside a running Pipemic, use a short private
  `XDG_RUNTIME_DIR` with a symlink to `pipemicd.sock` and an absolute `WAYLAND_DISPLAY` (single-instance socket).
- Follow-up (user feedback 2026-11-06): no login/auto-download (Elgato has no third-party OAuth; posing as Wave
  Link was declined). Instead "Get" remembers the channel and applies the preset as soon as its file lands in
  Downloads (matched by name; 15 min window). Downloaded presets can be moved to the trash (`gio trash`, else
  `kioclient`). Remove button dropped: the switch bypasses, picking another preset replaces. Marketplace thumbnails
  (2:1 PNG, cached in `~/.cache/pipemic/addons/`). Catalogue refetched hourly and when the sheet opens after 30 min;
  version from the file name (`Name-1.0.0_xxx.xml`) vs `latest_version_number` shows "Update".
  Import now keeps only bands that change the sound (bypassed and 0 dB peak/shelf slots dropped).
- "Open with Wave Link" on the marketplace sends `wavelink://presets/install/<variant id>` (captured 2026-11-06;
  no token, the file still needs the logged-in Download button). Pipemic now handles `wavelink://` and
  `wavelink-v2://` (`Exec=pipemic %u`, `xdg-mime default` in install-user.sh; a running instance gets
  `open <url>` on its single-instance socket). It shows a popup: "Getting <name>… click Download", then
  "<name> was added", closed only by OK. Added presets are copied to `~/.local/share/pipemic/presets/`
  (the library, listed first under YOUR PRESETS); Delete trashes every copy (library + Downloads).
  Plugin links (`vst/...`) get "Pipemic can use EQ presets only".
- Wayland: winit's focus_window is a no-op, so a marketplace link reopens the window (closed + new) unless it has focus; new windows come to the front on KDE.
- Probing Elgato's gateway for an unauthenticated download URL was stopped (treated as getting around their
  login). Don't retry; downloads stay with the user's browser.
- Elgato sign-in (user chose "option 2", 2026-11-06): `pipemic-login` (wry/WebKitGTK window, own profile in
  `~/.local/share/pipemic/webview`; private mode dropped the OIDC handshake cookies) shows Elgato's own sign-in
  (Keycloak realm `mp`, Google SSO works) and saves the marketplace cookies (`kc_user`, `__Host-kc_at`,
  `__Secure-kc_rt` (path-scoped, so read with `cookies()` not `cookies_for_url`)) to
  `~/.local/share/pipemic/elgato-session.json` (0600). Downloads do what the website's Download button does:
  `GET marketplace.elgato.com/api/proxy/purchase-link/<variant>` with the cookies -> `{direct_link (signed
  mp-cdn URL), product_name, version_number, file_size, metadata.{win,mac}_destination_path, deep_link}`, then GET
  `direct_link`. Public `mp-gateway.elgato.com/allitems?ids=<variant>` gives name/thumbnail without sign-in.
  Set-Cookie renewals are merged; 401/403 signs out. Verified: a `wavelink://` link installed "EQ for Female
  Vocal Range" into the library with no browser Download click.
- Session lifetime: `__Host-kc_at` 15 min, `__Secure-kc_rt` (path `/api`) and `kc_user` 30 days, Keycloak SSO
  (`KEYCLOAK_SESSION`) ~3 months. The proxy does NOT renew an expired access cookie by itself; the site's JS does.
  So on 401/403 Pipemic runs `pipemic-login --refresh` (hidden window, same profile) and retries.
  Verified 2026-11-06 16:23: expired access cookie (16:22:55) renewed to 16:38:11 in ~2 s, session file updated.
  (A harmless "WebKit internal error" line is printed when the hidden window exits mid-load.)
- Not yet verified: Get for a preset not yet in the user's library (expected: falls back to the product page).
- Open: what's inside the 20 single-variant marketplace plugins (Linux build or not). Editing bands by hand.

## Free add-ons and Quit (2026-11-06, Linux)
- Free marketplace add-ons not in the user's library: Get now adds them (POST `/api/proxy/orders`
  `{"items":[{"variant_id","plan":"once"}]}`, the website's "Get now"), after checking `/allitems` says free,
  no licence, zero price. Verified: RE20 order `succeeded`, then `purchase-link` 200. The order takes >6 s to
  reach the library, so the download is retried for up to 45 s. Not yet verified end-to-end on a second item.
- Waiting-for-download state has a Cancel button.
- pipemicd now runs only while the app does: the app starts it, tray Quit stops it. Login starts the app in the
  tray (autostart `--background`); the unit is no longer enabled. Verified: stopping pipemicd removes every
  `pipemic.*` node, Spotify falls back to the default sink, the Wave:3 stays default source; start restores routes.
- Mic gain was found at 6.5 dB (was ~38). The ALSA "Mic Capture Volume" (0–80 = 0–40 dB) *is* the hardware gain,
  so app AGC or a volume slider on the Wave:3 source changes it. Open: lock it.
- Robot voice came back (2026-11-06): the PR #7 bridge never split clocks. Both loopback halves shared
  `node.group`/`node.link-group`, and PipeWire 1.6 schedules a link-group under one driver, so the Wave:3 still
  followed the CalDigit (pw-top errors climbing on the Wave source). Fix: each half gets its own `node.group` and
  `node.link-group`, `target.delay.sec = 0.02`. Verified: the Wave:3 drives itself (quantum 256), its error count
  stays flat, and Outgoing still reaches Slack. Check with `pw-top`: the Wave source must be a driver line, not `+`.
- Replug (2026-11-06): the Wave:3 came back on profile `output:iec958-stereo` (no mic source); UI showed Live, no
  input. pipemicd now checks after every connect and sets `output:analog-stereo+input:mono-fallback` if the card
  has no Wave:3 source. Verified by forcing iec958 and restarting pipemicd: fixed in 3 s, bridge relinked.
- 2026-11-06: GitHub repo renamed to `davidboulay/Pipemic` (capital P); old URLs redirect. v0.3.0 released.
- Clean-install check (2026-11-06): removed the local `54-wave3-never-driver.conf` WirePlumber tweak
  (priority.driver 100, headroom 1024). With stock WirePlumber (priority.driver 2100, no headroom) the Wave:3
  still drives its own clock through the bridge, 0 errors over 20 s in `pw-top`. The tweak is not needed; deleted.
- WirePlumber restart (2026-11-06): the mic bridge stayed unlinked after `systemctl --user restart wireplumber`
  (Wave:3 source back with its ports, pipemicd never relinked; `heal()` only retried links it had asked for).
  Fix: `heal()` now also runs the reconcile every 2 s. Verified: 4/4 spaced WirePlumber restarts, bridge linked,
  every ALSA node configured, Wave:3 drives its own clock.
- Related WirePlumber race (not our code): a WirePlumber restart sometimes leaves devices unconfigured (nodes with
  `PortConfig none`, 0 ports). Without pipemicd 0/12 restarts; with a client running `pactl` every second 3-4/12.
  pipemicd's mute sync no longer runs `pactl` every second: it reads the Wave:3 source and its mute from the
  registry (`mic::WaveView`, node Props `mute`) and only runs `pactl` to change something. Verified on the device:
  4 pad taps, PipeWire followed each within ~1 s, no bounce back to the mic. ~50 restarts a few
  seconds apart wedged PipeWire as a whole (every ALSA node unconfigured, WirePlumber restarts didn't help);
  `systemctl --user restart pipewire pipewire-pulse wireplumber` fixed it. Don't stress-test that way again.
- Stop crash (2026-11-06): pipemicd segfaulted on stop: `pw_context_destroy` disconnected the core while the
  filter-chain audio thread was still running (libspa-filter-graph). Fix: loaded modules are destroyed in reverse
  order before the context. Verified: 0 crashes in 11 stops; Spotify back on the default output after stop.
- Open: one mid-run crash at 18:35 (SIGSEGV in libspa-filter-graph from the builtin plugin, main loop idle, no
  log). No debug symbols for PipeWire 1.6.10 on the Arch/CachyOS debuginfod servers. Watch `coredumpctl list pipemicd`.
