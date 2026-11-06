# Wave Link marketplace add-ons: how they work, and how Pipemic can use them

Research on the Mac, 2026-11-05. Sample: `Pip-Boy Voice FX-1.0.1.106.zip` from
<https://marketplace.elgato.com/product/pip-boy-voice-fx-07fff950-baac-448e-8090-d75e056a2175>.
DSP and parameter detail for Pip-Boy: `docs/addon-pipboy-dsp.md`.

## 1. What an add-on is

There are two kinds of Wave Link add-on.

| Kind (`extension`) | Count | What it is |
|---|---|---|
| `vst` | 39 | A standard audio plugin. On Windows it is a **VST3** bundle; on macOS it is an **Audio Unit** (`aufx`) installed from a `.pkg`. Nothing in it is specific to Wave Link. |
| `presets` | 14 | A plain XML preset for Elgato's own **Equalizer** plugin (for example, "Old Radio"). Pipemic can import it natively (see below). |

### Pip-Boy Voice FX zip (Windows variant)
```
PipBoyVoiceFX_v1.0.1.106.vst3/Contents/
  x86_64-win/PipBoyVoiceFX_v1.0.1.106.vst3   PE32+ DLL, x86-64 (9.9 MB)
  arm64-win/PipBoyVoiceFX_v1.0.1.106.vst3    PE32+ DLL, ARM64
  Resources/moduleinfo.json                  VST3 module info
```
- Framework: **JUCE 7.0.12** (`juce_version_7_0_12`), VST3 SDK 3.7.8. The C++ class is
  `ElgatoFalloutFxAudioProcessor`. The GUI uses JUCE's Windows renderer (d2d1, DWrite, dxgi).
- Class IDs: processor `ABCDEF019182FAEB4D616E75476C3370`, controller `ABCDEF011234ABCD4D616E75476C3370`.
- **No licence check and no host check.** The only "isLicensed" string is JUCE's WAV iXML metadata
  field. There is no network code and no Wave Link IPC, so any VST3 host can load it.
- Files it writes on Windows: `Documents/VST3 Presets/Elgato/Pip-BoyVoiceFX/`, plus a log under
  `Elgato/VSTs/Pip-BoyVoiceFX.log` (app data). State is a JUCE ValueTree:
  `WholeState` > `Parameters`, `Presets`/`CustomisedPresets`, `ActivePreset`, `PresetName`.
- Parameters (IDs): `RadioToggle`, `RadioSignalDrift`, `RadioBandwidth`, `RadioStaticLevel`,
  `AgingToggle`, `AgingAmount`, `AgingNoiseFloor`, `RadiationToggle`, `RadiationMix`, `RadiationGain`,
  `ReverbToggle`, `ReverbMix`, `ReverbDecayTime`, `SelectedPreset`, plus the VST3 `Bypass`.
- Factory presets: Wasteland Wanderer, Vault-Tec Announcement, Radiation Storm, Brotherhood of Steel
  Broadcast, Super Mutant Roar, Ghoulish Whisper, Caps Trader Haggling, Bunker Blues, Wasteland Echoes.

### How Wave Link on the Mac hosts them (from `WaveLinkMacOS` strings and `config.json`)
- Effects are AVAudioUnit/AUAudioUnit instances in a per-channel `effectRack`. `config.json`
  stores each one as `description {type, subType, manufacturer}` (FourCC) and `effectState`
  (base64 binary-plist AU preset). See `docs/wavelink3-architecture.md`.
- `WavePluginRack.framework` is **not** the effect host. It drives the hardware DSP insert on MK.2
  devices (Lewitt code: `/mic/vst_enabled`, `MIC_POSTVST`). It is not relevant to the Wave:3.
- Marketplace module `ElgatoMarketplace` (OAuth2 code grant via account.elgato.com) downloads a
  variant, unarchives it, finds the component `.pkg` inside, and installs it. Presets go to
  `~/Library/Audio/Presets/` (the app asks for folder permission). Deep links use
  `wavelink://` / `wavelink-v2://`.

### Elgato Equalizer + EQ presets (decoded 2026-11-05)
- `Equalizer-1.3.0.110.zip` contains `ElgatoEq_v1.3.0.110.vst3`, a JUCE VST3 (x64 + ARM64 Windows), class
  `ABCDEF019182FAEB4D616E754232626D`. Factory presets are saved to `Documents/VST3 Presets/Elgato/EQ/`.
- **A preset add-on is a single plain XML file** (for example `Old_Radio-1.0.0_HdYjEzV.xml`, 2.7 KB),
  not a zip:
  ```xml
  <Preset>
    <Info Name="Old Radio" Position="6"/>
    <Parameters>
      <PARAM id="Band 1 Filter Bypass" value="0.0"/>  ... 6 params x 8 bands
    </Parameters>
  </Preset>
  ```
- 8 bands, each with 6 parameters. Values are in real units, not 0–1. Ranges come from the
  parameter-building function `FUN_18001bb00`:

  | Param | Range | Step | Notes |
  |---|---|---|---|
  | `Band N Filter Type` | choice 0–4 | — | 0 highPass, 1 lowShelf, 2 peak, 3 highShelf, 4 lowPass (**measured** 2026-11-06) |
  | `Band N Filter Frequency` | 20–20000 Hz | 0.1 | |
  | `Band N Filter Quality` | 0.1–10 | 0.01 | Q |
  | `Band N Filter Gain` | 0.25–4.0 | 0.01 | **linear** gain factor: dB = 20·log10(value); 2.0 measured +6.02 dB, 4.0 → +12.04 dB |
  | `Band N Filter Bypass` | bool | | |
  | `Band N Filter Visible` | bool | | UI only. **An invisible band still filters** (measured: invisible peak ×4 gave +12.04 dB). Only `Bypass` disables a band |

- **Measured on Linux (2026-11-06, yabridge 5.1.1 + wine-staging 11.21, pedalboard host, 48 kHz):**
  - Type order is the UI list order. The Mac-side guess (4 = HP) was wrong.
  - Every type is a textbook RBJ "Audio EQ Cookbook" biquad: white-noise null against a float64 RBJ cascade
    is −60 to −115 dB for all 5 types at 100 Hz / 1 kHz / 12 kHz and Q 0.5–3. HP/LP are **one biquad,
    12 dB/oct**, and Q applies to them (Q 3 → +9.54 dB bump at fc = 20·log10 Q).
  - "Old Radio" = LP 1505 Hz Q 0.71 → HP 2732 Hz Q 0.71 → peak 2037 Hz Q 3.1 +12 dB: a narrow resonant band
    (−0.7 dB at 2037 Hz, about −11 dB at 1.5 and 2.7 kHz). Plugin vs RBJ magnitude: max 0.05 dB off, 50 Hz–16 kHz.
  - Plugin defaults: band 1 HP 20 Hz, band 2 peak 1 kHz, band 8 LP 20 kHz visible; bands 3–7 hidden peaks at 0 dB.
- Import into Pipemic: every **non-bypassed** band becomes a filter-chain biquad (`bq_highpass`,
  `bq_lowshelf`, `bq_peaking`, `bq_highshelf`, `bq_lowpass`) with `Freq`, `Q` and
  `Gain = 20·log10(value)` dB. Bands that are an identity (peak/shelf at gain 1.0) can be dropped.
  Ignore `Visible`. Note filter-chain's `bq_*` "Q" for shelves must be checked against RBJ shelf Q in the PR test.
  Test fixture: values from "Old Radio" above (don't commit Elgato's files).

## 2. Marketplace API (public, no login needed for metadata)

Base `https://mp-gateway.elgato.com`:

| Call | Returns |
|---|---|
| `GET /products?application=wave-link[&extension=vst\|presets]&limit=20&offset=N` | Catalogue (53 Wave Link products on 2026-11-05) |
| `GET /products/{product_id}` | Description, `variants[]` (`name` Windows/Mac/default, `is_free`, `needs_license`, `versions[]`) |
| `GET /vst-os-mapping?product_id={id}` | `{"mac": variant_id, "win": variant_id}` |
| `GET /variants?ids={variant_id}` | Variant summary |

File downloads need an Elgato account (OAuth2). Not tested from code, and we shouldn't automate it
without asking: the user can download manually from the website, which gives the Windows zip.

### Catalogue snapshot (2026-11-05)
- **Free, no licence (hostable today):** Pip-Boy Voice FX, Black Ops 6 Voice Modifier, De-Esser,
  Equalizer, Compressor, Noise Removal, Simple Noise Gate (Videophile), Tiny Phone (MilkpackTV,
  Windows only), MONOCORE (makrofon), Lil' Mod, VX Pilot and Mr. Meowington (Voicemod). Plus all
  14 EQ presets.
- **Paid, `needs_license`:** Voice Focus, ReverbFX, DemonicFX, all paid Voicemod FX, makrofon
  (AETHER, AutoLevel, CyberDroid, EXN0ISE, Morphento, OBSIDIAN, SPECTER, VoiceSpace, ZERO-SYNC),
  Lofi Loris, Just Pitch. How each one checks its licence is unknown, and it may not work under Wine.
- Raw catalogue JSON is reproducible with the calls above, so it isn't committed.

## 2b. Which add-ons can run on Linux (2026-11-06)

Variants per product (`GET /products/{id}`, 53 Wave Link products):

| Variants | Count | On Linux |
|---|---|---|
| `presets`, one `default` variant (EQ presets, plain XML) | 14 | Yes: native import (channel EQ) |
| `vst`, one `default` variant (Simple Noise Gate, the Voicemod FX…) | 20 | Unknown: contents not listed; a VST3 bundle can hold `x86_64-linux`. Needs one download to check |
| `vst`, Windows + macOS variants | 18 | No (Windows needs Wine; macOS AUs can't run on Linux) |
| `vst`, Windows only (Tiny Phone) | 1 | No |

**Decision (user, 2026-11-06): no Wine.** Option A below is dropped. The marketplace website has no OS filter
(`/audio/presets` and `/audio/vst` are the only categories), so Pipemic lists compatible add-ons itself from the
public API and links each one to its product page for the download.

## 3. Options for Pipemic on Linux

No add-on ships a Linux build. There are three routes, and they can be combined.

**A. Host the Windows VST3 through yabridge + Wine (works for any add-on, including paid ones if their licence check works under Wine).**
- yabridge wraps `~/.vst3/*.vst3` (Windows) into a Linux VST3 that runs the DLL in a Wine host process.
  JUCE 7 plugins are usually well supported. The GUI runs as an X11/XWayland window.
- PipeWire `filter-chain` can't load VST3 (it supports builtin, LADSPA and LV2; check this on 1.6).
  So Pipemic needs a VST3 host. Two ways to get one:
  1. **Carla** (`carla-rack` / `carla-single`) as a PipeWire/JACK client inserted in the mic chain.
     This is the quickest prototype and already handles editor windows and state.
  2. A **`pipemic-fx` host process in Rust** (`vst3` / `vst3-sys` crates + `pipewire` filter node),
     one per channel rack. Running it as a separate process keeps a plugin crash out of `pipemicd`.
     Save state as base64 VST3 component+controller state in `mixer.toml`.
- Install flow: the user downloads the zip from the marketplace. Pipemic installs it with "Import add-on…
  (.zip/.vst3)", unpacks it to `~/.vst3/`, and runs `yabridgectl sync`. Pipemic never redistributes
  Elgato or third-party plugins (same rule as firmware).
- Dependencies: wine (staging is recommended by yabridge) and yabridge from the AUR. This is heavy, so
  make it an optional feature.

**B. Native reimplementations (no Wine).** Only for simple effects:
- Equalizer and the 14 EQ presets: once the preset format is decoded, map the bands onto filter-chain
  `bq_*` biquads. This is the best value for the effort.
- Compressor, De-Esser, Noise gate: use LV2 plugins (for example LSP or x42) or builtin filter-chain nodes.
- Noise Removal / Voice Focus: RNNoise or DeepFilterNet (already on the roadmap).
- Pip-Boy: a clean-room reimplementation from the observed parameters and DSP (`addon-pipboy-dsp.md`).
  We must not copy Elgato's code, assets, preset names or Fallout branding.

**C. Native Linux VST3 / LV2 support in general.** Once the host from A.2 exists, any Linux VST3
or LV2 plugin works too. This is the "add your own effect" feature.

### Recommendation
1. Build a per-channel effect rack in `pipemicd` with a separate host process (A.2), and support Linux LV2
   and VST3 first (C).
2. Add Windows add-ons through yabridge as an optional "Import Wave Link add-on" (A).
3. Add native EQ preset import (B) once a preset sample is decoded.

## 4. Open items
- EQ preset format: decoded and measured (see above); done. Samples on Linux: `~/Downloads/pipemic-addons/`
  (not in git).
- Licence behaviour of paid plugins under Wine (test with a free licensed one if one exists, otherwise skip).
- On Linux (2026-11-06): yabridge + Wine loads both plugins; parameter lists confirmed (see
  `addon-pipboy-dsp.md` §4 for what is still open on Pip-Boy).
- yabridge setup that worked: Windows bundles in `~/.wine/drive_c/Program Files/Common Files/VST3/`
  (not `~/.vst3`, which is where yabridgectl writes the Linux wrappers), `yabridgectl add <that dir> && yabridgectl sync`.
  Wine's `wineusb` enumerates every USB device including the Wave:3, and left `winedevice.exe` processes
  running after the host exited, so it was disabled in the prefix
  (`HKLM\System\CurrentControlSet\Services\wineusb` Start=4). An import feature should do the same in its own prefix.
- Legal: check Elgato's marketplace EULA on using add-ons outside Wave Link before shipping the
  import feature. For now treat it like firmware: the user's own download, never bundled.
