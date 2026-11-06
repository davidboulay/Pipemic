# Add-ons: handoff to the Linux session (2026-11-06)

Goal: let Pipemic use Wave Link marketplace add-ons on Linux. The Mac-side reverse engineering is done.
What's left is verification on Linux, then building the feature once the user approves the design.

Read first (don't re-derive):
- `docs/addons-research.md`: what add-ons are, the EQ preset format, the marketplace API, options A/B/C
  and the recommendation.
- `docs/addon-pipboy-dsp.md`: Pip-Boy parameters, presets, signal chain and open unknowns (section 4).

## Sample files: where to put them on Linux
Copy these three files from the Mac's `~/Downloads` to **`~/Downloads/pipemic-addons/`** on the Linux box.
That folder is outside the repo, so they can't be committed by accident. Never commit or redistribute them.

| File | What it is |
|---|---|
| `Pip-Boy Voice FX-1.0.1.106.zip` | Windows VST3 (JUCE 7) |
| `Equalizer-1.3.0.110.zip` | Elgato EQ, Windows VST3 |
| `Old_Radio-1.0.0_HdYjEzV.xml` | EQ preset (plain XML) |

If they're missing, ask the user to copy them first. Don't download them from the marketplace yourself,
because downloads need the user's Elgato login.

## Status (2026-11-06)
Steps 1–3 done; step 4 partly (see `addon-pipboy-dsp.md` §4). Step 5 decided: **no Wine**; native add-ons only
(see `addons-research.md` §2b). Channel EQ engine built (branch `addons-eq`); next is the UI add-on browser.

## Remaining work, in order
1. **Check the host side (read-only).**
   - On PipeWire 1.6, does `filter-chain` only support builtin, LADSPA and LV2 (no VST3)?
   - Are `wine` (staging) and `yabridge` / `yabridgectl` installed, or available from the AUR?
     Ask the user before installing anything.
2. **Prototype with yabridge.**
   - Unzip both plugins into `~/.vst3/`, then run `yabridgectl add ~/.vst3 && yabridgectl sync`.
   - Load each one in Carla and confirm it loads, its GUI opens, and audio passes.
3. **Measure the EQ** (this closes the open question in `addons-research.md` §1).
   - Enable band 1 alone and sweep it. Find out which `Filter Type` index is high-pass and which is low-pass
     (we suspect 4 = HP and 0 = LP), and how steep the HP/LP slope is.
   - Confirm that `Gain` is a linear factor (1.0 = 0 dB).
   - Null-test "Old Radio" against an equivalent filter-chain made of `bq_*` biquads.
4. **Measure Pip-Boy.**
   - Dump the parameters after each factory preset to confirm the per-preset toggles.
   - Render noise and sine sweeps to settle `addon-pipboy-dsp.md` §4 (reverb delay lengths, stereo path).
5. **Design decision: ask the user before writing code.**
   - Recommended: a per-channel effect rack in `pipemicd`, hosted by a separate `pipemic-fx` process (VST3/LV2).
   - Plus "Import Wave Link add-on" (yabridge, optional) and native EQ preset import.
   - Present 2–3 options briefly, then wait for "go".
6. **Build it** on a feature branch with a PR, starting with the part the user picks. Native EQ preset import
   (XML to `bq_*` biquads) is the cheapest first PR once step 3 settles the type mapping.
7. **Record what you verified** in `addons-research.md` / `addon-pipboy-dsp.md`, and add dated bullets to `HANDOFF.md`.

## Suggested skills
- `superpowers:brainstorming`: before step 5 (designing the effect rack with the user).
- `superpowers:writing-plans`, then `superpowers:executing-plans` or `superpowers:subagent-driven-development`: for step 6.
- `superpowers:test-driven-development`: for the EQ XML parser and the biquad mapping (fixture values come from
  "Old Radio", typed into the test rather than copying Elgato's file).
- `superpowers:systematic-debugging`: if yabridge, Carla or filter-chain misbehave.
- `superpowers:verification-before-completion`: before you claim the measurements are done.
- Not `rea:*`: REA and Ghidra are Mac-only. Any new binary question goes in `HANDOFF.md` as a Mac-side task.

## Rules carried over
- Never redistribute Elgato or third-party plugins or presets; the user imports their own downloads.
- Don't automate marketplace downloads (they need OAuth) without asking.
- Don't leave the mic broken: run `wpctl status` after any filter-chain or Carla experiment, and remove
  any test nodes you created.
