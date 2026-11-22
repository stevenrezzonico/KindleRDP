//! config.ini parsing: the display settings. Servers are in servers.ini (servers.rs).

#[derive(Debug, PartialEq)]
pub struct Config {
    /// Windows display scaling in percent (100-500).
    pub scale: u32,
    /// Above 1 darkens mid-grays so pale Windows UI stays visible on e-ink.
    pub gamma: f32,
    pub fbink: Option<String>,
    /// Refresh the screen through the fbink tool even where the direct way works.
    pub refresh_fbink: bool,
    pub touch: TouchFix,
    pub touch_device: Option<String>,
}

/// Corrections for a touch screen whose axes do not match the screen's.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TouchFix {
    pub swap_xy: bool,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

impl TouchFix {
    /// A raw position, each axis given with its maximum, as a framebuffer pixel.
    pub fn place(self, x: (i32, i32), y: (i32, i32), fb_w: i32, fb_h: i32) -> (i32, i32) {
        let (x, y) = if self.swap_xy { (y, x) } else { (x, y) };
        let scaled = |(at, max): (i32, i32), size: i32, mirror: bool| {
            let pixel = (at as i64 * size as i64 / (max as i64 + 1)) as i32;
            if mirror { size - 1 - pixel } else { pixel }
        };
        (scaled(x, fb_w, self.mirror_x), scaled(y, fb_h, self.mirror_y))
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut c = Config { scale: 200, gamma: 1.0, fbink: None, refresh_fbink: false, touch: TouchFix::default(), touch_device: None };
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(format!("line {}: expected key = value", n + 1))?;
        let (key, value) = (key.trim(), value.trim().to_string());
        let number = |what: &str| format!("line {}: bad {what}", n + 1);
        match key {
            "scale" => c.scale = value.parse().ok().filter(|s| (100..=500).contains(s)).ok_or(number("scale (100-500)"))?,
            // Keys of the first version: ignored, so a config file from then still loads.
            "host" | "port" | "user" | "domain" | "password" | "fast_waveform" => {}
            "gamma" => c.gamma = value.parse().ok().filter(|g| (0.5..=3.0).contains(g)).ok_or(number("gamma (0.5-3.0)"))?,
            "fbink" => c.fbink = Some(value),
            "refresh" => {
                c.refresh_fbink = match value.as_str() {
                    "fbink" => true,
                    "direct" => false,
                    _ => return Err(number("refresh (direct or fbink)")),
                }
            }
            "touch" => {
                for word in value.split_whitespace() {
                    match word {
                        "swap_xy" => c.touch.swap_xy = true,
                        "mirror_x" => c.touch.mirror_x = true,
                        "mirror_y" => c.touch.mirror_y = true,
                        _ => return Err(number("touch (swap_xy, mirror_x, mirror_y)")),
                    }
                }
            }
            "touch_device" => c.touch_device = Some(value),
            _ => return Err(format!("line {}: unknown key '{key}'", n + 1)),
        }
    }
    Ok(c)
}

/// The built-in config.ini, written on first start: every setting commented at its default.
pub const DEFAULT: &str = include_str!("default_config.ini");

/// The config text with `key` set to `value`. Replaces the line that sets the key, else the
/// commented-out line that documents it, else appends. Every other line is kept as it is.
pub fn set(text: &str, key: &str, value: &str) -> String {
    let names_key = |line: &str| line.trim().trim_start_matches('#').split_once('=').is_some_and(|(k, _)| k.trim() == key);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let active = lines.iter().position(|l| !l.trim().starts_with('#') && names_key(l));
    let setting = format!("{key} = {value}");
    match active.or_else(|| lines.iter().position(|l| names_key(l))) {
        Some(i) => lines[i] = setting,
        None => lines.push(setting),
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_defaults() {
        // a file from the first version: its server keys are ignored
        let c = parse("# my pc\r\nhost = 192.168.1.10\r\nuser=steve\r\npassword = p#a=s s \r\n\r\nscale=150\n").unwrap();
        assert_eq!((c.scale, c.gamma, c.fbink, c.refresh_fbink, c.touch, c.touch_device), (150, 1.0, None, false, TouchFix::default(), None));
    }

    #[test]
    fn hardware_overrides() {
        let c = parse("refresh = fbink\ntouch = swap_xy mirror_y\ntouch_device = /dev/input/event2").unwrap();
        assert!(c.refresh_fbink && c.touch.swap_xy && c.touch.mirror_y && !c.touch.mirror_x);
        assert_eq!(c.touch_device.as_deref(), Some("/dev/input/event2"));
        assert!(parse("refresh = fast").is_err());
        assert!(parse("touch = upside_down").is_err());
        // a 600x800 panel whose touch axes run to 4095
        let plain = TouchFix::default();
        assert_eq!(plain.place((0, 4095), (4095, 4095), 600, 800), (0, 799));
        assert_eq!(c.touch.place((0, 4095), (4095, 4095), 600, 800), (599, 799));
        let mirrored = TouchFix { mirror_x: true, ..plain };
        assert_eq!(mirrored.place((0, 4095), (0, 4095), 600, 800), (599, 0));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("host=a\nuser=b\nscale=50").is_err());
        assert!(parse("host=a\nuser=b\ngamma=9").is_err());
        assert_eq!(parse("host=a\nuser=b\ngamma=1.5\nfast_waveform=du").unwrap().gamma, 1.5);
        assert!(parse("host=a\nuser=b\nhots=c").is_err());
        assert!(parse("host=a\nuser=b\njunk").is_err());
    }

    #[test]
    fn default_file_is_all_defaults_and_set_edits_one_line() {
        let defaults = parse("").unwrap();
        assert_eq!(parse(DEFAULT).unwrap(), defaults);
        // first change: the documented, commented line becomes the setting
        let once = set(DEFAULT, "scale", "250");
        assert_eq!(parse(&once).unwrap().scale, 250);
        assert_eq!(once.lines().count(), DEFAULT.lines().count());
        assert!(once.contains("# Windows display scaling"));
        // second change: same line again, nothing appended
        let twice = set(&once, "scale", "150");
        assert_eq!(parse(&twice).unwrap().scale, 150);
        assert_eq!(twice.matches("scale =").count(), 1);
        // a key the file never mentions is appended; other settings survive
        let added = set("scale = 150\nfbink = /x", "gamma", "1.4");
        let c = parse(&added).unwrap();
        assert_eq!((c.scale, c.gamma), (150, 1.4));
        assert_eq!(parse(&set(&set(DEFAULT, "scale", "300"), "gamma", "1.2")).unwrap().scale, 300);
    }
}
