//! Dirty-rectangle merging and the e-ink refresh policy. Pure logic, no I/O.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn union(self, o: Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let r = (self.x + self.w).max(o.x + o.w);
        let b = (self.y + self.h).max(o.y + o.h);
        Rect { x, y, w: r - x, h: b - y }
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// True when the rects overlap or lie within `gap` pixels of each other.
    fn near(self, o: Rect, gap: i32) -> bool {
        self.x - gap < o.x + o.w && o.x - gap < self.x + self.w && self.y - gap < o.y + o.h && o.y - gap < self.y + self.h
    }
}

/// How the session is laid out on the panel. The framebuffer itself is always portrait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Portrait,
    /// Session (x, y) is framebuffer (fb_w - 1 - y, x).
    Landscape,
}

impl Rotation {
    pub fn toggled(self) -> Self {
        if self == Rotation::Portrait { Rotation::Landscape } else { Rotation::Portrait }
    }

    /// Session size; the width is a multiple of 4 as RDP servers expect.
    pub fn session_size(self, fb_w: i32, fb_h: i32) -> (i32, i32) {
        match self {
            Rotation::Portrait => (fb_w & !3, fb_h),
            Rotation::Landscape => (fb_h & !3, fb_w),
        }
    }

    /// A session rectangle in framebuffer coordinates.
    pub fn to_fb(self, r: Rect, fb_w: i32) -> Rect {
        match self {
            Rotation::Portrait => r,
            Rotation::Landscape => Rect { x: fb_w - r.y - r.h, y: r.x, w: r.h, h: r.w },
        }
    }

    /// A framebuffer point (a touch) in session coordinates.
    pub fn point_from_fb(self, x: i32, y: i32, fb_w: i32) -> (i32, i32) {
        match self {
            Rotation::Portrait => (x, y),
            Rotation::Landscape => (y, fb_w - 1 - x),
        }
    }
}

const GAP: i32 = 16;
const MAX_RECTS: usize = 6;

/// Add `r` to `list`, merging rects that touch. Collapses to one bounding box past MAX_RECTS:
/// many small e-ink refreshes cost more than one large one.
pub fn add(list: &mut Vec<Rect>, mut r: Rect) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    while let Some(i) = list.iter().position(|o| o.near(r, GAP)) {
        r = r.union(list.swap_remove(i));
    }
    list.push(r);
    if list.len() > MAX_RECTS {
        let all = list.iter().copied().reduce(Rect::union).unwrap();
        list.clear();
        list.push(all);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wave {
    /// 16 grays, no flash (GL16): every change of the session picture.
    Fast,
    /// Flashing full-screen update. Clears accumulated ghosting.
    Full,
    /// Black/white, quick: the app's own screens and keyboard.
    Mono,
}

const FLUSH_MS: u64 = 30;
/// How long one Fast update keeps its area busy.
const BUSY_MS: u64 = 450;
const FULL_BUSY_MS: u64 = 600;
const FULL_EVERY: u32 = 40;
const IDLE_FULL_MS: u64 = 1500;

/// Decides when to refresh which area with which waveform. Times are milliseconds.
///
/// The panel cannot restart an area that is still mid-update: such a request queues in the
/// driver and everything behind it lags. So an area is held back here until its previous
/// update is done, and by then all newer damage to it has merged into one refresh.
pub struct Scheduler {
    screen: Rect,
    pending: Vec<Rect>,
    first_pending: u64,
    inflight: Vec<(Rect, u64)>,
    last_flush: u64,
    flushes: u32,
    force_full: bool,
}

impl Scheduler {
    pub fn new(screen: Rect) -> Self {
        Self {
            screen,
            pending: Vec::new(),
            first_pending: 0,
            inflight: Vec::new(),
            last_flush: 0,
            flushes: 0,
            force_full: false,
        }
    }

    pub fn damage(&mut self, r: Rect, now: u64) {
        if self.pending.is_empty() {
            self.first_pending = now;
        }
        add(&mut self.pending, r);
    }

    pub fn request_full(&mut self) {
        self.force_full = true;
    }

    /// Milliseconds until `tick` has work, or None when idle.
    pub fn wait(&self, now: u64) -> Option<u64> {
        if self.force_full {
            Some(0)
        } else if !self.pending.is_empty() {
            let flush_at = self.first_pending + FLUSH_MS;
            if now < flush_at {
                Some(flush_at - now)
            } else {
                Some(self.inflight.iter().map(|(_, until)| until.saturating_sub(now)).min().unwrap_or(0))
            }
        } else if self.flushes >= FULL_EVERY {
            Some((self.last_flush + IDLE_FULL_MS).saturating_sub(now))
        } else {
            None
        }
    }

    fn full(&mut self, now: u64) -> Vec<(Rect, Wave)> {
        self.force_full = false;
        self.pending.clear();
        self.flushes = 0;
        self.inflight = vec![(self.screen, now + FULL_BUSY_MS)];
        vec![(self.screen, Wave::Full)]
    }

    /// Refreshes to perform now.
    pub fn tick(&mut self, now: u64) -> Vec<(Rect, Wave)> {
        self.inflight.retain(|(_, until)| *until > now);
        if self.force_full {
            return self.full(now);
        }
        if !self.pending.is_empty() {
            if now < self.first_pending + FLUSH_MS {
                return Vec::new();
            }
            let (blocked, ready): (Vec<Rect>, Vec<Rect>) =
                self.pending.drain(..).partition(|r| self.inflight.iter().any(|(busy, _)| busy.near(*r, 0)));
            self.pending = blocked;
            if ready.is_empty() {
                return Vec::new();
            }
            self.inflight.extend(ready.iter().map(|r| (*r, now + BUSY_MS)));
            self.last_flush = now;
            self.flushes += 1;
            return ready.into_iter().map(|r| (r, Wave::Fast)).collect();
        }
        if self.flushes >= FULL_EVERY && now >= self.last_flush + IDLE_FULL_MS {
            return self.full(now); // ghosting has built up and the screen is idle: clean it
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn rotation_maps_rects_and_points_consistently() {
        let (fb_w, fb_h) = (1236, 1648);
        assert_eq!(Rotation::Landscape.session_size(fb_w, fb_h), (1648, 1236));
        assert_eq!(Rotation::Portrait.session_size(fb_w, fb_h), (1236, 1648));
        assert_eq!(Rotation::Portrait.to_fb(r(5, 6, 7, 8), fb_w), r(5, 6, 7, 8));
        // top-left 10x20 of the landscape session is the top-right corner of the panel
        assert_eq!(Rotation::Landscape.to_fb(r(0, 0, 10, 20), fb_w), r(1216, 0, 20, 10));
        // a touch inside that panel area lands inside the session rect
        let (x, y) = Rotation::Landscape.point_from_fb(1220, 3, fb_w);
        assert!(r(0, 0, 10, 20).contains(x, y), "{x},{y}");
        assert_eq!(Rotation::Portrait.point_from_fb(3, 4, fb_w), (3, 4));
        assert_eq!(Rotation::Portrait.toggled().toggled(), Rotation::Portrait);
    }

    #[test]
    fn merges_touching_keeps_distant() {
        let mut l = Vec::new();
        add(&mut l, r(0, 0, 10, 10));
        add(&mut l, r(5, 5, 10, 10));
        assert_eq!(l, vec![r(0, 0, 15, 15)]);
        add(&mut l, r(500, 500, 10, 10));
        assert_eq!(l.len(), 2);
        add(&mut l, r(0, 0, 0, 5)); // empty: ignored
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn chain_merge_and_collapse() {
        let mut l = Vec::new();
        add(&mut l, r(0, 0, 10, 10));
        add(&mut l, r(60, 0, 10, 10));
        add(&mut l, r(20, 0, 30, 10)); // bridges both
        assert_eq!(l, vec![r(0, 0, 70, 10)]);
        let mut l = Vec::new();
        for i in 0..7 {
            add(&mut l, r(i * 100, i * 100, 10, 10));
        }
        assert_eq!(l, vec![r(0, 0, 610, 610)]);
    }

    #[test]
    fn single_change_shows_at_once_without_second_pass() {
        let mut s = Scheduler::new(r(0, 0, 1000, 1000));
        assert_eq!(s.wait(0), None);
        s.damage(r(0, 0, 10, 10), 1000);
        assert_eq!(s.wait(1000), Some(30));
        assert!(s.tick(1010).is_empty());
        assert_eq!(s.tick(1030), vec![(r(0, 0, 10, 10), Wave::Fast)]);
        assert_eq!(s.wait(1030), None);
        assert!(s.tick(5000).is_empty());
    }

    #[test]
    fn busy_area_waits_other_area_does_not() {
        let mut s = Scheduler::new(r(0, 0, 1000, 1000));
        s.damage(r(0, 0, 100, 100), 0);
        assert_eq!(s.tick(30).len(), 1); // busy until 480
        s.damage(r(50, 50, 10, 10), 100); // same area: must wait
        s.damage(r(500, 500, 10, 10), 100); // elsewhere: goes out
        assert_eq!(s.tick(130), vec![(r(500, 500, 10, 10), Wave::Fast)]);
        assert_eq!(s.wait(130), Some(350));
        s.damage(r(60, 60, 20, 20), 300); // merges into the waiting rect
        assert!(s.tick(479).is_empty());
        assert_eq!(s.tick(480), vec![(r(50, 50, 30, 30), Wave::Fast)]);
    }

    #[test]
    fn full_flash_when_idle_after_many_updates_and_on_request() {
        let screen = r(0, 0, 1000, 1000);
        let mut s = Scheduler::new(screen);
        let mut now = 0;
        for _ in 0..FULL_EVERY {
            s.damage(r(0, 0, 10, 10), now);
            now += BUSY_MS + 50;
            assert_eq!(s.tick(now).len(), 1);
        }
        assert_eq!(s.wait(now), Some(IDLE_FULL_MS));
        assert!(s.tick(now + 1000).is_empty());
        assert_eq!(s.tick(now + IDLE_FULL_MS), vec![(screen, Wave::Full)]);
        assert_eq!(s.wait(now + IDLE_FULL_MS), None);
        s.request_full();
        assert_eq!(s.wait(now), Some(0));
        assert_eq!(s.tick(now + 5000), vec![(screen, Wave::Full)]);
    }
}
