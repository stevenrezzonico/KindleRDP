//! Kindle hardware: framebuffer, e-ink refresh through the fbink tool, touch screen, state files.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context as _, bail};

use crate::gesture::{MtParser, Touch};
use crate::rects::{Rect, Wave};

/// Root-only ext partition: survives reboots and is not exposed over USB.
pub const STATE_DIR: &str = "/var/local/kindlerdp";

static FBINK: OnceLock<String> = OnceLock::new();

pub fn set_fbink(configured: Option<&str>) {
    let found = configured.map(str::to_owned).unwrap_or_else(|| {
        ["/mnt/us/libkh/bin/fbink", "/var/local/kmc/bin/fbink", "/mnt/us/koreader/fbink", "/usr/bin/fbink"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .unwrap_or("fbink") // PATH
            .to_owned()
    });
    let _ = FBINK.set(found);
}

fn fbink(args: &[&str]) {
    let tool = FBINK.get().map_or("fbink", String::as_str);
    match Command::new(tool).arg("-q").args(args).status() {
        Ok(s) if s.success() => {}
        other => eprintln!("fbink {args:?} failed: {other:?}"),
    }
}

/// Show a line of text in the middle of the screen.
pub fn message(text: &str) {
    eprintln!("{text}");
    fbink(&["-pmM", text]);
}

pub fn serial() -> Vec<u8> {
    // Empty when unreadable: the saved password is then only obfuscated, not device-bound.
    fs::read("/proc/usid").unwrap_or_default()
}

pub fn random_nonce() -> anyhow::Result<[u8; 12]> {
    let mut n = [0; 12];
    File::open("/dev/urandom")?.read_exact(&mut n)?;
    Ok(n)
}

pub fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    fs::create_dir_all(STATE_DIR)?;
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    Ok(())
}

pub fn state(name: &str) -> PathBuf {
    Path::new(STATE_DIR).join(name)
}

const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOGET_FSCREENINFO: u32 = 0x4602;

#[repr(C)]
#[derive(Default)]
struct FixInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// Waveform by name: (driver id, ms one update keeps its area busy, limited to black/white-ish).
/// Ids are MTK_WAVEFORM_MODE_* from the Kindle kernel's hwtcon_ioctl_cmd.h.
pub fn waveform(name: &str) -> Option<(u32, u64, bool)> {
    Some(match name {
        "A2" => (6, 120, true),
        "DU" => (1, 260, true),
        "DU4" => (7, 290, true),
        "GL16" => (3, 450, false),
        "REAGL" => (4, 450, false),
        "GC16" => (2, 450, false),
        "AUTO" => (257, 450, false),
        _ => return None,
    })
}

/// MXCFB_SEND_UPDATE_MTK: _IOW('F', 0x2E, struct mxcfb_update_data_mtk), 24 u32 fields.
const SEND_UPDATE: u32 = (1 << 30) | (96 << 16) | ((b'F' as u32) << 8) | 0x2E;
const TEMP_USE_AMBIENT: u32 = 0x1000;

/// The panel, driven in landscape: session (x, y) maps to framebuffer (fb_w - 1 - y, x).
// shortcut: fixed 90° rotation and 8-bit gray only; add a config key and 32bpp for other models.
pub struct Fb {
    file: File,
    map: *mut u8,
    len: usize,
    fb_w: i32,
    fb_h: i32,
    stride: usize,
    fast: String,
    fast_id: u32,
    gamma: [u8; 256],
    marker: Cell<u32>,
    /// Set after the direct ioctl failed once (not an MTK panel): use the fbink tool from then on.
    via_fbink: Cell<bool>,
}

impl Fb {
    pub fn open(fast_waveform: &str, gamma: f32) -> anyhow::Result<Self> {
        let (fast_id, ..) = waveform(fast_waveform).with_context(|| format!("unknown fast_waveform '{fast_waveform}'"))?;
        let gamma = std::array::from_fn(|i| ((i as f32 / 255.0).powf(gamma) * 255.0).round() as u8);
        let f = OpenOptions::new().read(true).write(true).open("/dev/fb0").context("open /dev/fb0")?;
        let mut var = [0u32; 40]; // struct fb_var_screeninfo: 0 = xres, 1 = yres, 6 = bits_per_pixel
        let mut fix = FixInfo::default();
        // SAFETY: both buffers match the kernel struct sizes for these ioctls.
        let ok = unsafe {
            libc::ioctl(f.as_raw_fd(), FBIOGET_VSCREENINFO as _, var.as_mut_ptr()) == 0
                && libc::ioctl(f.as_raw_fd(), FBIOGET_FSCREENINFO as _, &mut fix) == 0
        };
        if !ok {
            bail!("framebuffer ioctl failed: {}", std::io::Error::last_os_error());
        }
        let (fb_w, fb_h, bpp) = (var[0] as i32, var[1] as i32, var[6]);
        if bpp != 8 {
            bail!("unsupported screen: {bpp} bits per pixel (only 8-bit gray)");
        }
        let (stride, len) = (fix.line_length as usize, fix.smem_len as usize);
        if stride < fb_w as usize || len < stride * fb_h as usize {
            bail!("unexpected framebuffer layout: {fb_w}x{fb_h} stride {stride} len {len}");
        }
        // SAFETY: mapping a framebuffer of the size the kernel just reported.
        let map = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, f.as_raw_fd(), 0) };
        if map == libc::MAP_FAILED {
            bail!("mmap /dev/fb0: {}", std::io::Error::last_os_error());
        }
        Ok(Self {
            file: f,
            map: map.cast(),
            len,
            fb_w,
            fb_h,
            stride,
            fast: fast_waveform.to_owned(),
            fast_id,
            gamma,
            marker: Cell::new(0),
            via_fbink: Cell::new(false),
        })
    }

    /// Session size: landscape, width a multiple of 4 as RDP servers expect.
    pub fn session_size(&self) -> (u16, u16) {
        ((self.fb_h & !3) as u16, self.fb_w as u16)
    }

    pub fn fb_size(&self) -> (i32, i32) {
        (self.fb_w, self.fb_h)
    }

    /// Copy `r` (session coordinates) from an RGBA image to the panel as gray.
    pub fn blit(&self, rgba: &[u8], img_w: usize, r: Rect) {
        // SAFETY: the mapping is `len` bytes and lives as long as self.
        let fb = unsafe { std::slice::from_raw_parts_mut(self.map, self.len) };
        for y in r.y..r.y + r.h {
            let col = (self.fb_w - 1 - y) as usize;
            let row = &rgba[(y as usize * img_w + r.x as usize) * 4..][..r.w as usize * 4];
            for (i, px) in row.chunks_exact(4).enumerate() {
                let gray = (px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8;
                fb[(r.x as usize + i) * self.stride + col] = self.gamma[gray as usize];
            }
        }
    }

    pub fn refresh(&self, r: Rect, wave: Wave) {
        let (top, left, width, height) = (r.x, self.fb_w - r.y - r.h, r.h, r.w);
        if !self.via_fbink.get() {
            let (mode, full) = match wave {
                Wave::Fast => (self.fast_id, 0),
                Wave::Quality => (3, 0), // GL16
                Wave::Full => (2, 1),    // GC16, flashing
            };
            self.marker.set(self.marker.get().wrapping_add(1).max(1));
            let mut update = [0u32; 24];
            update[..7].copy_from_slice(&[top as u32, left as u32, width as u32, height as u32, mode, full, self.marker.get()]);
            update[7] = TEMP_USE_AMBIENT;
            update[20] = 1; // AUTO: DU for black/white areas
            update[21] = 3; // AUTO: GL16 for gray areas
            // SAFETY: `update` has the size encoded in the request and outlives the call.
            if unsafe { libc::ioctl(self.file.as_raw_fd(), SEND_UPDATE as _, update.as_mut_ptr()) } == 0 {
                return;
            }
            eprintln!("direct refresh failed ({}), using the fbink tool", std::io::Error::last_os_error());
            self.via_fbink.set(true);
        }
        let region = format!("top={top},left={left},width={width},height={height}");
        match wave {
            Wave::Fast => fbink(&["-s", &region, "-W", &self.fast]),
            Wave::Quality => fbink(&["-s", &region, "-W", "GL16"]),
            Wave::Full => fbink(&["-s", &region, "-W", "GC16", "-f"]),
        }
    }
}

impl Drop for Fb {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly what open() mapped.
        unsafe { libc::munmap(self.map.cast(), self.len) };
    }
}

const fn ioc(dir: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | ((b'E' as u32) << 8) | nr
}
const ABS_MT_POSITION_X: u32 = 0x35;
const ABS_MT_POSITION_Y: u32 = 0x36;

/// The touch screen, grabbed so the Kindle UI does not also react to our taps.
pub struct TouchScreen {
    file: File,
    parser: MtParser,
    max_x: i32,
    max_y: i32,
    fb_w: i32,
    fb_h: i32,
}

impl TouchScreen {
    pub fn open(fb_w: i32, fb_h: i32) -> anyhow::Result<Self> {
        for entry in fs::read_dir("/dev/input")?.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("event") {
                continue;
            }
            let Ok(file) = File::open(entry.path()) else { continue };
            let fd = file.as_raw_fd();
            let mut abs_bits = [0u8; 8];
            let mut info = [[0i32; 6]; 2]; // struct input_absinfo: value, minimum, maximum, ...
            // SAFETY: buffers match the sizes encoded in the ioctl requests.
            let multitouch = unsafe {
                libc::ioctl(fd, ioc(2, 0x20 + 3, 8) as _, abs_bits.as_mut_ptr()) >= 0
                    && abs_bits[ABS_MT_POSITION_X as usize / 8] & (1 << (ABS_MT_POSITION_X % 8)) != 0
                    && libc::ioctl(fd, ioc(2, 0x40 + ABS_MT_POSITION_X, 24) as _, info[0].as_mut_ptr()) >= 0
                    && libc::ioctl(fd, ioc(2, 0x40 + ABS_MT_POSITION_Y, 24) as _, info[1].as_mut_ptr()) >= 0
            };
            if !multitouch || info[0][2] <= 0 || info[1][2] <= 0 {
                continue;
            }
            // SAFETY: EVIOCGRAB takes an int argument.
            unsafe { libc::ioctl(fd, ioc(1, 0x90, 4) as _, 1 as libc::c_int) };
            return Ok(Self { file, parser: MtParser::default(), max_x: info[0][2], max_y: info[1][2], fb_w, fb_h });
        }
        bail!("no touch screen found in /dev/input")
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Read what is available; positions come out in session coordinates.
    pub fn read(&mut self, out: &mut Vec<Touch>) -> anyhow::Result<()> {
        // struct input_event: two kernel longs of timestamp, then type, code, value.
        const TIME: usize = 2 * size_of::<libc::c_ulong>();
        let mut buf = [0u8; (TIME + 8) * 32];
        let n = self.file.read(&mut buf).context("read touch screen")?;
        let mut raw = Vec::new();
        for ev in buf[..n].chunks_exact(TIME + 8) {
            let ev = &ev[TIME..];
            let kind = u16::from_ne_bytes([ev[0], ev[1]]);
            let code = u16::from_ne_bytes([ev[2], ev[3]]);
            let value = i32::from_ne_bytes([ev[4], ev[5], ev[6], ev[7]]);
            self.parser.feed(kind, code, value, &mut raw);
        }
        let rotate = |x: i32, y: i32| {
            let fx = (x as i64 * self.fb_w as i64 / (self.max_x as i64 + 1)) as i32;
            let fy = (y as i64 * self.fb_h as i64 / (self.max_y as i64 + 1)) as i32;
            (fy, self.fb_w - 1 - fx)
        };
        out.extend(raw.into_iter().map(|t| match t {
            Touch::Down { slot, x, y } => {
                let (x, y) = rotate(x, y);
                Touch::Down { slot, x, y }
            }
            Touch::Move { slot, x, y } => {
                let (x, y) = rotate(x, y);
                Touch::Move { slot, x, y }
            }
            up => up,
        }));
        Ok(())
    }
}

/// Wait until one of the fds is readable or the timeout passes. Returns per-fd readiness.
pub fn poll(fds: [RawFd; 2], timeout_ms: Option<u64>) -> anyhow::Result<[bool; 2]> {
    let mut p = fds.map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
    let timeout = timeout_ms.map_or(-1, |t| t.min(60_000) as libc::c_int);
    // SAFETY: `p` is a valid array of two pollfd.
    let n = unsafe { libc::poll(p.as_mut_ptr(), 2, timeout) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            bail!("poll: {e}");
        }
        return Ok([false; 2]);
    }
    Ok(p.map(|p| p.revents != 0))
}
