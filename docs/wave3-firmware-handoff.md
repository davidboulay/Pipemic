# Handoff: Wave:3 firmware update on Linux (from the Mac session, 2026-11-04)

## Goal of the next session
Continue the Wave:3 firmware-update work **on the Linux machine** that has the mic plugged in. Static reverse engineering is done on the Mac. What's left is confirming on the device and then, with the user's explicit go-ahead, building an updater in Rust.

## Read first (don't re-derive)
- `docs/wave3-dfu.md`: the full finding. It covers the catalog URL, image format, the XMOS DFU sequence (0xF2 → 10 s wait → 0xF4 → DNLOAD/GETSTATUS → manifest), the Wave:3 `fwDownloadOptions = 0x00010100`, and the open questions.
- `HANDOFF.md` § "Firmware update": a short status line.
- `docs/wave3-protocol.md`: USB interface layout (interface 3 = vendor control, interface 4 = DFU FE/01).

> ⚠️ `docs/wave3-dfu.md` is **untracked** and the `HANDOFF.md` changes aren't committed on the Mac. The user has to commit and push them (or copy them across) before the Linux session can see them.

## State
- The firmware catalog lists only 1.2.2 for the Wave:3, which the mic already runs, so there's nothing newer to install.
- Image: `https://edge.elgato.com/egc/macos/ewlm/Firmware/wave_3_firmware_1_2_2.dfu`, sha256 `05cf543a36373a9685a3460a889df5cc7189403ae419296f23098ae74c6a8b1f`. Re-download it on Linux; it isn't in the repo.
- Nothing has been sent to the device. No DFU request has been issued.

## Next steps (in order; stop for user approval before any write to flash)
1. **Read-only:** `lsusb -v -d 0fd9:0070` → record the DFU functional descriptor:
   - bmAttributes: canDnload/canUpload/manifestationTolerant/willDetach
   - wDetachTimeout, wTransferSize, bcdDFU
   - confirm the DFU interface number (expected 4)

   Add the results to `docs/wave3-dfu.md`.
2. **Ask the user first:** send `0x21 / bRequest 0xF2 / wValue 0 / wIndex <dfu itf>` to enter DFU mode, then record the DFU-mode VID:PID and descriptors (`lsusb`, `dmesg`). Then get it back to normal mode: try `0xF3` (RESETFROMDFU) or a USB reset or replug, and note which one works. Check that `snd-usb-audio` and `wavelinkd` recover afterwards. Stop `wavelinkd` first, because it claims interface 3 and polls every 200 ms.
3. If bitCanUpload is set: a `DFU_UPLOAD` read-back to check the protocol without writing.
4. Only after explicit approval: a `wavectl fw` command (or a `wave-proto` dfu module, using rusb) that implements the sequence in `docs/wave3-dfu.md`. Re-flashing 1.2.2 is the only test image available. The recovery path (XMOS factory image, REVERTFACTORY 0xF1) is inferred, not verified.

## Constraints
- **REA and Ghidra stay on the Mac.** Don't install them on Linux. If more binary analysis is needed (for example, how the device leaves DFU mode, or the `DFU@` header), note the question for the Mac.
- The udev rule `udev/70-elgato-wave.rules` already grants user access to 0fd9:0070. The DFU-mode PID may need its own rule.
- The user prefers short replies that say exactly what they need to do in 1–2 lines.

## Suggested skills
- `superpowers:brainstorming`: before designing the `wavectl fw` / DFU module.
- `superpowers:systematic-debugging`: if the device doesn't come back from DFU mode or enumerates unexpectedly.
- `superpowers:test-driven-development`: for the suffix/CRC parsing and DFU state machine (unit-testable without hardware).
- `superpowers:verification-before-completion`: before claiming any device step worked.
- Not `rea:reverse-engineer-anything`: it's Mac-only (see the constraints).
