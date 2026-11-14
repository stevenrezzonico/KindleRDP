//! Touch handling: multitouch protocol B parser and the gesture state machine. Pure logic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touch {
    Down { slot: usize, x: i32, y: i32 },
    Move { slot: usize, x: i32, y: i32 },
    Up { slot: usize },
}

const EV_SYN: u16 = 0;
const EV_ABS: u16 = 3;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const SLOTS: usize = 2;

#[derive(Clone, Copy, Default)]
struct Slot {
    active: bool,
    was_active: bool,
    x: i32,
    y: i32,
    moved: bool,
}

/// Turns raw evdev events into Touch events, one batch per SYN_REPORT.
#[derive(Default)]
pub struct MtParser {
    slots: [Slot; SLOTS],
    cur: usize,
}

impl MtParser {
    pub fn feed(&mut self, kind: u16, code: u16, value: i32, out: &mut Vec<Touch>) {
        if kind == EV_ABS {
            if code == ABS_MT_SLOT {
                self.cur = value as usize;
                return;
            }
            let Some(s) = self.slots.get_mut(self.cur) else { return }; // third finger and up: ignored
            match code {
                ABS_MT_TRACKING_ID => s.active = value >= 0,
                ABS_MT_POSITION_X => (s.x, s.moved) = (value, true),
                ABS_MT_POSITION_Y => (s.y, s.moved) = (value, true),
                _ => {}
            }
        } else if kind == EV_SYN && code == 0 {
            for (slot, s) in self.slots.iter_mut().enumerate() {
                match (s.was_active, s.active) {
                    (false, true) => out.push(Touch::Down { slot, x: s.x, y: s.y }),
                    (true, true) if s.moved => out.push(Touch::Move { slot, x: s.x, y: s.y }),
                    (true, false) => out.push(Touch::Up { slot }),
                    _ => {}
                }
                s.was_active = s.active;
                s.moved = false;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move(i32, i32),
    Left(bool),
    Right(bool),
    /// Wheel notches, positive scrolls up.
    Wheel(i32),
    Exit,
    FullRefresh,
}

const SLOP: i32 = 24;
const LONG_MS: u64 = 500;
const CORNER: i32 = 120;
const WHEEL_STEP: i32 = 48;

enum State {
    Idle,
    /// Finger down, not yet a drag.
    Pending { x: i32, y: i32, t: u64 },
    Drag,
    Scroll { last_y: i32 },
}

/// Tap = left click. Hold 0.5 s then lift = right click. Drag = left drag. Two fingers = wheel.
/// Hold in the top-left corner = exit, top-right corner = full refresh.
pub struct Gestures {
    state: State,
    width: i32,
}

impl Gestures {
    pub fn new(width: i32) -> Self {
        Self { state: State::Idle, width }
    }

    pub fn feed(&mut self, ev: Touch, now: u64, out: &mut Vec<Action>) {
        match (&self.state, ev) {
            (State::Idle, Touch::Down { slot: 0, x, y }) => self.state = State::Pending { x, y, t: now },
            (State::Pending { x, y, .. }, Touch::Down { slot: 1, .. }) => {
                out.push(Action::Move(*x, *y)); // wheel goes to the window under the first finger
                self.state = State::Scroll { last_y: *y };
            }
            (State::Pending { x: sx, y: sy, .. }, Touch::Move { slot: 0, x, y }) => {
                if (x - sx).abs() > SLOP || (y - sy).abs() > SLOP {
                    out.extend([Action::Move(*sx, *sy), Action::Left(true), Action::Move(x, y)]);
                    self.state = State::Drag;
                }
            }
            (State::Pending { x, y, t }, Touch::Up { slot: 0 }) => {
                let (x, y, long) = (*x, *y, now - *t >= LONG_MS);
                if long && y < CORNER && x < CORNER {
                    out.push(Action::Exit);
                } else if long && y < CORNER && x >= self.width - CORNER {
                    out.push(Action::FullRefresh);
                } else if long {
                    out.extend([Action::Move(x, y), Action::Right(true), Action::Right(false)]);
                } else {
                    out.extend([Action::Move(x, y), Action::Left(true), Action::Left(false)]);
                }
                self.state = State::Idle;
            }
            (State::Drag, Touch::Move { slot: 0, x, y }) => out.push(Action::Move(x, y)),
            (State::Drag, Touch::Up { slot: 0 }) => {
                out.push(Action::Left(false));
                self.state = State::Idle;
            }
            (State::Scroll { last_y }, Touch::Move { slot: 0, y, .. }) => {
                let notches = (y - last_y) / WHEEL_STEP;
                if notches != 0 {
                    out.push(Action::Wheel(notches)); // finger down = content down = wheel up
                    self.state = State::Scroll { last_y: last_y + notches * WHEEL_STEP };
                }
            }
            (State::Scroll { .. }, Touch::Up { slot: 0 }) => self.state = State::Idle,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Action::*;
    use super::*;

    fn run(evs: &[(Touch, u64)]) -> Vec<Action> {
        let mut g = Gestures::new(1648);
        let mut out = Vec::new();
        for (e, t) in evs {
            g.feed(*e, *t, &mut out);
        }
        out
    }

    const UP: Touch = Touch::Up { slot: 0 };
    fn down(x: i32, y: i32) -> Touch {
        Touch::Down { slot: 0, x, y }
    }
    fn mv(x: i32, y: i32) -> Touch {
        Touch::Move { slot: 0, x, y }
    }

    #[test]
    fn tap_and_long_press() {
        assert_eq!(run(&[(down(300, 400), 0), (mv(305, 402), 50), (UP, 100)]), [Move(300, 400), Left(true), Left(false)]);
        assert_eq!(run(&[(down(300, 400), 0), (UP, 600)]), [Move(300, 400), Right(true), Right(false)]);
    }

    #[test]
    fn drag() {
        assert_eq!(
            run(&[(down(300, 400), 0), (mv(400, 400), 50), (mv(500, 450), 90), (UP, 900)]),
            [Move(300, 400), Left(true), Move(400, 400), Move(500, 450), Left(false)]
        );
    }

    #[test]
    fn corners() {
        assert_eq!(run(&[(down(10, 10), 0), (UP, 700)]), [Exit]);
        assert_eq!(run(&[(down(1640, 10), 0), (UP, 700)]), [FullRefresh]);
        // a short tap in the corner is still a click
        assert_eq!(run(&[(down(10, 10), 0), (UP, 100)]), [Move(10, 10), Left(true), Left(false)]);
    }

    #[test]
    fn two_finger_scroll() {
        let second = Touch::Down { slot: 1, x: 700, y: 500 };
        assert_eq!(
            run(&[(down(600, 500), 0), (second, 10), (mv(600, 600), 50), (mv(600, 500), 90), (Touch::Up { slot: 1 }, 95), (UP, 100)]),
            [Move(600, 500), Wheel(2), Wheel(-2)]
        );
    }

    #[test]
    fn mt_parser() {
        let mut p = MtParser::default();
        let mut out = Vec::new();
        for (k, c, v) in [(3, 0x2f, 0), (3, 0x39, 7), (3, 0x35, 100), (3, 0x36, 200), (0, 0, 0)] {
            p.feed(k, c, v, &mut out);
        }
        assert_eq!(out, [Touch::Down { slot: 0, x: 100, y: 200 }]);
        out.clear();
        for (k, c, v) in [(3, 0x35, 110), (0, 0, 0), (0, 0, 0), (3, 0x2f, 5), (3, 0x39, 9), (0, 0, 0), (3, 0x2f, 0), (3, 0x39, -1), (0, 0, 0)] {
            p.feed(k, c, v, &mut out);
        }
        assert_eq!(out, [Touch::Move { slot: 0, x: 110, y: 200 }, Touch::Up { slot: 0 }]);
    }
}
