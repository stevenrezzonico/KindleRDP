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

    /// True when the rects overlap or lie within `gap` pixels of each other.
    fn near(self, o: Rect, gap: i32) -> bool {
        self.x - gap < o.x + o.w && o.x - gap < self.x + self.w && self.y - gap < o.y + o.h && o.y - gap < self.y + self.h
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
    /// Quick, 1-bit, ghosts. Used while the screen is still changing.
    Fast,
    /// 16 grays, no flash. Cleans up an area once it stops changing.
    Quality,
    /// Flashing full-screen update. Clears accumulated ghosting.
    Full,
}

const FLUSH_MS: u64 = 120;
const SETTLE_MS: u64 = 600;
const FULL_EVERY: u32 = 20;

/// Decides when to refresh which area with which waveform. Times are milliseconds.
pub struct Scheduler {
    screen: Rect,
    pending: Vec<Rect>,
    first_pending: u64,
    unsettled: Option<Rect>,
    last_fast: u64,
    quality_passes: u32,
    force_full: bool,
}

impl Scheduler {
    pub fn new(screen: Rect) -> Self {
        Self { screen, pending: Vec::new(), first_pending: 0, unsettled: None, last_fast: 0, quality_passes: 0, force_full: false }
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
            Some((self.first_pending + FLUSH_MS).saturating_sub(now))
        } else if self.unsettled.is_some() {
            Some((self.last_fast + SETTLE_MS).saturating_sub(now))
        } else {
            None
        }
    }

    /// Refreshes to perform now.
    pub fn tick(&mut self, now: u64) -> Vec<(Rect, Wave)> {
        if self.force_full {
            self.force_full = false;
            self.pending.clear();
            self.unsettled = None;
            self.quality_passes = 0;
            return vec![(self.screen, Wave::Full)];
        }
        if !self.pending.is_empty() {
            if now < self.first_pending + FLUSH_MS {
                return Vec::new();
            }
            let out: Vec<_> = self.pending.drain(..).map(|r| (r, Wave::Fast)).collect();
            for (r, _) in &out {
                self.unsettled = Some(self.unsettled.map_or(*r, |u| u.union(*r)));
            }
            self.last_fast = now;
            return out;
        }
        if let Some(area) = self.unsettled
            && now >= self.last_fast + SETTLE_MS
        {
            self.unsettled = None;
            self.quality_passes += 1;
            if self.quality_passes >= FULL_EVERY {
                self.quality_passes = 0;
                return vec![(self.screen, Wave::Full)];
            }
            return vec![(area, Wave::Quality)];
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
    fn fast_then_quality_then_idle() {
        let mut s = Scheduler::new(r(0, 0, 1000, 1000));
        assert_eq!(s.wait(0), None);
        s.damage(r(0, 0, 10, 10), 1000);
        assert_eq!(s.wait(1000), Some(120));
        assert!(s.tick(1050).is_empty());
        assert_eq!(s.tick(1120), vec![(r(0, 0, 10, 10), Wave::Fast)]);
        // more damage before settling: area grows, settle timer restarts
        s.damage(r(100, 100, 10, 10), 1200);
        assert_eq!(s.tick(1320), vec![(r(100, 100, 10, 10), Wave::Fast)]);
        assert!(s.tick(1800).is_empty());
        assert_eq!(s.tick(1920), vec![(r(0, 0, 110, 110), Wave::Quality)]);
        assert_eq!(s.wait(2000), None);
    }

    #[test]
    fn full_flash_periodic_and_on_request() {
        let screen = r(0, 0, 1000, 1000);
        let mut s = Scheduler::new(screen);
        let mut now = 0;
        let mut last = Vec::new();
        for _ in 0..FULL_EVERY {
            s.damage(r(0, 0, 10, 10), now);
            now += FLUSH_MS;
            s.tick(now);
            now += SETTLE_MS;
            last = s.tick(now);
        }
        assert_eq!(last, vec![(screen, Wave::Full)]);
        s.request_full();
        assert_eq!(s.wait(now), Some(0));
        assert_eq!(s.tick(now), vec![(screen, Wave::Full)]);
    }
}
