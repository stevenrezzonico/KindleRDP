# KindleRDP

A Remote Desktop (RDP) client for jailbroken Kindle e-readers. Connect to a Windows PC and use it on the e-ink screen with touch.

Tested on one device only: a Kindle Paperwhite (11th generation, 1236x1648), firmware 5.19.6, jailbroken with Vera. It is written to run on the other touch-screen Kindles too, but nobody has tried: see [Other Kindle models](#other-kindle-models).

## Features

- Server list with on-device editor and keyboard; several servers, one saved password each
- Real Windows touch input: one-finger scroll, drag, pinch, press-and-hold
- Two-finger tap for right click
- Landscape and portrait
- E-ink aware refresh: 16 grays, per-area pacing, a cleaning flash only when idle
- One static 3.4 MB binary, no dependencies on the Kindle besides the jailbreak

## Requirements

- A jailbroken Kindle with the Universal Hotfix (it provides scriptlets, which put the app in the library, and the `fbink` tool used as a fallback)
- A Windows PC with Remote Desktop enabled and Network Level Authentication (the Windows default)
- Both on the same network, or the PC reachable from the Kindle

## Install

1. Build `dist\` (see below) or take it from a release.
2. Connect the Kindle over USB and copy the contents of `dist\` to its root folder. This adds `documents/KindleRDP.sh` and the `kindlerdp/` folder.
3. Eject. **KindleRDP** appears in the library.

To update, copy `dist\` over again. Servers, passwords and settings are kept.

## Use

On the server list: tap **Add**, fill in host, user and password, **Save**, then tap the server to connect.

| Gesture | Action |
|---|---|
| Tap | Click |
| One-finger drag | Scroll, or drag what is under the finger (Windows decides) |
| Press and hold about 1 s, lift | Right click |
| Two-finger tap | Right click |
| Two fingers moving | Pinch zoom |
| Two fingers resting 0.7 s | KindleRDP menu |

The menu offers **Keyboard** (shows or hides the keyboard, see below), **Scale** (Windows display scale for this server; reconnects), **Disconnect**, **Rotate screen** and **Back to Windows** (which also refreshes the whole screen).

### Keyboard

The keyboard is KindleRDP's own and covers the lower part of the Windows picture. **Move** puts it at the top when it hides what you type into; **Hide** closes it.

- A key acts when the finger lands, so you can type ahead of the screen.
- **Shift**: one tap for the next letter, two taps to lock.
- **Ctrl**, **Alt** and **Win** stay on for the next key: tap **Ctrl**, then **c**.
- Backspace, **Del** and the arrows repeat while held.
- **#+=** and **àéü** switch to symbols and accented letters.

Letters and symbols arrive right on any Windows keyboard layout. Shortcuts use the US key positions: on a QWERTZ layout Ctrl+Z and Ctrl+Y are swapped, on AZERTY A/Q and Z/W.

**Settings** on the server list changes the default Windows display scale and the contrast. A server can have its own scale: the **Scale %** field in its editor, or **Scale** in the menu. Empty means the default. The same values live in `kindlerdp/config.ini`, which the app creates on first start.

If something fails, the reason is shown on screen and written to `kindlerdp/log.txt`.

## Other Kindle models

Only the Paperwhite above is tested. What the code does for the others:

| | Status |
|---|---|
| Newer MediaTek models (Paperwhite 11th and 12th generation, Kindle 2022 and 2024, Scribe) | Same screen driver as the tested one. Expected to work. |
| Older models with a 1072x1448 or 1264x1680 screen (Paperwhite 3 and 4, Voyage, Oasis) | Untested. The screen is refreshed through the `fbink` tool, which is slower. |
| Models with a 600x800 or 758x1024 screen (Kindle Touch, Kindle 7th to 10th generation, Paperwhite 1 and 2) | Untested. The app's own screens are shown at half size. Set **Windows scale** to 100 or 125 in **Settings**. The oldest of them have a Linux kernel older than Rust supports, so the program may not start at all. |
| Colour screens (Colorsoft) | Not supported: the app stops with "unsupported screen". |
| Kindles without a touch screen | Not supported. |

The app is started from the library by a scriptlet, which needs a jailbreak with the Universal Hotfix on a recent firmware.

If it does not work, `kindlerdp/log.txt` starts with the firmware, the screen and the touch screen that were found. These lines in `kindlerdp/config.ini` can help:

- `refresh = fbink`: do not use the direct screen refresh at all.
- `touch = swap_xy mirror_x mirror_y` (any of the three): when taps land in the wrong place.
- `touch_device = /dev/input/event1`: when the touch screen is not found or the wrong device is used.

## Passwords and security

- Passwords are stored encrypted in `/var/local/kindlerdp`, which is not visible over USB. The key is derived from the device serial, so the files are useless on another device. Anyone with root access to the Kindle can still recover them: use a Windows account with limited rights.
- The server's certificate is remembered on first connection. If it later changes (Windows renews it about twice a year), KindleRDP stops before sending the password and asks whether to trust the new one.

## Build

Both need [rustup](https://rustup.rs) and [zig](https://ziglang.org) on the `PATH`. Zig is only used as the C compiler for the crypto library; Rust links the binary itself.

### Windows

```powershell
rustup default stable-x86_64-pc-windows-gnu
rustup target add armv7-unknown-linux-musleabihf
.\package.ps1
```

`package.ps1` runs the unit tests, cross-compiles for the Kindle and fills `dist\`.

### Linux

Tested on Ubuntu 26.04 (under WSL) with Rust 1.100 and zig 0.17.1. Neither needs root: rustup installs into your home folder, and zig is one archive from [ziglang.org/download](https://ziglang.org/download/) to unpack, with its `zig` program put on the `PATH`.

```sh
rustup target add armv7-unknown-linux-musleabihf
./package.sh
```

`package.sh` cross-compiles for the Kindle and fills `dist/`. It does not run the unit tests. Run them with `cargo test`; on Linux that also builds the device code for the build machine, which needs a C compiler there (`gcc` or `clang`).

## Limits

- Windows hosts only; no RD Gateway, Kerberos, audio or clipboard
- No keyboard opens by itself on a text field; use **Keyboard** in the menu
- The keyboard has no ë, ï or å (the font lacks them) and no F1 to F12
- An e-ink update takes about half a second: fine for desktop work, not for video

## Built with

[IronRDP](https://github.com/Devolutions/IronRDP) for the protocol, [Spleen](https://github.com/fcambus/spleen) for the font. The launcher follows what [KOReader](https://github.com/koreader/koreader) does to keep the Kindle's own interface quiet.
