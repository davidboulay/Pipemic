# Wave Link 3.4.0 architecture (Phase 2 findings)

Investigated 2026-11-03 on macOS. Labels follow `wave3-protocol.md`: **observed** means seen in shipped files, runtime state or binaries; **inferred** means an interpretation.

## Data model: `~/Library/Application Support/com.elgato.WaveLink3/config.json` (observed, `version: 15`)

Plain JSON. Every object is keyed by a UUID.

| Key | Meaning | Fields |
|---|---|---|
| `channels[]` | Mixer input strips | `id`, `name`, `bus` (int), `mute`, `volume {range:[-60,0], value}` (dB), `duckedBy[]`, `effectRack {enabled, effects[]}`, `source` |
| `channels[].source` | Where audio comes from | `type: "device"` + `input {deviceID, selection {type:"busID", busID:"mic1"}}`, **or** `type: "applications"` + `applications: [bundle IDs]` (`com.elgato.wavelink.system-audio` = everything else) |
| `channels[].effectRack.effects[]` | AudioUnit chain | `name`, `bypass`, `description {type, subType, manufacturer}` (FourCC ints), `effectState` (base64 binary plist = AU preset) |
| `mixes[]` | Output mixes | `id`, `name`, `bus`, `mute`, `volume`, `maximizer`, `maximizerBypassed`, `icon`, `alwaysShowsVolumeSliders`, `outputs[] {deviceID, selection}` |
| `channelToMix[]` | Crosspoint matrix (channel × mix) | `channelID`, `mixID`, `mute`, `volume`, `fxSelect` (int: which effect stage feeds this mix; meaning not yet decoded) |
| `devices[]` | Known endpoints | `kind {type: loopback \| elgatoProduct \| thirdParty, value}`, `audioInputs[] {level, mute, clipGuard, gainLock, lowCutFilter}`, `audioOutputs[]`, `features {crossover, sectionSelection, upsideDownMode}`, `serialNumber` |
| `loopbackDevices[]` | Virtual device pairs from the HAL driver | `inputDeviceName` / `outputDeviceName` (e.g. `Stream Mix` / `Stream Mix Out`), hidden flags |
| `mainOutput` | System output target | `deviceID`, `selection` |
| `apps[]` | Apps seen for routing | `id` (bundle ID), `name`, `bundleURL` |

On this Mac there are 6 channels: Wave:3 mic (bus 5, Voice Focus), All audio, Music (Spotify), Browser (Safari), System (Brave), Voice Chat (Zoom, Slack). There are 3 mixes: Personal → speakers + one more output, Stream → `Stream Mix Out` loopback, Chat → `Chat Mix Out` loopback.

The device fields (`clipGuard`, `gainLock`, `lowCutFilter`, `level`) map onto the hardware protocol in `wave3-protocol.md`.

## Audio engine (observed in `Diagnostics/engine-health-*.txt` and imports)

- There's one IOProc render loop over an **aggregate device**: 5 sub-devices, 4 drift-compensated, at 48 kHz, 3 inputs and 10 outputs.
- **Per-app capture uses Core Audio process taps** ("Taps: 1, processes per tap: [2]"). Channels select apps by bundle ID; they aren't virtual output devices that apps play into. Wave Link 1/2 worked that way, version 3 doesn't.
- **Mixes are published as loopback devices** by `WaveLink3VirtualAudio.driver` (HAL plugin in `/Library/Audio/Plug-Ins/HAL/`). An "X Mix" input device is what OBS or Zoom records from.
- **Effects are AudioUnits** hosted by `WavePluginRack.framework`. "Voice Focus" = `aufx` / `AICu` / `ELGA` (Elgato's AU; `AICu` and the bundled `libaic.dylib` suggest the ai-coustics SDK, which is inferred).
- The DSP settings UI strings include clipguard, expander, compressor, equalizer and ducking presets (low/medium/high).

## Remote control API (observed)

- `NWListener` with `NWProtocolWebSocket` on **127.0.0.1:1884** (live `lsof`).
- The client request handler rejects plain handshakes with HTTP 400, whether or not an `Origin` header is sent. The required header, path or subprotocol is still unknown and needs decompiling.
- The DTO type names (`SD` = Stream Deck) are: `SDApplicationInfo`, `SDChannelConfig`, `SDMixConfig`, `SDInputConfig`, `SDInputDeviceConfig`, `SDInputDevices`, `SDOutputConfig`, `SDOutputDevice(s)`, `SDOutputDeviceConfig`, `SDMainOutput`, `SDAddToChannel`, `SDFocusedAppChannel`, `SDProfileConfig`, `SDPluginInfo`, `SDLevelMeterSubscriptionConfig`, `SDFocusedAppSubscriptionConfig`, `SDWindowModeConfig`.
- The installed Stream Deck plugin `com.elgato.wavelink.sdPlugin` is the older Wave Link 1/2 client: JSON-RPC 2.0 over `ws://127.0.0.1:<base..base+9>`. Its method names are a starting point for the version 3 API.

## Linux mapping (design, inferred)

| Wave Link 3 on macOS | Linux / PipeWire |
|---|---|
| Process taps per bundle ID | Match app streams by `application.process.binary` / `application.name` / `pipewire.access.portal.app_id`, and redirect them into per-channel null sinks or set `target.object` |
| One render loop over an aggregate device | One `wavelinkd` PipeWire filter node (`pipewire-rs`): N channel inputs × M mix outputs with gain, mute and crosspoints from `channelToMix` |
| Loopback devices "X Mix" / "X Mix Out" | `Audio/Source` virtual nodes named "Wave Link Stream Mix" etc. (sources apps can record from) |
| AU effect rack | LV2 / LADSPA hosted in the filter node, or a PipeWire `filter-chain` per channel. Replace Voice Focus with DeepFilterNet or RNNoise |
| WebSocket on 1884 | Same JSON-RPC surface from `wavelinkd`, so Stream Deck-style controllers (OpenDeck on Linux) can drive it |
| `config.json` | The same schema can be read by the Linux daemon: import a Mac config, then map bundle IDs to Linux app identifiers |

## Open items

1. The WebSocket handshake requirement and the full method and notification list (decompile `WaveLinkMacOS`, which needs the 8 GB Ghidra heap).
2. The meaning of `fxSelect` and `bus` numbering, and how ducking (`duckedBy`) is computed.
3. Partly answered: `WaveLink3VirtualAudio.driver` 1.6 is a single AudioServerPlugIn (factory `createVirtualDevice`, Corsair copyright 2019–2022). Its Info.plist declares no fixed device list, so the loopback pairs are probably created at runtime to match `loopbackDevices[]` (3 pairs here; inferred). On Linux this needs no driver, because PipeWire creates the same nodes at runtime.
