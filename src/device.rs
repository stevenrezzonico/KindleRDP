//! Kindle hardware: framebuffer, e-ink refresh, touch screen, state files.

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context as _, bail};

use crate::config::TouchFix;
use crate::gesture::{MtParser, Touch};
use crate::rects::{Rect, Rotation, Wave};
use crate::ui::Canvas;

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

/// Show a line of text in the middle of the screen. Only for errors before the app's own
/// screens can draw.
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

/// MXCFB_SEND_UPDATE_MTK: _IOW('F', 0x2E, struct mxcfb_update_data_mtk), 24 u32 fields.
const SEND_UPDATE: u32 = (1 << 30) | (96 << 16) | ((b'F' as u32) << 8) | 0x2E;
const TEMP_USE_AMBIENT: u32 = 0x1000;

/// The panel. Callers work in session coordinates; `rotation` maps them to the framebuffer.
// shortcut: 8-bit gray panels only; add 32bpp for color models.
pub struct Fb {
    file: File,
    map: *mut u8,
    len: usize,
    fb_w: i32,
    fb_h: i32,
    stride: usize,
    pub rotation: Rotation,
    gamma: [u8; 256],
    marker: Cell<u32>,
    /// Set after the direct ioctl failed once (not an MTK panel): use the fbink tool from then on.
    via_fbink: Cell<bool>,
}

impl Fb {
    pub fn set_gamma(&mut self, gamma: f32) {
        self.gamma = std::array::from_fn(|i| ((i as f32 / 255.0).powf(gamma) * 255.0).round() as u8);
    }

    /// `via_fbink` refreshes through the fbink tool from the start (`refresh = fbink`).
    pub fn open(gamma: f32, rotation: Rotation, via_fbink: bool) -> anyhow::Result<Self> {
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
        // For reports from Kindle models nobody has tested.
        let id = String::from_utf8_lossy(&fix.id);
        eprintln!("screen: '{}' {fb_w}x{fb_h}, {bpp} bpp, line {} bytes, rotate {}", id.trim_end_matches('\0'), fix.line_length, var[34]);
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
        let mut fb = Self {
            file: f,
            map: map.cast(),
            len,
            fb_w,
            fb_h,
            stride,
            rotation,
            gamma: [0; 256],
            marker: Cell::new(0),
            via_fbink: Cell::new(via_fbink),
        };
        fb.set_gamma(gamma);
        Ok(fb)
    }

    /// Size of the session (and of the app's screens) in the current rotation.
    pub fn size(&self) -> (i32, i32) {
        self.rotation.session_size(self.fb_w, self.fb_h)
    }

    pub fn fb_size(&self) -> (i32, i32) {
        (self.fb_w, self.fb_h)
    }

    /// Write the pixels of `r` (session coordinates); `gray(x, y)` supplies each one.
    fn draw(&self, r: Rect, gray: impl Fn(usize, usize) -> u8) {
        // SAFETY: the mapping is `len` bytes and lives as long as self.
        let fb = unsafe { std::slice::from_raw_parts_mut(self.map, self.len) };
        for y in r.y as usize..(r.y + r.h) as usize {
            for x in r.x as usize..(r.x + r.w) as usize {
                let at = match self.rotation {
                    Rotation::Portrait => y * self.stride + x,
                    Rotation::Landscape => x * self.stride + self.fb_w as usize - 1 - y,
                };
                fb[at] = gray(x, y);
            }
        }
    }

    /// Copy `r` from an RGBA image to the panel as gray, through the gamma curve.
    pub fn blit(&self, rgba: &[u8], img_w: usize, r: Rect) {
        self.draw(r, |x, y| {
            let px = &rgba[(y * img_w + x) * 4..][..3];
            self.gamma[((px[0] as u32 * 77 + px[1] as u32 * 150 + px[2] as u32 * 29) >> 8) as usize]
        });
    }

    /// The app's own screens are laid out for a 300 dpi panel. The small, coarser panels
    /// (600x800, 758x1024) get them drawn at twice the panel size and shown at half size.
    pub fn zoom(&self) -> i32 {
        if self.fb_w.min(self.fb_h) < 1000 { 2 } else { 1 }
    }

    /// Copy `r` of a canvas `zoom()` times the size of the panel (the app's own screens).
    /// Returns the panel area written.
    pub fn blit_gray(&self, canvas: &Canvas, r: Rect) -> Rect {
        let z = self.zoom();
        let (x, y) = (r.x / z, r.y / z);
        let out = Rect { x, y, w: (r.x + r.w + z - 1) / z - x, h: (r.y + r.h + z - 1) / z - y };
        if z == 1 {
            self.draw(out, |x, y| canvas.buf[y * canvas.w as usize + x]);
        } else {
            self.draw(out, |x, y| canvas.reduced(x as i32, y as i32, z));
        }
        out
    }

    pub fn refresh(&self, r: Rect, wave: Wave) {
        let Rect { x: left, y: top, w: width, h: height } = self.rotation.to_fb(r, self.fb_w);
        if !self.via_fbink.get() {
            let (mode, full) = match wave {
                // MTK_WAVEFORM_MODE_* from the Kindle kernel's hwtcon_ioctl_cmd.h
                Wave::Fast => (3, 0), // GL16
                Wave::Full => (2, 1), // GC16, flashing
                Wave::Mono => (1, 0), // DU
            };
            self.marker.set(self.marker.get().wrapping_add(1).max(1));
            let mut update = [0u32; 24];
            update[..7].copy_from_slice(&[top as u32, left as u32, width as u32, height as u32, mode, full, self.marker.get()]);
            update[7] = TEMP_USE_AMBIENT;
            // SAFETY: `update` has the size encoded in the request and outlives the call.
            if unsafe { libc::ioctl(self.file.as_raw_fd(), SEND_UPDATE as _, update.as_mut_ptr()) } == 0 {
                return;
            }
            eprintln!("direct refresh failed ({}), using the fbink tool", std::io::Error::last_os_error());
            self.via_fbink.set(true);
        }
        let region = format!("top={top},left={left},width={width},height={height}");
        match wave {
            Wave::Fast => fbink(&["-s", &region, "-W", "GL16"]),
            Wave::Full => fbink(&["-s", &region, "-W", "GC16", "-f"]),
            Wave::Mono => fbink(&["-s", &region, "-W", "DU"]),
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
    fix: TouchFix,
}

impl TouchScreen {
    pub fn open(fb_w: i32, fb_h: i32, fix: TouchFix, device: Option<&str>) -> anyhow::Result<Self> {
        let mut paths: Vec<PathBuf> = match device {
            Some(path) => vec![path.into()],
            None => fs::read_dir("/dev/input")?.flatten().map(|e| e.path()).collect(),
        };
        paths.retain(|p| device.is_some() || p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event")));
        paths.sort(); // the same choice on every start
        for path in paths {
            let Ok(file) = File::open(&path) else { continue };
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
            let mut name = [0u8; 64];
            // SAFETY: EVIOCGNAME writes at most the 64 bytes encoded in the request; EVIOCGRAB takes an int.
            unsafe {
                libc::ioctl(fd, ioc(2, 0x06, 64) as _, name.as_mut_ptr());
                libc::ioctl(fd, ioc(1, 0x90, 4) as _, 1 as libc::c_int);
            }
            let name = String::from_utf8_lossy(&name);
            eprintln!("touch: {} '{}', x to {}, y to {}, {fix:?}", path.display(), name.trim_end_matches('\0'), info[0][2], info[1][2]);
            return Ok(Self { file, parser: MtParser::default(), max_x: info[0][2], max_y: info[1][2], fb_w, fb_h, fix });
        }
        bail!("no multitouch screen found (touch_device in config.ini names one)")
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Read what is available; positions come out in session coordinates for `rotation`.
    pub fn read(&mut self, rotation: Rotation, out: &mut Vec<Touch>) -> anyhow::Result<()> {
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
        let place = |x: i32, y: i32| {
            let (fx, fy) = self.fix.place((x, self.max_x), (y, self.max_y), self.fb_w, self.fb_h);
            rotation.point_from_fb(fx, fy, self.fb_w)
        };
        out.extend(raw.into_iter().map(|t| match t {
            Touch::Down { slot, x, y } => {
                let (x, y) = place(x, y);
                Touch::Down { slot, x, y }
            }
            Touch::Move { slot, x, y } => {
                let (x, y) = place(x, y);
                Touch::Move { slot, x, y }
            }
            up => up,
        }));
        Ok(())
    }
}

/// Wait until one of the fds is readable or the timeout passes. Returns per-fd readiness.
/// A negative fd is ignored.
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
