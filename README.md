# KindleRDP

A Remote Desktop (RDP) client for jailbroken Kindle e-readers. Connect to a Windows PC and use it on the e-ink screen with touch.

Tested on a Kindle Paperwhite, firmware 5.19.6, jailbroken with Vera. Other MediaTek-based Kindles with an 8-bit gray screen should work; nothing else has been tried.

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

The menu offers **Keyboard** (toggles the Windows On-Screen Keyboard), **Scale** (Windows display scale for this server; reconnects), **Disconnect**, **Rotate screen** and **Back to Windows** (which also refreshes the whole screen).

**Settings** on the server list changes the default Windows display scale and the contrast. A server can have its own scale: the **Scale %** field in its editor, or **Scale** in the menu. Empty means the default. The same values live in `kindlerdp/config.ini`, which the app creates on first start.

If something fails, the reason is shown on screen and written to `kindlerdp/log.txt`.

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

Not yet tested on a Linux machine; please report what breaks.

```sh
rustup target add armv7-unknown-linux-musleabihf
./package.sh
```

`package.sh` cross-compiles for the Kindle and fills `dist/`. It does not run the unit tests: on Linux `cargo test` also builds the device code for the build machine, which needs a C compiler there.

## Limits

- Windows hosts only; no RD Gateway, Kerberos, audio or clipboard
- The Windows touch keyboard does not open by itself on a text field; use **Keyboard** in the menu
- An e-ink update takes about half a second: fine for desktop work, not for video

## Built with

[IronRDP](https://github.com/Devolutions/IronRDP) for the protocol, [Spleen](https://github.com/fcambus/spleen) for the font. The launcher follows what [KOReader](https://github.com/koreader/koreader) does to keep the Kindle's own interface quiet.
