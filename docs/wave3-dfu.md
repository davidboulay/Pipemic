# Wave:3 firmware update (DFU)

How Wave Link 3.4.0 for macOS updates the Wave:3 firmware. Sources:
- Static analysis of the shipped binaries (2026-11-04): Ghidra via REA on `tlusbdfuapi`, otool disassembly of `WaveAPI`.
- The downloaded firmware image.

**No update has been observed or attempted on the device.** Each fact below is marked *observed* (read from bytes or code) or *inferred*.

## Pieces

| Piece | Role |
|---|---|
| `WaveLinkMacOS` | Swift app: firmware catalog, download, SHA-256 check, UI |
| `WaveAPI.framework` | C++ device layer (`LegacyWaveDevice`, `TLDFUBackend.cpp`). Loads `tlusbdfuapi` with dlsym |
| `tlusbdfuapi.framework` | Thesycon USB DFU library v2.71, licensed per VID/PID (`0x0fd9:0x0070` is on the list) |

## Catalog and image (observed)

- Catalog: `GET https://gc-updates.elgato.com/mac/ewlm-update/final/device-update-check.php?osName=macOS&osVersion=…&osPlatform=arm64&appVersion=3.4.0&appLanguage=en-FR`.
  - The JSON reply has `hardwareDevices[]`, matched by `id.vendorId` / `id.productId`.
  - Each entry has `firmware[]` with `version`, `downloadUrl`, `importance`, `minimumOS` and more.
- Wave:3 (PID 112): **1.2.2**, `https://edge.elgato.com/egc/macos/ewlm/Firmware/wave_3_firmware_1_2_2.dfu`. That's the version the test device already runs, so there's no newer firmware today.
- `wave_3_firmware_1_2_2.dfu`: 98 384 bytes, sha256 `05cf543a36373a9685a3460a889df5cc7189403ae419296f23098ae74c6a8b1f`.
  - Not encrypted: the code region 0x2040–0x16e40 has about 6.1 bits/byte of entropy.
  - Starts with a `DFU@` header (not decoded, and not needed: the library treats the image as opaque).
  - The firmware commit `0b4251ee` sits just before the suffix.
  - It ends with a **standard DFU 1.1 suffix**: bcdDevice 0x0122, PID 0x0070, VID 0x0fd9, bcdDFU 0x0100, `UFD`, bLength 16, dwCRC 0xaff413c3. The CRC checks out (DFU CRC-32 over every byte before dwCRC).

## Wave:3 settings in WaveAPI (observed)

`LegacyWaveDevice::fwDownloadOptions()` returns the constant `0x00010100`, so the `FWDownloadOptions` bytes are:

| Byte | Value | Effect in `TLDFUBackend::doFWDownload` |
|---|---|---|
| 0 | 0 | **No** `RebootDevice(runMode 1)` after the upgrade |
| 1 | 1 | After the reset into DFU, pick the DFU device whose serial number matches |
| 2 | 1 | `flags = 1` on RebootDevice and StartUpgrade, which turns on the **XMOS DFU extensions** |
| 3 | 0 | Use `TLDFUBackend`, not the legacy `DfuDriverApi` fallback |

The bRequest values match XMOS `sw_usb_audio` (RESETINTODFU 0xF2, RESETFROMDFU 0xF3, SELECTIMAGE 0xF4), so the Wave:3 is very likely XMOS-based (*inferred*).

## Sequence (observed in code; timings from the disassembly)

All control requests go to the DFU interface (interface 4 on the Wave:3; *inferred* from the descriptor layout, the library uses the DFU interface it opened).

1. **Load image:** `TLDFU_LoadFirmwareFromBuffer(buf, len, imageType=1 (raw), &img, flags=1)`. `LookForDfuFileSuffix` checks `UFD` and the CRC; a CRC mismatch is a hard error. The suffix is stripped, so the payload is **file minus 16 bytes** (98 368 bytes). The raw type skips the Thesycon "TL binary" compatibility check (vendor IN 0xD2).
2. **Into DFU mode:** `TLDFU_RebootDevice(dev, runMode=2, flags=1)`:
   - SET_INTERFACE alt 0, if not already set.
   - Class/interface OUT request: `bmRequestType 0x21, bRequest 0xF2, wValue 0, wIndex <DFU itf>, wLength 0`.
   - Errors from this request are ignored; the device drops off the bus.
3. **Wait 10 s**, enumerate DFU-mode devices and open the one with the matching serial number. The app's device table has a separate `wave3_DFU` identity; the DFU-mode PID is unknown.
4. **Upgrade:** `TLDFU_StartUpgrade(dev, img, targetId=0, flags=1)`:
   - `EnsureDfuIDLE`: GETSTATUS, with CLRSTATUS or ABORT as needed.
   - **Select image:** `0x21, bRequest 0xF4, wValue 0 (targetId), wIndex <DFU itf>, wLength 0`.
   - **Download:** `DFU_DNLOAD` (`0x21, bRequest 1, wValue = block number from 0`) in chunks of the descriptor's **wTransferSize**. After each block, `DFU_GETSTATUS` (`0xA1, bRequest 3`), sleep bwPollTimeout, until the state is dfuDNLOAD_IDLE.
   - **Manifest:** a zero-length DNLOAD with the next block number, then poll GETSTATUS:
     - MANIFEST_SYNC (6): sleep bwPollTimeout (10 ms if it's 0) and poll again.
     - MANIFEST (7): sleep bwPollTimeout; done if the device isn't manifestation-tolerant.
     - IDLE (2): done.
     - ERROR (10) or bStatus ≠ 0: fail.
5. Poll `GetUpgradeStatus` until it finishes, then `FinishUpgrade` and **wait 2 s**. Because byte 0 = 0, the app sends no RESETFROMDFU. How the device gets back to run mode (self-reset after manifest, or on close) is **unknown**.

The macOS app also has a parallel `LewittDeviceThesyconMacBackend::dfuReset(enter, xmos)`. It sends the same request directly: 0xF2 to enter, 0xF3 to leave, 0x00 (standard DFU_DETACH) when `xmos` is false, with a 500 ms timeout.

## For Linux

- No Thesycon library needed. Everything above is standard libusb control transfers (rusb), and Linux can also issue a USB reset if the device doesn't come back by itself.
- `dfu-util` alone won't do it: it sends a standard DETACH, not 0xF2, and doesn't send 0xF4. A small Rust tool in `wave-proto` is simpler.
- Safety: XMOS designs usually keep a factory image and write upgrades to a separate slot (there's a REVERTFACTORY 0xF1 request), which would make a failed flash recoverable. That's *inferred* and not verified for this device. Bootloader 0.3.7 is reported separately.

## Tooling (`pipemicctl fw`)

Code: `wave-proto::dfu` (image + mode switching) and `pipemicctl/src/fw.rs`. **There's no flash write yet**; that waits for a firmware newer than 1.2.2 to test against.

| Command | Does |
|---|---|
| `fw status` | Mode (run / bootloader / absent), firmware and bootloader versions, DFU state |
| `fw check` | Compares the installed firmware with Elgato's catalog |
| `fw fetch [--version V] [-o F]` | Downloads and validates (suffix, CRC, 0fd9:0070 target, version match, pinned sha256 if known) |
| `fw inspect FILE` | Validates a local image |
| `fw dfu-test --yes` | 0xF2 → GETSTATUS → 0xF3, then checks the firmware version didn't change |
| `fw leave` | Recovers a mic stuck at 0fd9:0071 |

Unit tests: `WAVE3_DFU=path/to/1.2.2.dfu cargo test -p wave-proto` also checks the real image.

## DFU descriptor (observed on Linux, 2026-11-04, fw 1.22)

`lsusb -v -d 0fd9:0070`, run-time mode. There are 5 interfaces; **interface 4** is DFU (class FE / subclass 01 / protocol 1, "Elgato Wave:3 DFU", no endpoints).

| Field | Value |
|---|---|
| bmAttributes | `0x0D`: canDnload, **no upload**, manifestationTolerant, willDetach |
| wDetachTimeout | 240 ms |
| wTransferSize | 64 bytes |
| bcdDFU | 1.10 |

Implications:
- **willDetach** means the device should re-enumerate by itself after DETACH/0xF2, with no host-side USB reset needed.
- **No upload**, so the read-back check (old step 3) isn't possible. Any protocol check has to be enter-DFU then leave, without DNLOAD.
- **wTransferSize 64**: send the image as 64-byte DNLOAD blocks.

## Enter/leave DFU mode (observed on Linux, 2026-11-04)

Nothing was written to flash. Requests went through rusb, with interface 4 claimed in run mode and interface 0 in DFU mode.

- Run mode, GETSTATUS (0xA1/0x03) on itf 4 → `00 0a 00 00 00 00`: OK, poll 10 ms, state 0 = appIDLE. GETSTATE (0x05) **times out** in run mode, so use GETSTATUS.
- `0x21 / 0xF2 / wValue 0 / wIndex 4` → Ok. The device re-enumerates within 1 s, with no host reset, as:
  - **0fd9:0071 "USB Bootloader"**, bcdDevice **0.37**, serial `xxxxxxxxxxxxxx`, self-powered.
  - 1 interface (0), FE/01/**protocol 2** (DFU mode). Alt 0 and alt 1 both exist, and alt 1 carries the functional descriptor (same values: 0x0D, 240 ms, 64 B, DFU 1.10).
  - GETSTATUS on itf 0 → `00 0a 00 00 02 00`: OK, state 2 = dfuIDLE.
  - The mic **does not** go back to run mode on its own (it stayed at 0071 for more than 15 s).
  - It needs its own udev rule. 0071 has been added to `udev/70-elgato-wave.rules`. After installing it, the already-enumerated device needed `udevadm trigger --action=add <syspath>`; a plain `trigger -s usb` didn't apply it.
- `0x21 / 0xF3 / wValue 0 / wIndex 0` (RESETFROMDFU) → libusb returns **Pipe** (the device resets before it ACKs, so this is expected), and the device comes straight back as 0fd9:0070 fw 1.22. snd-usb-audio and the PipeWire source recover with no replug. Treat a Pipe or NoDevice error on 0xF3 as success.
- Side effect: after the round-trip, **Chrome's audio service still saw no audio devices at all**, not even speakers, even though PipeWire was healthy. Reloading the tab didn't help. Ending Chrome's "Utility: Audio Service" (Shift+Esc in Chrome) fixed it. An updater should warn about this, or say to restart the browser afterwards.

## Still open (needs the device on Linux)

1. ~~DFU functional descriptor~~: done, see above.
2. ~~DFU-mode VID:PID / leaving DFU~~: done (0071, and 0xF3 works). Still unknown: whether the device leaves DFU mode on its own **after manifestation**.
3. ~~DFU_UPLOAD read-back~~: not possible because bitCanUpload is 0. The only non-writing check is enter DFU (0xF2) → inspect → leave (0xF3 / reset).
4. The meaning of the `DFU@` header.
