# Elgato Wave:3 control protocol (API 5.3)

Source: Wave Link 3.4.0, `WaveAPI.framework` (LEWITT `lewittaudio-waveapi` C++ library, namespace `LWT`), plus read-only probes of a live Wave:3. Investigated 2026-11-03.

Confidence labels:
- **observed**: read in the shipped code or returned by the device.
- **derived**: inferred by matching raw bytes against values the library reported. Not yet confirmed by changing a setting and re-reading.

## Device

| | |
|---|---|
| VID / PID | `0x0fd9` / `0x0070` (observed) |
| Firmware | 1.2.2 build 3, commit `0b4251ee`. Bootloader 0.3.7. API 5.3 (observed) |
| USB interfaces | 0 AudioControl (1/1), 1–2 AudioStreaming (1/2), **3 vendor (FF/F0/01)**, 4 DFU (FE/01) (observed) |
| Library class | `LWT::Wave3Device`, backend strategy `LegacyUAC1VendorUSBBackendStrategy` (observed) |

## Transport (observed in `LegacyUAC1VendorUSBBackendStrategy::{read,write}` and `LewittDeviceThesyconMacBackend::{in,out}CtrlRequest`)

All traffic is UAC1 memory requests, sent as class requests with an interface recipient. The timeout is 500 ms.

| Operation | bmRequestType | bRequest | wValue | wIndex | Length |
|---|---|---|---|---|---|
| Read message | `0xA1` | `0x85` GET_MEM | message ID | `0x3300 \| ifnum` | message size |
| Write message | `0x21` | `0x05` SET_MEM | message ID | `0x3300 \| ifnum` | message size |
| API version | `0xA1` | `0x85` | `10` | `0x3300 \| ifnum` | 2 → `[major, minor]` |

`0x33` is the entity ID and the low byte is the interface number. The device answered with ifnum 0–3 (observed).

**Linux note:** `snd-usb-audio` binds interface 0, and usbfs refuses class requests aimed at an interface owned by another driver. Interface 3 has no kernel driver, so set ifnum = 3, claim interface 3 with `rusb`, and audio streaming isn't disturbed. (This follows from the usbfs `check_ctrlrecip` rules and still needs testing on Linux.)

Other LEWITT devices use two more strategies:
- **Legacy UAC2**: class request, bRequest `0x03` (MEM) for both read and write.
- **MK2** (Wave:3 MK2, XLR MK2, XLR Dock MK2): vendor request (`0xC1`/`0x41`), bRequest `0x01`, wValue = message ID, wIndex = `apiMajor<<8 | ifnum`. The version probe is bRequest `0x00`, 2 bytes.

## Messages (layout derived; sizes observed)

The library reads every message whole and writes it back whole. Writes should do the same: read, change the field, write all of it.

### msg 0: `config` (16 bytes, read/write, packed little-endian)

The offsets for bytes 0–12 were **observed on 2026-11-03** by polling msg 0 while each control was changed in Wave Link (log in `docs/wave3-config-watch.log`). Bytes 13–15 are still **derived**: they're placed by schema order and Wave Link has no LED settings to test them with. Bytes 2–3 never changed.

| Off | Size | Path | Encoding | Range / step |
|---|---|---|---|---|
| 0 | 2 | `/config/input_gain` | i16 8.8 fixed, dB | 0.0 … 40.0 / 0.5 |
| 2 | 2 | (not in schema) | i16 8.8 (read -20.0) | unknown: keep unchanged |
| 4 | 1 | `/config/input_mute` | bool | |
| 5 | 1 | `/config/clipguard_enable` | bool | |
| 6 | 1 | `/config/lowcut_enable` | bool | |
| 7 | 2 | `/config/headphone_volume` | i16 8.8, dB | -60.0 … 0.0 / 0.5 |
| 9 | 1 | `/config/headphone_mute` | bool | |
| 10 | 2 | `/config/direct_monitor` | i16 8.8, % (`0x3700` = 55, `0x3200` = 50) | 0 … 100 / 5 |
| 12 | 1 | `/config/volume_select` | u8 enum: 1 MIC, 2 HEADPHONE, 3 MIX | |
| 13 | 1 | `/config/all_leds_off` | bool | |
| 14 | 1 | `/config/leds_flip` | bool | |
| 15 | 1 | `/config/gain_lock` | bool: ignore OS SET_CUR volume | |

### msg 1: `status` (8 bytes, read-only)

| Off | Size | Path |
|---|---|---|
| 0 | 4 | `/status/touch_pressed_ms` u32: on fw 1.2.2 always equal to offset 4 (observed) |
| 4 | 4 | `/status/touch_signal` u32: raw capacitive reading of the top pad (observed) |

Observed 2026-11-03 on Linux (polling every 50 ms): idle about 100–450, touched 8 000–9 300. During a 3 s hold the value stays flat, so it isn't a press timer. The thresholds in msg 10 (900 / 1500) sit between those two levels. A tap on the pad toggles mute in the firmware, and the change shows up in config byte 4. So the hardware mute can be followed by polling msg 0; msg 1 isn't needed for that.

### msg 10: `version` (52 bytes, read-only)

| Off | Size | Path |
|---|---|---|
| 0 | 1 | api major |
| 1 | 1 | api minor |
| 2 | 2 | vbus_voltage i16 × 0.004 V |
| 4 | 1 | mic_type (1 Schubert, 2 Strauss) |
| 5 | 1 | hw_version_board |
| 6–8 | 3 | bootloader major/minor/patch |
| 9 | 4 | bootloader build u32 |
| 13 | 8 | bootloader commit (ASCII, may be empty) |
| 21–23 | 3 | firmware major/minor/patch |
| 24 | 4 | firmware build u32 |
| 28 | 8 | firmware commit (ASCII) |
| 36 | 12 | serial (ASCII) |
| 48 | 2 | touch threshold lower u16 |
| 50 | 2 | touch threshold upper u16 |

## Still to verify

1. ✅ Config offsets 0–12 confirmed. Still open: LED bytes 13–15 and the purpose of bytes 2–3. Test of 2026-11-03: writing byte 13 = 1 was accepted and read back as 1, but the LED ring showed **no visible change** while the mic sat untouched. Byte 13 was restored to 0. A retry with byte 13 = 1 while turning the knob and tapping the pad also changed nothing: the LEDs stayed lit and reacted normally. So byte 13 is a stored field, but it does **not** behave as `all_leds_off` on fw 1.2.2. It's either a different setting, or the firmware ignores it. Bytes 13–15 stay unconfirmed. Leave them alone; the daemon doesn't need them.
2. ✅ Linux (2026-11-03, CachyOS, `wavectl`): claiming interface 3 and using wIndex `0x3303` works without root (udev `uaccess`). Capture kept streaming non-zero audio before, during and after the claim. A SET_MEM of msg 0 (gain 40.0 → 39.5 → 40.0) was read back byte-exact; bytes 2–15 stayed unchanged. The ALSA `Mic Capture Volume` control did not follow the vendor write (it still showed 80 = 40 dB), so the daemon must not rely on ALSA for gain state.
3. Other device models (Wave:1, XLR, Neo, MK2): run `tools/dump.cpp` against each, changing the PID in the VID/PID list.
4. ✅ msg 1 `status`: decoded (see the msg 1 table). Both fields carry the raw touch signal; `touch_pressed_ms` isn't a timer on this firmware.

## Tools

- `tools/probe.c`: probes which protocol version and strategy a device answers.
- `tools/memdump.c`: reads raw message bytes for IDs 0–31.
- `tools/dump.cpp`: loads Elgato's `WaveAPI.framework` (macOS only) and prints the library's own schema with live values. Build command:
  `clang++ -std=c++20 dump.cpp -F"/Applications/Elgato Wave Link.app/Contents/Frameworks" -framework WaveAPI -Wl,-rpath,…`
