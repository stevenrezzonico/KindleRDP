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

    /// Pixel (x, y) of this canvas shown at 1/z of its size. A block that is part black and
    /// part white takes the colour that is rarer around it: thin strokes then survive both
    /// as black on white and as white on black, and the result is still black and white.
    pub fn reduced(&self, x: i32, y: i32, z: i32) -> u8 {
        let dark = |x0: i32, y0: i32, size: i32| {
            let (xs, ys) = (x0.max(0)..(x0 + size).min(self.w), y0.max(0)..(y0 + size).min(self.h));
            let count = ys.clone().map(|y| xs.clone().filter(|x| self.buf[(y * self.w + x) as usize] < 128).count()).sum::<usize>();
            (count, xs.len() * ys.len())
        };
        match dark(x * z, y * z, z) {
            (0, _) => WHITE,
            (count, all) if count == all => BLACK,
            _ => {
                let (count, all) = dark((x - 2) * z, (y - 2) * z, 5 * z);
                if count * 2 > all { WHITE } else { BLACK }
            }
        }
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
        let name: String = server.name.chars().take(((edit.x - MARGIN) / 32).max(0) as usize).collect(); // stops before Edit
        c.text(MARGIN, y + 14, Font::Big, &name, BLACK);
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

/// Modifier bits that come with a typed key.
pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const WIN: u8 = 4;
pub const SHIFT: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Esc,
    Tab,
    Left,
    Down,
    Up,
    Right,
    Delete,
    Backspace,
    Enter,
    /// Ctrl, Alt or Win: stays on for the next key.
    Mod(u8),
    Shift,
    Symbols,
    Accents,
    /// Session only: put the keyboard at the other end of the screen.
    Move,
    /// Session only: close the keyboard.
    Hide,
}

impl Key {
    /// Keys that repeat while held.
    pub fn repeats(self) -> bool {
        matches!(self, Key::Backspace | Key::Delete | Key::Left | Key::Down | Key::Up | Key::Right)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layer {
    #[default]
    Letters,
    Symbols,
    Accents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shift {
    #[default]
    Off,
    /// For the next key only.
    Once,
    Locked,
}

/// The on-screen keyboard's state, shared by the server editor and the session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Keyboard {
    pub layer: Layer,
    pub shift: Shift,
    mods: u8,
    /// The key under a finger: drawn inverted.
    pub pressed: Option<Key>,
}

impl Keyboard {
    /// A key went down. Returns what to type, with its modifier bits, or None when the key
    /// only changed the keyboard itself.
    pub fn press(&mut self, key: Key) -> Option<(Key, u8)> {
        let flip = |layer: Layer, to: Layer| if layer == to { Layer::Letters } else { to };
        match key {
            Key::Shift => {
                self.shift = match self.shift {
                    Shift::Off => Shift::Once,
                    Shift::Once => Shift::Locked,
                    Shift::Locked => Shift::Off,
                }
            }
            Key::Symbols => self.layer = flip(self.layer, Layer::Symbols),
            Key::Accents => self.layer = flip(self.layer, Layer::Accents),
            Key::Mod(bit) => self.mods ^= bit,
            _ => {
                // A character is already drawn and sent in its shifted form.
                let shift = if self.shift != Shift::Off && !matches!(key, Key::Char(_)) { SHIFT } else { 0 };
                if self.shift == Shift::Once {
                    self.shift = Shift::Off;
                }
                return Some((key, std::mem::take(&mut self.mods) | shift));
            }
        }
        None
    }

    /// The four rows of character keys as shown now.
    fn rows(&self) -> [String; 4] {
        let rows = match self.layer {
            Layer::Letters => ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm,."],
            Layer::Symbols => ["1234567890", "!\"#$%&'()*", "+/:;<=>?@", "~[\\]^_`{|}"],
            // The font has no ë, ï, Å, « or »: a test checks every key for a glyph.
            Layer::Accents => ["€£§°¿¡¨´ÿœ", "àáâäãžæçèé", "êìíîñòóôö", "õøùúûüýšß"],
        };
        let upper = |ch: char| {
            let mut up = ch.to_uppercase();
            match (up.next(), up.next()) {
                (Some(one), None) if self.shift != Shift::Off => one,
                _ => ch, // ß has no single capital
            }
        };
        rows.map(|row| row.chars().map(upper).collect())
    }
}

/// Where `ch` sits on a US keyboard, as a scan code. Used for shortcuts such as Ctrl+C.
pub fn us_scancode(ch: char) -> Option<u8> {
    if ch == ' ' {
        return Some(0x39);
    }
    let rows = [("1234567890-", 0x02), ("qwertyuiop", 0x10), ("asdfghjkl", 0x1E), ("zxcvbnm,.", 0x2C)];
    rows.iter().find_map(|(row, first)| row.find(ch).map(|i| first + i as u8))
}

/// Draw the keyboard into `area`: four rows of ten keys and a row of wide keys. In a session
/// it has a row of special keys on top, and the keys to move and hide it.
pub fn keyboard(c: &mut Canvas, area: Rect, kb: &Keyboard, session: bool) -> Areas<Key> {
    let key_h = area.h / if session { 6 } else { 5 };
    // (row, first tenth, tenths wide, label, key, drawn as switched on)
    let mut keys: Vec<(i32, i32, i32, String, Key, bool)> = Vec::new();
    let mut row = 0;
    if session {
        let top = [
            ("Esc", Key::Esc),
            ("Tab", Key::Tab),
            ("Ctrl", Key::Mod(CTRL)),
            ("Alt", Key::Mod(ALT)),
            ("Win", Key::Mod(WIN)),
            ("<", Key::Left),
            ("v", Key::Down),
            ("^", Key::Up),
            (">", Key::Right),
            ("Del", Key::Delete),
        ];
        for (i, (text, key)) in top.into_iter().enumerate() {
            keys.push((0, i as i32, 1, text.into(), key, matches!(key, Key::Mod(bit) if kb.mods & bit != 0)));
        }
        row = 1;
    }
    for (i, chars) in kb.rows().iter().enumerate() {
        // A row of nine leaves room for Backspace at its end, or Shift at its start.
        let nine = chars.chars().count() == 9;
        let shift = nine && i == 3;
        if shift {
            let text = if kb.shift == Shift::Locked { "LOCK" } else { "Shift" };
            keys.push((row, 0, 1, text.into(), Key::Shift, kb.shift != Shift::Off));
        } else if nine {
            keys.push((row, 9, 1, "<-".into(), Key::Backspace, false));
        }
        for (n, ch) in chars.chars().enumerate() {
            keys.push((row, n as i32 + i32::from(shift), 1, ch.to_string(), Key::Char(ch), false));
        }
        row += 1;
    }
    let (symbols, accents) = (kb.layer == Layer::Symbols, kb.layer == Layer::Accents);
    let bottom: &[(i32, &str, Key, bool)] = if session {
        &[
            (1, "#+=", Key::Symbols, symbols),
            (1, "àéü", Key::Accents, accents),
            (3, "space", Key::Char(' '), false),
            (1, "-", Key::Char('-'), false),
            (2, "Enter", Key::Enter, false),
            (1, "Move", Key::Move, false),
            (1, "Hide", Key::Hide, false),
        ]
    } else {
        // In the editor Enter goes to the next field.
        &[
            (2, "#+=", Key::Symbols, symbols),
            (2, "àéü", Key::Accents, accents),
            (3, "space", Key::Char(' '), false),
            (1, "-", Key::Char('-'), false),
            (2, "Next", Key::Enter, false),
        ]
    };
    let mut start = 0;
    for (span, text, key, on) in bottom {
        keys.push((row, start, *span, text.to_string(), *key, *on));
        start += span;
    }
    keys.into_iter()
        .map(|(row, start, span, text, key, on)| {
            let r = Rect { x: area.x + start * area.w / 10, y: area.y + row * key_h, w: span * area.w / 10, h: key_h };
            c.button(r, &text, on || kb.pressed == Some(key));
            (r, key)
        })
        .collect()
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
    pub keys: Keyboard,
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
        Self { fields, active: if server.is_some() { PASSWORD } else { 0 }, keys: Keyboard::default(), note: note.into(), saved_password }
    }

    pub fn press(&mut self, key: Key) {
        match self.keys.press(key) {
            Some((Key::Char(ch), _)) => self.fields[self.active].push(ch),
            Some((Key::Backspace, _)) => drop(self.fields[self.active].pop()),
            Some((Key::Enter, _)) => self.active = (self.active + 1) % FIELDS.len(),
            _ => {}
        }
    }
}

pub fn form_screen(c: &mut Canvas, form: &Form, can_delete: bool) -> Areas<FormHit> {
    const BUTTON_H: i32 = 100;
    // A short screen (1448x1072 lying down) gives up field height so the keys stay 80 high.
    let field_h = ((c.h - BUTTON_H - 48 - 5 * 80) / FIELDS.len() as i32).clamp(72, 84);
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
        let y = fields_y + i as i32 * field_h;
        let boxed = Rect { x: LABEL_W, y: y + 4, w: c.w - LABEL_W - MARGIN, h: field_h - 8 };
        c.text(MARGIN, y + (field_h - 32) / 2, Font::Small, label, BLACK);
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
        areas.push((Rect { x: 0, y, w: c.w, h: field_h }, FormHit::Field(i)));
    }

    // The keyboard fills the bottom.
    let top = fields_y + FIELDS.len() as i32 * field_h + 8;
    let key_h = ((c.h - top) / 5).min(150);
    let area = Rect { x: 0, y: c.h - 5 * key_h, w: c.w, h: 5 * key_h };
    areas.extend(keyboard(c, area, &form.keys, false).into_iter().map(|(r, key)| (r, FormHit::Key(key))));
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
            tap(&form, FormHit::Key(Key::Char('c'))); // shift was for one letter
            form.press(Key::Symbols);
            tap(&form, FormHit::Key(Key::Char('\\')));
            form.press(Key::Char('~'));
            form.press(Key::Backspace);
            assert_eq!(form.fields[0], "pC");
            form.press(Key::Symbols);
            assert_eq!(form.keys.layer, Layer::Letters);
            form.press(Key::Accents);
            tap(&form, FormHit::Key(Key::Char('é')));
            form.press(Key::Enter);
            assert_eq!(form.active, 1);
        }
    }

    /// Every Kindle panel size, as the canvas the screens are drawn on: the two small panels
    /// doubled (`Fb::zoom`), the others as they are.
    const CANVASES: [(i32, i32); 7] = [(1200, 1600), (1512, 2048), (1516, 2048), (1072, 1448), (1236, 1648), (1264, 1680), (1860, 2480)];

    #[test]
    fn every_screen_fits_every_kindle_panel() {
        fn check<T: Copy + PartialEq + std::fmt::Debug>(what: &str, (w, h): (i32, i32), areas: &Areas<T>) {
            for (r, action) in areas {
                let (x, y) = center(*r);
                assert_eq!(hit(areas, x, y), Some(*action), "{what} {action:?} is covered at {w}x{h}");
                assert!(r.x >= 0 && r.y >= 0 && r.x + r.w <= w && r.y + r.h <= h, "{what} {action:?} off screen at {w}x{h}");
            }
        }
        for (w, h) in CANVASES.into_iter().flat_map(|(w, h)| [(w, h), (h, w)]) {
            let canvas = || Canvas::new(w, h);
            let mut c = canvas();
            check("list", (w, h), &list_screen(&mut c, &servers(20), 0));
            let mut c = canvas();
            check("settings", (w, h), &settings_screen(&mut c, &SettingsForm::new(200, 1.0)));
            let mut c = canvas();
            let dialog = dialog_screen(&mut c, "The certificate of pc0 changed.", &["Keyboard", "Scale", "Disconnect", "Rotate", "Close"]);
            check("dialog", (w, h), &dialog);
            assert!(dialog[0].0.y > 80 + 72, "dialog buttons over the text at {w}x{h}");
            let mut c = canvas();
            let form = form_screen(&mut c, &Form::new(None, false), true);
            check("editor", (w, h), &form);
            let fields_end = form.iter().filter(|(_, a)| matches!(a, FormHit::Field(_))).map(|(r, _)| r.y + r.h).max().unwrap();
            for (r, action) in &form {
                // a big glyph is 64 high and a button's frame takes 4 on each side
                let need = 64 + 8;
                assert!(r.h >= need, "editor {action:?} is {} high at {w}x{h}", r.h);
                assert!(!matches!(action, FormHit::Key(_)) || r.y >= fields_end, "editor {action:?} over the fields at {w}x{h}");
            }
            // the session keyboard, sized as rdp.rs does on a panel that is not doubled
            let key_h = (h * 9 / 20 / 6).min(110);
            let mut c = canvas();
            let keys = keyboard(&mut c, Rect { x: 0, y: h - 6 * key_h, w, h: 6 * key_h }, &Keyboard::default(), true);
            check("keyboard", (w, h), &keys);
            assert!(keys.iter().all(|(r, _)| r.h >= 64 + 8 && r.w >= 32 + 16), "keyboard keys too small at {w}x{h}");
        }
    }

    #[test]
    fn half_size_keeps_thin_strokes_black_or_white() {
        // a 2 px black line on white, off the 2x2 grid, and the same in white on black
        let mut c = Canvas::new(40, 40);
        c.fill(Rect { x: 0, y: 20, w: 40, h: 20 }, BLACK);
        c.fill(Rect { x: 9, y: 0, w: 2, h: 16 }, BLACK);
        c.fill(Rect { x: 9, y: 24, w: 2, h: 16 }, WHITE);
        assert_eq!((c.reduced(2, 2, 2), c.reduced(4, 2, 2), c.reduced(5, 2, 2)), (WHITE, BLACK, BLACK));
        assert_eq!((c.reduced(2, 16, 2), c.reduced(4, 16, 2), c.reduced(5, 16, 2)), (BLACK, WHITE, WHITE));
        assert_eq!(c.reduced(9, 0, 1), BLACK);
    }

    #[test]
    fn keyboard_shift_modifiers_and_layers() {
        let mut kb = Keyboard::default();
        assert_eq!(kb.press(Key::Shift), None);
        assert_eq!(kb.rows()[1], "QWERTYUIOP");
        assert_eq!(kb.press(Key::Char('Q')), Some((Key::Char('Q'), 0)));
        assert_eq!(kb.rows()[1], "qwertyuiop");
        // two taps lock it; a special key then carries Shift, a character does not
        kb.press(Key::Shift);
        kb.press(Key::Shift);
        assert_eq!(kb.shift, Shift::Locked);
        assert_eq!(kb.press(Key::Left), Some((Key::Left, SHIFT)));
        assert_eq!(kb.press(Key::Char('A')), Some((Key::Char('A'), 0)));
        assert_eq!(kb.shift, Shift::Locked);
        kb.press(Key::Shift);
        // Ctrl and Alt stay on for one key
        kb.press(Key::Mod(CTRL));
        kb.press(Key::Mod(ALT));
        assert_eq!(kb.press(Key::Delete), Some((Key::Delete, CTRL | ALT)));
        assert_eq!(kb.press(Key::Char('c')), Some((Key::Char('c'), 0)));
        kb.press(Key::Mod(WIN));
        kb.press(Key::Mod(WIN)); // a second tap takes it back
        assert_eq!(kb.press(Key::Tab), Some((Key::Tab, 0)));
        // ß stays when shifted
        kb.press(Key::Accents);
        kb.press(Key::Shift);
        assert!(kb.rows()[3].ends_with("ÝŠß"), "{}", kb.rows()[3]);
        assert_eq!((us_scancode('c'), us_scancode('-'), us_scancode('.'), us_scancode(' '), us_scancode('é')), (Some(0x2E), Some(0x0C), Some(0x34), Some(0x39), None));
    }

    #[test]
    fn session_keyboard_fits_and_every_key_has_a_glyph() {
        for (w, h) in [(1648, 1236), (1236, 1648)] {
            let area = Rect { x: 0, y: h - 600, w, h: 600 };
            for layer in [Key::Shift, Key::Symbols, Key::Accents] {
                let mut kb = Keyboard::default();
                kb.press(layer);
                for shifted in [false, true] {
                    if shifted {
                        kb.press(Key::Shift);
                    }
                    let mut c = Canvas::new(w, h);
                    let areas = keyboard(&mut c, area, &kb, true);
                    assert_eq!(areas.len(), 10 + 40 + 7, "{layer:?} {shifted}");
                    let mut missing = String::new();
                    for (r, key) in &areas {
                        let (x, y) = center(*r);
                        assert_eq!(hit(&areas, x, y), Some(*key), "{key:?} is covered at {w}x{h}");
                        assert!(r.y >= area.y && r.y + r.h <= h && r.x >= 0 && r.x + r.w <= w, "{key:?} off the keyboard at {w}x{h}");
                        if let Key::Char(ch) = key {
                            let mut psf = PSF2Font::new(FONT_32X64).unwrap();
                            if *ch != ' ' && psf.glyph_for_utf8(ch.encode_utf8(&mut [0; 4]).as_bytes()).is_none() {
                                missing.push(*ch);
                            }
                        }
                    }
                    assert_eq!(missing, "", "keys without a glyph");
                    assert!(c.buf[..(area.y * w) as usize].iter().all(|p| *p == 255), "drawn above the keyboard");
                }
            }
        }
        // the key under a finger is the only thing that changes
        let (mut up, mut down) = (Canvas::new(1236, 1648), Canvas::new(1236, 1648));
        let area = Rect { x: 0, y: 0, w: 1236, h: 660 };
        let mut kb = Keyboard::default();
        let areas = keyboard(&mut up, area, &kb, true);
        kb.pressed = Some(Key::Enter);
        keyboard(&mut down, area, &kb, true);
        let (enter, _) = areas.iter().find(|(_, k)| *k == Key::Enter).unwrap();
        assert_eq!(down.diff(&up).map(|d| enter.union(d)), Some(*enter));
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
