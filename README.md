<p align="center"><img src="packaging/pipemic.svg" width="112" alt="Pipemic icon"></p>

<h1 align="center">Pipemic</h1>
<p align="center"><b>Audio mixer and mic control for the Elgato Wave:3 on Linux</b><br>
An open-source alternative to Elgato Wave Link, built on PipeWire.</p>

---

Elgato's Wave Link app runs on Windows and macOS only. Pipemic brings the same
workflow to Linux:

- **Control the Wave:3 itself:** mic gain, mute, low cut, Clipguard, headphone
  volume, the monitor mix, and what the front knob controls. A pad tap or a
  knob-mode change on the mic shows up live.
- **Mix your apps into two mixes:** **Personal** (what you hear) and
  **Outgoing** (what others hear). Select *Pipemic Outgoing* as the
  microphone in OBS, Discord, Zoom or Meet.
- **Channels by type:** Microphone, Music, Browser, Voice Chat and Other.
  Apps land in the right channel by their desktop category (Spotify goes to
  Music, Chrome to Browser, Slack to Voice Chat). You can move any app by
  hand, and Other holds the apps you add yourself.
- **Safe by default:** only your mic goes out until you send a channel
  out yourself. That way nothing leaks into a call or stream by surprise.
- **Live meters** for every channel and both mixes, a per-channel fader link,
  and an output picker for your headphones or speakers.

## Supported hardware

| Device | Mixer | Mic control |
|---|---|---|
| Elgato Wave:3 (`0fd9:0070`) | ✅ | ✅ |
| Other Elgato Wave mics (Wave:1, Neo, XLR, Wave:3 MK.2) | ✅ as a plain audio device | ❌ not yet (USB protocol not captured) |

Tested on Arch/CachyOS with PipeWire 1.6 and WirePlumber, on KDE Plasma (Wayland).

## Install (from source)

Requirements: Rust (stable), PipeWire with its development headers, libusb,
clang. `rsvg-convert` is optional and renders the icon sizes.

```sh
# Arch / CachyOS
sudo pacman -S --needed rust clang libpipewire libusb librsvg

git clone https://github.com/davidboulay/pipemic
cd pipemic

# Let your user talk to the mic without root, then replug it
sudo install -m644 udev/70-elgato-wave.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger -s usb

# Build and install for your user (~/.local), and start the daemon
./packaging/install-user.sh
```

Then open **Pipemic** from your app menu. To update, use *Preferences → Check
for updates*, or `git pull` and re-run the install script.

## How it works

| Part | What it does |
|---|---|
| `pipemicd` | User daemon (systemd user unit). Creates one virtual sink per channel and one in-process filter-chain per mix, wires them, routes apps, meters levels and talks to the mic over USB. |
| `pipemic` | The mixer window (Rust, [iced](https://iced.rs)). Talks to the daemon over `$XDG_RUNTIME_DIR/pipemicd.sock`. |
| `pipemicctl` | Command-line control: `pipemicctl get`, `pipemicctl watch`, `pipemicctl mix channel music -6`, … |
| `wave-proto` | The Wave:3 USB control protocol (vendor interface 3), documented in [`docs/wave3-protocol.md`](docs/wave3-protocol.md). |

Mixer settings live in `~/.config/pipemic/mixer.toml`. All PipeWire objects
belong to the daemon and disappear when it stops; apps it moved go back to
your default output.

## Status

Pipemic is young and works day to day on the author's machine. These aren't
built yet: noise suppression, firmware updates, upside-down mode, and Stream
Deck integration.

## License

MIT. See [LICENSE](LICENSE).

---

<sub>Pipemic is an independent project. It is not affiliated with, endorsed by,
or sponsored by Elgato or Corsair. Elgato, Wave, Wave:3 and Wave Link are
trademarks of Corsair Memory, Inc., used here only to describe compatibility.</sub>
