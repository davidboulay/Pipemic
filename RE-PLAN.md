# Wave Link 3.4.0 → Rust/Linux: reverse-engineering plan

Target: `/Applications/Elgato Wave Link.app` (com.elgato.WaveLink3, 3.4.0, universal Swift/SwiftUI app).

## Bundle map (observed with otool/file, 2026-10-28)

| Component | Size | Likely role | Linux counterpart |
|---|---|---|---|
| `MacOS/WaveLinkMacOS` | 35M | SwiftUI app: mixer UI, state, routing | Rust app (UI layer TBD) |
| `Frameworks/WaveAPI.framework` | 22M | Device control protocol (mic gain, mute, LEDs, Clipguard) via IOKit/USB | **Core RE target**: Rust USB/HID crate (`rusb`/`hidapi`) |
| `Frameworks/WavePluginRack.framework` | 23M | Effects chain / AU plugin hosting | LV2/CLAP host or PipeWire filter-chain |
| `Frameworks/libaic.dylib` | 16M | Probably the ai-coustics SDK (AI voice/noise processing; unverified) | Proprietary. Can't be ported, so substitute (RNNoise/DeepFilterNet) |
| `Frameworks/tlusbdfuapi.framework` | 0.9M | Thesycon USB DFU (firmware updates) | Out of scope; do firmware updates on the Mac |
| `Resources/WaveLink3VirtualAudioDriverInstaller.pkg` + `Library/LaunchServices/...DriverHelper` | n/a | CoreAudio HAL virtual devices (channels, Stream/Monitor mixes) | PipeWire virtual sinks/sources and links |
| Sentry, Sparkle, Lottie | n/a | Telemetry, updates, animation | Drop |

## Questions for REA (checklist)

1. ✅ (Wave:3) WaveAPI: USB device matching (VID/PID), transport (HID reports, vendor control transfers, or USB Audio class controls), message layout per setting. See `docs/wave3-protocol.md`. WaveAPI is LEWITT's C++ `lewittaudio-waveapi` library, not Swift. Other models still to dump.
2. WaveLinkMacOS: data model for inputs, channels and the two mixes (Stream/Monitor); persistence format (SwiftData/CoreData store).
3. Virtual driver: number and naming of virtual devices; how the app routes audio into them (shared memory? XPC?).
4. WavePluginRack: built-in effects and their parameters; whether it hosts third-party AU/VST.
5. Network.framework usage: Stream Deck / external control API (local WebSocket?). Worth keeping that API compatible.

## Tooling (this Mac only, not synced)

- REA MCP `rea` is registered at local scope for this project only (`~/.claude.json` → projects.wave-link) and listed in `MCP_SYNC_EXCLUDE`.
- Ghidra 12.1.4 is in `~/.local/share/rea/` with the arm64 decompiler built locally. Thinned arm64 slices are in `~/.local/share/rea/work/wavelink-3.4.0/`.
- Ghidra caveat: WaveAPI's static initializers (~240 KB each) make Ghidra run out of memory at the default 2 GB heap. For small functions, use `objdump -d --start-address`.
- The REA skill lives in the local plugin `rea@rea-local` (`~/.local/share/rea/claude-marketplace`).
