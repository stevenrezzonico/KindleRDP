//! config.ini parsing: the display settings. Servers are in servers.ini (servers.rs).

#[derive(Debug, PartialEq)]
pub struct Config {
    /// Windows display scaling in percent (100-500).
    pub scale: u32,
    /// Above 1 darkens mid-grays so pale Windows UI stays visible on e-ink.
    pub gamma: f32,
    pub fbink: Option<String>,
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut c = Config { scale: 200, gamma: 1.0, fbink: None };
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
        assert_eq!(c, Config { scale: 150, gamma: 1.0, fbink: None });
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
