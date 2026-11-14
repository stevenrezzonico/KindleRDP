//! The app's own screens: drawing into a gray canvas, layout and hit-testing. Pure logic;
//! showing a canvas on the panel is app.rs.

use spleen_font::{FONT_16X32, FONT_32X64, PSF2Font};

use crate::rects::Rect;
use crate::servers::Server;

const BLACK: u8 = 0;
const WHITE: u8 = 255;

#[derive(Clone, Copy)]
pub enum Font {
    /// 32x64 pixels per character.
    Big,
    /// 16x32 pixels per character.
    Small,
}

impl Font {
    fn cell(self) -> (i32, i32) {
        match self {
            Font::Big => (32, 64),
            Font::Small => (16, 32),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub buf: Vec<u8>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Self { w, h, buf: vec![WHITE; (w * h) as usize] }
    }

    pub fn fill(&mut self, r: Rect, value: u8) {
        for y in r.y.max(0)..(r.y + r.h).min(self.h) {
            let row = (y * self.w) as usize;
            let (x0, x1) = (r.x.max(0), (r.x + r.w).min(self.w));
            if x0 < x1 {
                self.buf[row + x0 as usize..row + x1 as usize].fill(value);
            }
        }
    }

    fn frame(&mut self, r: Rect, t: i32) {
        self.fill(Rect { h: t, ..r }, BLACK);
        self.fill(Rect { y: r.y + r.h - t, h: t, ..r }, BLACK);
        self.fill(Rect { w: t, ..r }, BLACK);
        self.fill(Rect { x: r.x + r.w - t, w: t, ..r }, BLACK);
    }

    pub fn text(&mut self, x: i32, y: i32, font: Font, text: &str, color: u8) {
        let (cw, _) = font.cell();
        let mut psf = PSF2Font::new(match font {
            Font::Big => FONT_32X64,
            Font::Small => FONT_16X32,
        })
        .expect("embedded font");
        let mut utf8 = [0; 4];
        for (i, ch) in text.chars().enumerate() {
            let glyph = psf.glyph_for_utf8(ch.encode_utf8(&mut utf8).as_bytes()).or_else(|| psf.glyph_for_utf8(b"?"));
            let Some(glyph) = glyph else { continue };
            for (gy, row) in glyph.enumerate() {
                for (gx, on) in row.enumerate() {
                    let (px, py) = (x + i as i32 * cw + gx as i32, y + gy as i32);
                    if on && px >= 0 && px < self.w && py >= 0 && py < self.h {
                        self.buf[(py * self.w + px) as usize] = color;
                    }
                }
            }
        }
    }

    /// Text centered in `r`, cut to fit.
    fn label(&mut self, r: Rect, font: Font, text: &str, color: u8) {
        let (cw, ch) = font.cell();
        let shown: String = text.chars().take((r.w / cw).max(0) as usize).collect();
        let width = shown.chars().count() as i32 * cw;
        self.text(r.x + (r.w - width) / 2, r.y + (r.h - ch) / 2, font, &shown, color);
    }

    fn button(&mut self, r: Rect, text: &str, inverted: bool) {
        let r = Rect { x: r.x + 4, y: r.y + 4, w: r.w - 8, h: r.h - 8 };
        if inverted {
            self.fill(r, BLACK);
        } else {
            self.frame(r, 3);
        }
        let font = if text.chars().count() as i32 * 32 <= r.w - 8 { Font::Big } else { Font::Small };
        self.label(r, font, text, if inverted { WHITE } else { BLACK });
    }

    /// Bounding box of the pixels that differ from `other` (same size).
    pub fn diff(&self, other: &Canvas) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
        let w = self.w as usize;
        for (y, (a, b)) in self.buf.chunks_exact(w).zip(other.buf.chunks_exact(w)).enumerate() {
            if a == b {
                continue;
            }
            let first = a.iter().zip(b).position(|(p, q)| p != q).unwrap() as i32;
            let last = a.iter().zip(b).rposition(|(p, q)| p != q).unwrap() as i32;
            (x0, x1) = (x0.min(first), x1.max(last));
            (y0, y1) = (y0.min(y as i32), y as i32);
        }
        (x1 >= 0).then(|| Rect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 })
    }
}

pub type Areas<T> = Vec<(Rect, T)>;

pub fn hit<T: Copy>(areas: &Areas<T>, x: i32, y: i32) -> Option<T> {
    areas.iter().find(|(r, _)| r.contains(x, y)).map(|(_, t)| *t)
}

/// `count` equal buttons side by side across a row.
fn row_of(y: i32, h: i32, w: i32, count: i32) -> impl Iterator<Item = Rect> {
    (0..count).map(move |i| Rect { x: i * w / count, y, w: w / count, h })
}

const MARGIN: i32 = 24;
const BAR_H: i32 = 130;
const ROW_H: i32 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListHit {
    Connect(usize),
    Edit(usize),
    Add,
    Settings,
    Rotate,
    Exit,
    NextPage,
}

/// Servers that fit on one page of the list.
pub fn list_rows(c: &Canvas) -> usize {
    ((c.h - 2 * BAR_H) / ROW_H).max(1) as usize
}

pub fn list_screen(c: &mut Canvas, servers: &[Server], page: usize) -> Areas<ListHit> {
    let mut areas = Vec::new();
    c.text(MARGIN, (BAR_H - 64) / 2, Font::Big, "KindleRDP", BLACK);
    c.fill(Rect { x: 0, y: BAR_H - 4, w: c.w, h: 4 }, BLACK);
    if servers.is_empty() {
        c.text(MARGIN, BAR_H + 40, Font::Big, "No servers yet. Tap Add.", BLACK);
    }
    let rows = list_rows(c);
    for (slot, (index, server)) in servers.iter().enumerate().skip(page * rows).take(rows).enumerate() {
        let y = BAR_H + slot as i32 * ROW_H;
        let edit = Rect { x: c.w - 220, y: y + 15, w: 200, h: ROW_H - 30 };
        c.text(MARGIN, y + 14, Font::Big, &server.name, BLACK);
        c.text(MARGIN, y + 90, Font::Small, &format!("{}@{}", server.user, server.host), BLACK);
        c.fill(Rect { x: 0, y: y + ROW_H - 2, w: c.w, h: 2 }, BLACK);
        c.button(edit, "Edit", false);
        areas.push((edit, ListHit::Edit(index)));
        areas.push((Rect { x: 0, y, w: c.w - 220, h: ROW_H }, ListHit::Connect(index)));
    }
    let mut bar = vec![("Add", ListHit::Add), ("Settings", ListHit::Settings), ("Rotate", ListHit::Rotate), ("Exit", ListHit::Exit)];
    if servers.len() > rows {
        bar.insert(3, ("More", ListHit::NextPage));
    }
    for (r, (text, action)) in row_of(c.h - BAR_H, BAR_H, c.w, bar.len() as i32).zip(bar) {
        c.button(r, text, false);
        areas.push((r, action));
    }
    areas
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Shift,
    Symbols,
    Backspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Lower,
    Upper,
    Symbols,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormHit {
    Field(usize),
    Key(Key),
    Save,
    Cancel,
    Delete,
}

pub const FIELDS: [&str; 7] = ["Name", "Host", "Port", "User", "Domain", "Password", "Scale %"];
const PASSWORD: usize = 5;

/// The server editor's state.
#[derive(Clone)]
pub struct Form {
    pub fields: [String; 7],
    pub active: usize,
    pub layer: Layer,
    /// Shown under the buttons: what is wrong, or a hint.
    pub note: String,
    /// A password is already stored: the empty field shows stars and keeps it.
    saved_password: bool,
}

impl Form {
    pub fn new(server: Option<&Server>, saved_password: bool) -> Self {
        let fields = match server {
            Some(s) => {
                let scale = s.scale.map(|v| v.to_string()).unwrap_or_default();
                [s.name.clone(), s.host.clone(), s.port.to_string(), s.user.clone(), s.domain.clone(), String::new(), scale]
            }
            None => Default::default(),
        };
        let note = if saved_password { "Password is saved. Type to replace it." } else { "" };
        Self { fields, active: if server.is_some() { PASSWORD } else { 0 }, layer: Layer::Lower, note: note.into(), saved_password }
    }

    pub fn press(&mut self, key: Key) {
        match key {
            Key::Char(ch) => self.fields[self.active].push(ch),
            Key::Backspace => drop(self.fields[self.active].pop()),
            Key::Shift => self.layer = if self.layer == Layer::Upper { Layer::Lower } else { Layer::Upper },
            Key::Symbols => self.layer = if self.layer == Layer::Symbols { Layer::Lower } else { Layer::Symbols },
        }
    }
}

fn key_rows(layer: Layer) -> [&'static str; 4] {
    match layer {
        Layer::Lower => ["1234567890", "qwertyuiop", "asdfghjkl.", "zxcvbnm-_@"],
        Layer::Upper => ["1234567890", "QWERTYUIOP", "ASDFGHJKL.", "ZXCVBNM-_@"],
        Layer::Symbols => ["!\"#$%&'()*", "+,-./:;<=>", "?@[\\]^_`{|", "}~"],
    }
}

pub fn form_screen(c: &mut Canvas, form: &Form, can_delete: bool) -> Areas<FormHit> {
    const BUTTON_H: i32 = 100;
    const FIELD_H: i32 = 84;
    const LABEL_W: i32 = 180; // "Password" is 128 px wide after the margin
    let mut areas = Vec::new();

    let mut buttons = vec![("Save", FormHit::Save), ("Cancel", FormHit::Cancel)];
    if can_delete {
        buttons.push(("Delete", FormHit::Delete));
    }
    for (r, (text, action)) in row_of(0, BUTTON_H, c.w, buttons.len() as i32).zip(buttons) {
        c.button(r, text, false);
        areas.push((r, action));
    }
    c.text(MARGIN, BUTTON_H + 4, Font::Small, &form.note, BLACK);

    let fields_y = BUTTON_H + 40;
    for (i, label) in FIELDS.iter().enumerate() {
        let y = fields_y + i as i32 * FIELD_H;
        let boxed = Rect { x: LABEL_W, y: y + 4, w: c.w - LABEL_W - MARGIN, h: FIELD_H - 8 };
        c.text(MARGIN, y + (FIELD_H - 32) / 2, Font::Small, label, BLACK);
        c.frame(boxed, if i == form.active { 6 } else { 2 });
        let kept = i == PASSWORD && form.saved_password && form.fields[i].is_empty();
        let mut value = match i {
            PASSWORD if kept => "********".to_string(), // fixed length: says nothing about the real one
            PASSWORD => "*".repeat(form.fields[i].chars().count()),
            _ => form.fields[i].clone(),
        };
        if i == form.active && !kept {
            value.push('_');
        }
        // Long values show their end, where typing happens.
        let fit = ((boxed.w - 24) / 32) as usize;
        let tail: String = value.chars().skip(value.chars().count().saturating_sub(fit)).collect();
        c.text(boxed.x + 12, boxed.y + (boxed.h - 64) / 2, Font::Big, &tail, BLACK);
        areas.push((Rect { x: 0, y, w: c.w, h: FIELD_H }, FormHit::Field(i)));
    }

    // Keyboard: four rows of ten keys and one row of wide keys, filling the bottom.
    let top = fields_y + FIELDS.len() as i32 * FIELD_H + 8;
    let key_h = ((c.h - top) / 5).min(150);
    let top = c.h - 5 * key_h;
    for (row, keys) in key_rows(form.layer).iter().enumerate() {
        for (r, ch) in row_of(top + row as i32 * key_h, key_h, c.w, 10).zip(keys.chars()) {
            c.button(r, &ch.to_string(), false);
            areas.push((r, FormHit::Key(Key::Char(ch))));
        }
    }
    let unit = c.w / 10;
    let wide = [
        (0, 2, "Shift", Key::Shift, form.layer == Layer::Upper),
        (2, 2, "#+=", Key::Symbols, form.layer == Layer::Symbols),
        (4, 4, "space", Key::Char(' '), false),
        (8, 2, "<-", Key::Backspace, false),
    ];
    for (start, span, text, key, on) in wide {
        let r = Rect { x: start * unit, y: top + 4 * key_h, w: span * unit, h: key_h };
        c.button(r, text, on);
        areas.push((r, FormHit::Key(key)));
    }
    areas
}

/// Word-wraps `text` to `width` characters per line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let line = lines.last_mut().unwrap();
            let used = line.chars().count();
            let room = width.saturating_sub(used + usize::from(used > 0));
            if word.chars().count() <= room {
                if used > 0 {
                    line.push(' ');
                }
                line.push_str(word);
                break;
            }
            if used == 0 {
                // A word longer than a line: split it.
                let cut = word.char_indices().nth(width.max(1)).map_or(word.len(), |(i, _)| i);
                line.push_str(&word[..cut]);
                word = &word[cut..];
            }
            lines.push(String::new());
        }
    }
    lines
}

/// A message with up to five stacked buttons; returns each button's area in order.
pub fn dialog_screen(c: &mut Canvas, text: &str, buttons: &[&str]) -> Areas<usize> {
    let lines = wrap(text, ((c.w - 2 * MARGIN) / 32) as usize);
    for (i, line) in lines.iter().enumerate() {
        c.text(MARGIN, 80 + i as i32 * 72, Font::Big, line, BLACK);
    }
    let width = (c.w - 2 * MARGIN).min(900);
    let top = c.h - MARGIN - buttons.len() as i32 * 160;
    buttons
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let r = Rect { x: (c.w - width) / 2, y: top + i as i32 * 160, w: width, h: 150 };
            c.button(r, text, false);
            (r, i)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    /// Step row `0` (scale) or `1` (contrast) down or up.
    Step(usize, bool),
    Save,
    Cancel,
}

/// The Settings screen's state: the display options of config.ini.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsForm {
    pub scale: u32,
    gamma_tenths: u32,
}

impl SettingsForm {
    pub fn new(scale: u32, gamma: f32) -> Self {
        Self { scale, gamma_tenths: (gamma * 10.0).round() as u32 }
    }

    pub fn gamma(&self) -> f32 {
        self.gamma_tenths as f32 / 10.0
    }

    pub fn step(&mut self, row: usize, up: bool) {
        let moved = |value: u32, by: u32, low: u32, high: u32| if up { (value + by).min(high) } else { value.saturating_sub(by).max(low) };
        match row {
            0 => self.scale = moved(self.scale / 25 * 25, 25, 100, 500),
            _ => self.gamma_tenths = moved(self.gamma_tenths, 1, 5, 30),
        }
    }
}

pub fn settings_screen(c: &mut Canvas, form: &SettingsForm) -> Areas<SettingsHit> {
    const BUTTON_H: i32 = 100;
    const ROW: i32 = 190;
    const STEP_W: i32 = 170;
    let mut areas = Vec::new();
    for (r, (text, action)) in row_of(0, BUTTON_H, c.w, 2).zip([("Save", SettingsHit::Save), ("Cancel", SettingsHit::Cancel)]) {
        c.button(r, text, false);
        areas.push((r, action));
    }
    let rows = [
        ("Windows scale", format!("{} %", form.scale), "Size of text and icons in Windows"),
        ("Contrast", format!("{:.1}", form.gamma()), "Higher darkens pale grays"),
    ];
    for (i, (label, value, hint)) in rows.iter().enumerate() {
        let y = BUTTON_H + 40 + i as i32 * ROW;
        c.text(MARGIN, y + 16, Font::Big, label, BLACK);
        c.text(MARGIN, y + 92, Font::Small, hint, BLACK);
        let steps_x = c.w - 2 * STEP_W - MARGIN;
        c.text(steps_x - 20 - value.chars().count() as i32 * 32, y + 16, Font::Big, value, BLACK);
        for (n, (text, up)) in [("-", false), ("+", true)].into_iter().enumerate() {
            let r = Rect { x: steps_x + n as i32 * STEP_W, y: y + 4, w: STEP_W, h: ROW - 50 };
            c.button(r, text, false);
            areas.push((r, SettingsHit::Step(i, up)));
        }
        c.fill(Rect { x: 0, y: y + ROW - 14, w: c.w, h: 2 }, BLACK);
    }
    c.text(MARGIN, BUTTON_H + 60 + 2 * ROW, Font::Small, "Applies from the next connection", BLACK);
    areas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers(n: usize) -> Vec<Server> {
        (0..n).map(|i| Server { name: format!("pc{i}"), host: "h".into(), port: 3389, user: "u".into(), domain: String::new(), scale: None }).collect()
    }

    fn center(r: Rect) -> (i32, i32) {
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    #[test]
    fn list_hits_and_paging() {
        let mut c = Canvas::new(1648, 1236);
        let rows = list_rows(&c);
        let list = servers(rows + 2);
        let areas = list_screen(&mut c, &list, 0);
        assert_eq!(hit(&areas, 100, BAR_H + ROW_H + 10), Some(ListHit::Connect(1)));
        assert_eq!(hit(&areas, c.w - 100, BAR_H + ROW_H + 60), Some(ListHit::Edit(1)));
        assert_eq!(hit(&areas, 10, c.h - 10), Some(ListHit::Add));
        assert_eq!(hit(&areas, c.w - 10, c.h - 10), Some(ListHit::Exit));
        assert!(areas.iter().any(|(_, a)| *a == ListHit::NextPage));
        assert_eq!(hit(&areas, 100, 20), None);
        // second page holds the remaining two, with their real indexes
        let areas = list_screen(&mut Canvas::new(1648, 1236), &list, 1);
        assert_eq!(hit(&areas, 100, BAR_H + 10), Some(ListHit::Connect(rows)));
        assert_eq!(areas.iter().filter(|(_, a)| matches!(a, ListHit::Connect(_))).count(), 2);
        assert!(!list_screen(&mut Canvas::new(1236, 1648), &servers(2), 0).iter().any(|(_, a)| *a == ListHit::NextPage));
    }

    #[test]
    fn form_typing_in_both_orientations() {
        for (w, h) in [(1648, 1236), (1236, 1648)] {
            let mut form = Form::new(None, false);
            let tap = |form: &Form, want: FormHit| {
                let mut c = Canvas::new(w, h);
                let areas = form_screen(&mut c, form, false);
                let (r, _) = areas.iter().find(|(_, a)| *a == want).unwrap_or_else(|| panic!("{want:?} missing"));
                let (x, y) = center(*r);
                assert_eq!(hit(&areas, x, y), Some(want), "{want:?} is covered at {w}x{h}");
                assert!(r.y >= 0 && r.y + r.h <= h && r.x + r.w <= w, "{want:?} off screen at {w}x{h}");
            };
            for want in [FormHit::Save, FormHit::Cancel, FormHit::Field(0), FormHit::Field(6), FormHit::Key(Key::Char('1')), FormHit::Key(Key::Backspace)] {
                tap(&form, want);
            }
            form.press(Key::Char('p'));
            form.press(Key::Shift);
            tap(&form, FormHit::Key(Key::Char('C')));
            form.press(Key::Char('C'));
            form.press(Key::Symbols);
            tap(&form, FormHit::Key(Key::Char('\\')));
            form.press(Key::Char('~'));
            form.press(Key::Backspace);
            assert_eq!(form.fields[0], "pC");
            form.press(Key::Symbols);
            assert_eq!(form.layer, Layer::Lower);
        }
    }

    #[test]
    fn form_draws_only_what_changed() {
        let mut form = Form::new(Some(&servers(1)[0]), false);
        assert_eq!(form.active, PASSWORD);
        let mut before = Canvas::new(1648, 1236);
        form_screen(&mut before, &form, true);
        form.press(Key::Char('x'));
        let mut after = Canvas::new(1648, 1236);
        let areas = form_screen(&mut after, &form, true);
        let changed = after.diff(&before).unwrap();
        let (field, _) = areas.iter().find(|(_, a)| *a == FormHit::Field(PASSWORD)).unwrap();
        assert!(changed.y >= field.y && changed.y + changed.h <= field.y + field.h, "{changed:?}");
        assert_eq!(after.diff(&after.clone()), None);
        // the password itself is never drawn: same picture for another letter
        let mut other = form.clone();
        other.fields[PASSWORD] = "y".into();
        let mut masked = Canvas::new(1648, 1236);
        form_screen(&mut masked, &other, true);
        assert_eq!(masked.diff(&after), None);
    }

    #[test]
    fn saved_password_shows_stars_until_replaced_and_label_clears_the_box() {
        let draw = |form: &Form| {
            let mut c = Canvas::new(1648, 1236);
            form_screen(&mut c, form, true);
            c
        };
        let server = &servers(1)[0];
        let (saved, unsaved) = (Form::new(Some(server), true), Form::new(Some(server), false));
        assert!(draw(&saved).diff(&draw(&unsaved)).is_some(), "saved password is shown");
        let mut typed = unsaved.clone();
        typed.fields[PASSWORD] = "12345678".into();
        typed.active = 0;
        let mut resting = saved.clone();
        resting.active = 0;
        assert_eq!(draw(&resting).diff(&draw(&typed)).map(|r| r.y < 140), Some(true), "only the note differs");
        // typing replaces the stars with the new password's own
        let mut replaced = saved.clone();
        replaced.press(Key::Char('x'));
        assert_eq!(replaced.fields[PASSWORD], "x");
        assert!(draw(&replaced).diff(&draw(&saved)).is_some());
        // no label pixel reaches the field boxes: the column left of them is blank
        let c = draw(&unsaved);
        assert!((0..c.h).all(|y| (164..180).all(|x| c.buf[(y * c.w + x) as usize] == 255 || y < 140 || y > 650)));
    }

    #[test]
    fn wrapping_and_dialog() {
        assert_eq!(wrap("connection lost: timeout", 12), ["connection", "lost:", "timeout"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        for (w, h) in [(1236, 1648), (1648, 1236)] {
            let mut c = Canvas::new(w, h);
            let areas = dialog_screen(&mut c, "Menu", &["Keyboard", "Scale", "Disconnect", "Rotate", "Close"]);
            assert_eq!(areas.len(), 5);
            let (x, y) = center(areas[4].0);
            assert_eq!(hit(&areas, x, y), Some(4));
            assert!(areas[0].0.y > 80 + 72 && areas[4].0.y + areas[4].0.h <= h, "buttons clear the text at {w}x{h}");
        }
        let mut c = Canvas::new(1236, 1648);
        dialog_screen(&mut c, "Menu", &["Close"]);
        assert!(c.buf.contains(&0));
    }

    #[test]
    fn settings_steps_clamp() {
        let mut form = SettingsForm::new(200, 1.0);
        form.step(0, true);
        assert_eq!(form.scale, 225);
        for _ in 0..40 {
            form.step(0, false);
            form.step(1, true);
        }
        assert_eq!((form.scale, form.gamma()), (100, 3.0));
        form.step(1, false);
        assert_eq!(form.gamma(), 2.9);
        // hand-edited values snap to the steps
        let mut odd = SettingsForm::new(210, 1.45);
        odd.step(0, true);
        assert_eq!((odd.scale, odd.gamma()), (225, 1.5));
    }

    #[test]
    fn settings_screen_fits_both_orientations() {
        for (w, h) in [(1648, 1236), (1236, 1648)] {
            let mut c = Canvas::new(w, h);
            let areas = settings_screen(&mut c, &SettingsForm::new(200, 1.0));
            for want in [SettingsHit::Save, SettingsHit::Cancel, SettingsHit::Step(0, false), SettingsHit::Step(1, true)] {
                let (r, _) = areas.iter().find(|(_, a)| *a == want).unwrap();
                let (x, y) = center(*r);
                assert_eq!(hit(&areas, x, y), Some(want));
                assert!(r.x >= 0 && r.x + r.w <= w && r.y + r.h <= h, "{want:?} off screen at {w}x{h}");
            }
            // the longest label and value do not run into each other
            let label_end = MARGIN + 13 * 32;
            let value_start = w - 2 * 170 - MARGIN - 20 - 5 * 32;
            assert!(label_end < value_start, "{w}x{h}");
        }
    }
}
