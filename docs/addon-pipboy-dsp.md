# Pip-Boy Voice FX: parameters, presets and DSP (decompiled)

Date: 2026-11-05. Source: `PipBoyVoiceFX_v1.0.1.106.vst3/Contents/x86_64-win/` (Windows x64 VST3,
JUCE 7.0.12, internal class `ElgatoFalloutFxAudioProcessor`), sha256 `8be408caebcfd33ef40e0e82bb89afc60e4959d261b0c3557398924b5c1cedf2`.
Tool: Ghidra 12.1.4 through REA, on the Mac. Static analysis only: nothing was executed.

Labels: **O** = observed in decompiled code, **I** = inferred from it, **U** = unknown.
Addresses are in the x64 DLL image (base 0x180000000).

The plugin has no licence check and no Wave Link-specific API. It is a normal JUCE VST3 with an
`AudioProcessorValueTreeState`. That means a clean-room reimplementation is fully specified by the tables below.

## 1. Parameters

`createParameterLayout` = `FUN_180029c40`. Parameter ID strings are set up in static initialisers at
`0x180001ba0`–`0x180001e10`.

Every float parameter uses `NormalisableRange(0, 100, interval 0.01, skew 1.0)` (**O**: `local_2f0 = {0.0, 100.0}`,
`{0.01, 1.0}`). No unit suffix is set (**O**: the lambdas are empty). Every bool parameter defaults to `false`
(**O**: 4th argument is 0). The UI and Wave Link show the IDs as names.

| ID | Type | Range | Default | Group |
|---|---|---|---|---|
| `RadioToggle` | bool | off/on | off | Radio |
| `RadioSignalDrift` | float | 0–100 | 25 | Radio |
| `RadioStaticLevel` | float | 0–100 | 20 | Radio |
| `RadioBandwidth` | float | 0–100 | 50 | Radio |
| `AgingToggle` | bool | off/on | off | Aging |
| `AgingAmount` | float | 0–100 | 25 | Aging |
| `AgingNoiseFloor` | float | 0–100 | 25 | Aging |
| `RadiationToggle` | bool | off/on | off | Radiation |
| `RadiationGain` | float | 0–100 | 25 | Radiation |
| `RadiationMix` | float | 0–100 | 50 | Radiation |
| `ReverbToggle` | bool | off/on | off | Reverb |
| `ReverbDecayTime` | float | 0–100 | 25 | Reverb |
| `ReverbMix` | float | 0–100 | 50 | Reverb |
| `SelectedPreset` | choice (`MetaParameter<AudioParameterChoice>`) | the 9 preset names | index 0 | — |

Toggle semantics (**O**, `FUN_18002d4f0`): each block keeps a "bypassed" flag = `(toggle == 0)`.
Choosing a preset (the `SelectedPreset` parameter) writes all the values below. On first `prepareToPlay` the
selected preset (index 0, "Wasteland Wanderer") is applied (**O**, `FUN_18002bd20`, else branch), so a new instance
does not start with everything off.

Other state (**O**): presets saved by the user go in `%APPDATA%/VST3 Presets/Elgato/Pip-BoyVoiceFX/`
(`SHGetSpecialFolderPathW(5)` = My Documents, **I**), with `default_presets.xml`, `custom_presets_tmp`,
`CustomisedPresets`, `WholeState`, `ActivePreset`, `PresetName`. Log file `Pip-BoyVoiceFX.log` under `Elgato/VSTs`.

## 2. Factory presets

Preset table = `FUN_180029370`, a chain of name compares that fill a 20-field struct. `FUN_18002dc70` maps the
fields to parameters through `setValueNotifyingHost(normalised)` (**O**). So the values below are 0–1 normalised,
and the table shows them ×100 (the parameter's 0–100 scale). The exception is DecayTime: field 10 is a raw 0–100
value converted with the parameter's own range (**O**: `FUN_1800db6b0/db8a0`), so it is shown unchanged.

| # | Preset | Radio on | Drift | Band-width | Static | Aging on | Amount | Noise floor | Rad. on | Rad. mix | Rad. gain | Reverb on | Decay | Rev. mix |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | Wasteland Wanderer | 1 | 20 | 70 | 30 | 1 | 50 | 25 | 1 | 20 | 10 | 1 | 6 | 20 |
| 1 | Vault-Tec Announcement | 1 | 5 | 90 | 10 | 0 | 50 | 25 | 0 | 50 | 25 | 1 | 4 | 20 |
| 2 | Radiation Storm | 0 | 50 | 50 | 25 | 0 | 50 | 25 | 1 | 70 | 25 | 1 | 33 | 12 |
| 3 | Brotherhood of Steel Broadcast | 1 | 2 | 50 | 15 | 1 | 33 | 9 | 0 | 50 | 25 | 1 | 6 | 11 |
| 4 | Super Mutant Roar | 0 | 50 | 50 | 25 | 1 | 60 | 15 | 1 | 50 | 20 | 1 | 5 | 65 |
| 5 | Ghoulish Whisper | 1 | 3 | 10 | 30 | 1 | 35 | 30 | 1 | 10 | 30 | 1 | 12 | 40 |
| 6 | Caps Trader Haggling | 1 | 15 | 25 | 20 | 1 | 20 | 10 | 0 | 50 | 25 | 0 | 0.5 | 20 |
| 7 | Bunker Blues | 1 | 75 | 50 | 25 | 1 | 25 | 15 | 1 | 4 | 12 | 1 | 0.5 | 50 |
| 8 | Wasteland Echoes | 1 | 40 | 60 | 50 | 0 | 50 | 25 | 1 | 30 | 20 | 1 | 35 | 30 |

Confidence: **O** for the constants; **I** for the per-path toggle bytes, because Ghidra's goto-merged control flow
had to be followed by hand. Verify on Linux by loading the VST3 (yabridge) and reading the parameters after each
preset.

Unused struct fields (not written to parameters; **O**): 11 = 0.8, 13 = 1 − mix in most presets (dry), 14 = 0.4,
16 = 0.01, 17 = 0.1. These match the reverb's fixed internals (damping 0.8, width 0.4, mod 0.01, 0.1). If no name
matches, the fallback has all blocks off and reverb fields {0.2, 0.5, 0.33, 0.4, 1.0, 0, 1.0}.

## 3. Signal chain

`processBlock` = `FUN_18002c570` (**O**). FTZ/DAZ is set on entry. Blocks run in place, in this fixed order, and
each is skipped while bypassed:

1. **Radio**: `FUN_18017b620`, state at processor +0x230
2. **Aging**: `FUN_180174f10`, +0x3e0
3. **Radiation**: `FUN_1801743d0`, +0x6e0
4. **Reverb**: mono `FUN_180028cf0` / stereo `FUN_1800284c0`, +0x7a0. Runs only when in = out channels (1 or 2).

After the chain, an output RMS meter in dBFS (floor −100) feeds the UI. Channels are processed independently
(per-channel filter state). The plugin accepts mono or stereo (bus layout `{1,2}` in/out, **O** `FUN_18002ad40`).
The random generator in every block is `drand48` (`x*0x5DEECE66D + 0xB mod 2^48`, **O**), reseeded to 1
every block (**O**: `local_res10 = 1`). So the "random" pattern repeats per block length (**I**).

Filters: every filter is a TPT state-variable filter (`g = tan(pi*fc/fs)`, `R = 1/Q`,
`h = 1/(1 + R*g + g²)`) = `juce::dsp::StateVariableTPTFilter`; type index 0 = low-pass, 2 = high-pass (**O/I**).

Shared waveshaper `FUN_18017c010` (**O**): `x *= drive`; if `x < 0`: `y = tanh(sinh x) − 0.2·x·sin(pi·x)`, else
`y = tanh x`; output `y · 10^(−0.75·driveDb/20)`. It is set up by `FUN_18017bdf0` with a 10 Hz SVF
(DC block, R = √2, **I**).

### 3.1 Radio (`FUN_18017b3c0` prepare, `FUN_18017bc30` bandwidth, `FUN_18017bd90` drift)

| Item | Formula / value | Conf. |
|---|---|---|
| Low-pass (SVF type 0) | fc = 4000 − Bandwidth·20 Hz (4000→2000), Q = 1.7. Initial 8000 Hz | O |
| High-pass (SVF type 2) | fc = 400 + Bandwidth·4 Hz (400→800), Q = 1.7. Initial 200 Hz | O |
| Order | noise added, then HP, then LP | O |
| Static gain | smoothed `Static·0.01·0.7` (0–0.7) | O |
| Static source | `(saw(oscA)·2−1)·0.1 + (white·2−1)·0.2`, zeroed if \|n\| < 0.03, then waveshaper (drive 10^1.5, 30 dB), × 0.1 × static gain | O |
| Osc A | pitch = base × (0.98 + 0.04·\|tri(oscB)\| + (rand−0.5)·0.008). Osc B is a 3 Hz triangle | O |
| Input gate | `x = (1−a)·x + a·(|x|<0.01 ? 0 : x)`, a at +0x4 | O (a value U) |
| Drift → glitch threshold | Drift ≤ 50: `t = 1 − Drift·0.01`; > 50: `t = 0.5 − (Drift·0.01−0.5)·0.7`. Per block, glitch on if `rand ≥ t` | O |
| Glitch | adds a copy of the block × 0.1 × static × 0.6 | O |
| Gain smoothing | linear ramp over `floor(fs·0.0005)` samples | O |

### 3.2 Aging / tape (`FUN_180174ba0` prepare, `FUN_180174f10` process). A = Amount/100, N = NoiseFloor/100

| Item | Formula / value | Conf. |
|---|---|---|
| Wow/flutter | modulated delay line (`FUN_18017c9a0`), max depth `fs·0.007` samples, feedback/mix 0.9, triangle LFO with a random rate 0.0175–10 Hz picked each block. Used when `rand > 1 − A·0.5` | O |
| Dropout | if A > 0.5: per-block chance `(A−0.5)·0.1` to start a dropout lasting 51 blocks. Separately, a sample is zeroed when `rand > 1 − A·0.25` | O |
| Repeat/skip | when dropout is active, reads back from a circular buffer at a random offset (0..fs) | O |
| Saturation | waveshaper, drive = A·10 + 20 dB (init 25 dB) | O |
| Hiss | adds a noise buffer × (+0x4) × N when `rand ≤ 1 − A·0.02` | O (noise source U) |
| Low-pass | fc = 4000 − A·500 Hz, Q = 0.707 + A·1.293 | O |
| High-pass | fc = 200 + A·200 Hz, same Q | O |

### 3.3 Radiation / Geiger crackle (`FUN_1801743d0`, `FUN_180174810` mix). G = Gain/100

| Item | Formula / value | Conf. |
|---|---|---|
| Event threshold from Mix | Mix ≤ 50: `t = 1 − Mix·0.003`; > 50: `t = 0.85 − (Mix·0.01 − 0.6)·1.5`. Initial 0.4 | O |
| Event per block | Poisson draw λ = 1 − t (`FUN_1801742f0`), mapped to (−1,1); active if `t ≤ value` | O |
| Tone | 440 Hz osc (+0x6ec = 440), pitch × (0.99 + 0.02·\|tri\| + jitter 0.003); second osc 0.025/fs | O |
| Crackle | `n = ((white·2−1)·0.6 + sin(osc)·0.4)·0.1`, zeroed if \|n\| < 0.03, bit-crushed (`floor(n·0.07·2^0.4)/2^0.4`), added × G × 0.2 | O |
| Dry signal | passes through unchanged (`(1−a)x + a·x`) | O |

### 3.4 Reverb (`FUN_180027ad0` set, `FUN_180028cf0` mono)

Custom reverb, not `juce::Reverb`. Parameter struct at processor +0xb50:
`{decay, 0.8, wet, dry, 0.4, 0, 0.01, 0.1, ...}`, with wet = Mix/100 and dry = 1 − Mix/100 (**O**).

| Item | Formula / value | Conf. |
|---|---|---|
| Topology | pre-delay line → 8 parallel feedback delay lines (one-pole low-pass damping in each loop) summed → 4 series all-pass, g = 0.618 | O |
| Decay | each line's feedback gain = `10^(−3·len/(fs·DecayTime))`, so DecayTime is RT60 in seconds (0–100 s) | O/I |
| Damping | `p[7]·0.399 + 0.6` with p[7] = 0.1 → 0.64 | O |
| Input gain | smoothed `(p[4]+1)·(wet·0.7)·1.5` | O |
| Pre-delay / mod | `maxDelay·p[6]` (p[6] = 0.01) | O (max length U) |
| Output | `dry·2·x + wetGain·reverb` (both smoothed) | O |
| Delay lengths | in the reverb object (+0x38 table); not read | U |

## 4. Unknowns worth a runtime check (Linux, via yabridge)

- Delay-line lengths for the reverb FDN, and the stereo variant `FUN_1800284c0`.
- Radio coefficient `a` (+0x4) and the aging noise source and gain (+0x4).
- The preset toggle bytes (section 2) and the actual parameter values after loading each preset.
  **Partly done 2026-11-06** (yabridge + Carla 2.5 headless, state decoded from the JUCE `WholeState` chunk):
  - Parameter list, ranges (0–100, step 0.1 as seen by the host) and defaults match §1.
  - A new instance applies "Wasteland Wanderer" on first prepare; its 13 values match row 0 of §2 exactly.
  - Presets 1–8 not verified: setting `SelectedPreset` from the host changes the controller's text but the
    processor state stays on preset 0, and `CustomisedPresets` only holds presets that were applied. Needs the
    plugin GUI (click each preset, save state) or a host that sends the change through `process()`.
  - Spotify `pedalboard` 0.9.26 hangs on Pip-Boy's first `process()` (EQ works). Likely the preset apply in
    `prepareToPlay` notifying the host from the audio thread. Use Carla or a real host for this plugin.
- Null test: render white noise and a sine sweep through the real plugin and through a reimplementation.
