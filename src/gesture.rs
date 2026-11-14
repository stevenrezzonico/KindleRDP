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
const SLOTS: usize = 5;

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
            let Some(s) = self.slots.get_mut(self.cur) else { return }; // sixth finger and up: ignored
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
}

const SLOP: i32 = 24;
const LONG_MS: u64 = 500;
const WHEEL_STEP: i32 = 48;

enum State {
    Idle,
    /// Finger down, not yet a drag.
    Pending { x: i32, y: i32, t: u64 },
    Drag,
    Scroll { last_y: i32 },
}

/// Mouse emulation, used only when the server does not accept real touch input.
/// Tap = left click. Hold 0.5 s then lift = right click. Drag = left drag. Two fingers = wheel.
pub struct Gestures {
    state: State,
}

impl Gestures {
    pub fn new() -> Self {
        Self { state: State::Idle }
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
                if long {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Start,
    Move,
    End,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// A touch contact for Windows.
    Contact { slot: usize, phase: Phase, x: i32, y: i32 },
    /// Two-finger tap: a right mouse click here.
    RightClick { x: i32, y: i32 },
}

const PAIR_MS: u64 = 250;
const PAIR_TAP_MS: u64 = 450;
const HOLD_MS: u64 = 700;

#[derive(Clone, Copy, Default)]
struct Finger {
    down: bool,
    x: i32,
    y: i32,
    origin: (i32, i32),
    since: u64,
    /// Windows currently has this finger as a contact.
    sent: bool,
}

impl Finger {
    fn moved(&self) -> bool {
        (self.x - self.origin.0).abs() > SLOP || (self.y - self.origin.1).abs() > SLOP
    }
}

/// Decides which fingers Windows sees as touch contacts. Everything passes through, except
/// that two fingers landing together are held back until it is clear whether they tap
/// (a right click: Windows has no such touch-screen gesture), move (pinch: handed to Windows)
/// or rest (the app's own menu; the Kindle panel reports two fingers at most).
#[derive(Default)]
pub struct TouchRouter {
    fingers: [Finger; SLOTS],
    /// The two fingers being held back, and when the first landed.
    pair: Option<([usize; 2], u64)>,
}

impl TouchRouter {
    /// Contacts Windows has right now. A resting finger must be re-sent regularly:
    /// Windows drops a contact that gets no updates, which breaks press-and-hold.
    pub fn held(&self) -> impl Iterator<Item = (usize, i32, i32)> + '_ {
        self.fingers.iter().enumerate().filter(|(_, f)| f.sent).map(|(slot, f)| (slot, f.x, f.y))
    }

    /// When the two held-back fingers become a two-finger hold, if they stay down and still.
    pub fn hold_deadline(&self) -> Option<u64> {
        let (slots, start) = self.pair?;
        slots.iter().all(|s| self.fingers[*s].down).then_some(start + HOLD_MS)
    }

    /// True once when two fingers have rested long enough: open the menu. The fingers are
    /// forgotten, so lifting them does nothing.
    pub fn take_hold(&mut self, now: u64) -> bool {
        let due = self.hold_deadline().is_some_and(|deadline| now >= deadline);
        if due {
            self.clear();
        }
        due
    }

    /// Forget everything; the caller has cancelled the contacts on the Windows side.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Give the held-back fingers that are still down to Windows.
    fn hand_over(&mut self, out: &mut Vec<Routed>) {
        let Some((slots, _)) = self.pair.take() else { return };
        for slot in slots {
            let f = &mut self.fingers[slot];
            if f.down {
                f.sent = true;
                out.push(Routed::Contact { slot, phase: Phase::Start, x: f.x, y: f.y });
            }
        }
    }

    pub fn feed(&mut self, touch: Touch, now: u64, out: &mut Vec<Routed>) {
        let in_pair = |pair: &Option<([usize; 2], u64)>, slot| pair.is_some_and(|(slots, _)| slots.contains(&slot));
        match touch {
            Touch::Down { slot, x, y } => {
                let others: Vec<usize> = (0..SLOTS).filter(|s| *s != slot && self.fingers[*s].down).collect();
                self.fingers[slot] = Finger { down: true, x, y, origin: (x, y), since: now, sent: false };
                if let (None, [first]) = (&self.pair, others.as_slice()) {
                    let f = &mut self.fingers[*first];
                    if now - f.since <= PAIR_MS && !f.moved() {
                        if std::mem::take(&mut f.sent) {
                            out.push(Routed::Contact { slot: *first, phase: Phase::Cancel, x: f.x, y: f.y });
                        }
                        self.pair = Some(([*first, slot], f.since));
                        return;
                    }
                }
                self.hand_over(out); // a further finger: this is no two-finger tap
                self.fingers[slot].sent = true;
                out.push(Routed::Contact { slot, phase: Phase::Start, x, y });
            }
            Touch::Move { slot, x, y } => {
                let f = &mut self.fingers[slot];
                (f.x, f.y) = (x, y);
                if in_pair(&self.pair, slot) {
                    if f.moved() {
                        self.hand_over(out);
                    }
                } else if f.sent {
                    out.push(Routed::Contact { slot, phase: Phase::Move, x, y });
                }
            }
            Touch::Up { slot } => {
                let f = &mut self.fingers[slot];
                f.down = false;
                if std::mem::take(&mut f.sent) {
                    out.push(Routed::Contact { slot, phase: Phase::End, x: f.x, y: f.y });
                } else if let Some((slots, start)) = self.pair.filter(|(slots, _)| slots.contains(&slot)) {
                    if slots.iter().all(|s| !self.fingers[*s].down) {
                        self.pair = None;
                        if now - start <= PAIR_TAP_MS {
                            let (x, y) = self.fingers[slots[0]].origin;
                            out.push(Routed::RightClick { x, y });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Action::*;
    use super::*;

    fn run(evs: &[(Touch, u64)]) -> Vec<Action> {
        let mut g = Gestures::new();
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
        for (k, c, v) in [(3, 0x35, 110), (0, 0, 0), (0, 0, 0), (3, 0x2f, 7), (3, 0x39, 9), (0, 0, 0), (3, 0x2f, 0), (3, 0x39, -1), (0, 0, 0)] {
            p.feed(k, c, v, &mut out);
        }
        assert_eq!(out, [Touch::Move { slot: 0, x: 110, y: 200 }, Touch::Up { slot: 0 }]);
    }

    fn route(evs: &[(Touch, u64)]) -> (Vec<Routed>, TouchRouter) {
        let mut router = TouchRouter::default();
        let mut out = Vec::new();
        for (e, t) in evs {
            router.feed(*e, *t, &mut out);
        }
        (out, router)
    }

    fn contact(slot: usize, phase: Phase, x: i32, y: i32) -> Routed {
        Routed::Contact { slot, phase, x, y }
    }

    const DOWN2: Touch = Touch::Down { slot: 1, x: 400, y: 300 };
    const UP2: Touch = Touch::Up { slot: 1 };

    #[test]
    fn one_finger_passes_through_and_stays_held() {
        let (out, router) = route(&[(down(100, 100), 0), (mv(100, 200), 50)]);
        assert_eq!(out, [contact(0, Phase::Start, 100, 100), contact(0, Phase::Move, 100, 200)]);
        assert_eq!(router.held().collect::<Vec<_>>(), [(0, 100, 200)]);
        let (out, router) = route(&[(down(100, 100), 0), (UP, 2000)]);
        assert_eq!(out, [contact(0, Phase::Start, 100, 100), contact(0, Phase::End, 100, 100)]);
        assert_eq!(router.held().count(), 0);
    }

    #[test]
    fn two_finger_tap_is_a_right_click_and_windows_sees_no_tap() {
        let (out, router) = route(&[(down(300, 300), 0), (DOWN2, 40), (UP2, 150), (UP, 170)]);
        assert_eq!(out, [contact(0, Phase::Start, 300, 300), contact(0, Phase::Cancel, 300, 300), Routed::RightClick { x: 300, y: 300 }]);
        assert_eq!(router.held().count(), 0);
        // both fingers reported in the same instant, lifted in the other order
        let (out, _) = route(&[(down(300, 300), 0), (DOWN2, 0), (UP, 100), (UP2, 120)]);
        assert_eq!(out.last(), Some(&Routed::RightClick { x: 300, y: 300 }));
    }

    #[test]
    fn two_fingers_that_move_or_rest_go_to_windows() {
        // pinch: both handed over at their current positions once one moves
        let (out, router) = route(&[(down(300, 300), 0), (DOWN2, 40), (mv(250, 300), 90), (Touch::Move { slot: 1, x: 460, y: 300 }, 100)]);
        assert_eq!(
            out[2..],
            [contact(0, Phase::Start, 250, 300), contact(1, Phase::Start, 400, 300), contact(1, Phase::Move, 460, 300)]
        );
        assert_eq!(router.held().count(), 2);
        // resting too long is not a tap
        let (out, _) = route(&[(down(300, 300), 0), (DOWN2, 40), (UP2, 900), (UP, 910)]);
        assert!(!out.iter().any(|r| matches!(r, Routed::RightClick { .. })));
    }

    #[test]
    fn two_fingers_resting_open_the_menu_once() {
        let (out, mut router) = route(&[(down(300, 300), 0), (DOWN2, 40)]);
        assert_eq!(router.hold_deadline(), Some(HOLD_MS));
        assert!(!router.take_hold(HOLD_MS - 1));
        assert!(router.take_hold(HOLD_MS));
        assert!(!router.take_hold(HOLD_MS + 500));
        // lifting afterwards sends nothing: no right click, no contact
        let mut after = Vec::new();
        router.feed(UP2, 900, &mut after);
        router.feed(UP, 910, &mut after);
        assert_eq!(after, []);
        assert_eq!(out.len(), 2); // Start then Cancel of the first finger only
        // moving fingers never become a hold
        let (_, mut router) = route(&[(down(300, 300), 0), (DOWN2, 40), (mv(200, 300), 90)]);
        assert_eq!(router.hold_deadline(), None);
        assert!(!router.take_hold(5000));
        // one finger lifted: no hold either
        let (_, router) = route(&[(down(300, 300), 0), (DOWN2, 40), (UP2, 600)]);
        assert_eq!(router.hold_deadline(), None);
    }

    #[test]
    fn late_second_finger_is_a_normal_contact() {
        // first finger already scrolling, or simply down for a while
        let (out, _) = route(&[(down(300, 300), 0), (mv(300, 400), 30), (DOWN2, 60)]);
        assert_eq!(out.last(), Some(&contact(1, Phase::Start, 400, 300)));
        let (out, _) = route(&[(down(300, 300), 0), (DOWN2, 600), (UP2, 650), (UP, 660)]);
        assert_eq!(out[1..], [contact(1, Phase::Start, 400, 300), contact(1, Phase::End, 400, 300), contact(0, Phase::End, 300, 300)]);
    }
}
