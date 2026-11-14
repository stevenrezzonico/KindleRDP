//! config.ini parsing: display settings, plus the single server of the first version, which
//! is imported into servers.ini on first run. The file lives on the USB-visible partition, so
//! a password found here is moved to the encrypted store.

#[derive(Debug, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub domain: Option<String>,
    pub password: Option<String>,
    /// Windows display scaling in percent (100-500).
    pub scale: u32,
    /// Above 1 darkens mid-grays so pale Windows UI stays visible on e-ink.
    pub gamma: f32,
    pub fbink: Option<String>,
}

pub fn parse(text: &str) -> Result<Config, String> {
    let mut c = Config {
        host: String::new(),
        port: 3389,
        user: String::new(),
        domain: None,
        password: None,
        scale: 200,
        gamma: 1.0,
        fbink: None,
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(format!("line {}: expected key = value", n + 1))?;
        // The value is kept verbatim after trimming: passwords may contain '#' or '='.
        let (key, value) = (key.trim(), value.trim().to_string());
        let number = |what: &str| format!("line {}: bad {what}", n + 1);
        match key {
            "host" => c.host = value,
            "port" => c.port = value.parse().map_err(|_| number("port"))?,
            "user" => c.user = value,
            "domain" => c.domain = Some(value).filter(|v| !v.is_empty()),
            "password" => c.password = Some(value).filter(|v| !v.is_empty()),
            "scale" => c.scale = value.parse().ok().filter(|s| (100..=500).contains(s)).ok_or(number("scale (100-500)"))?,
            "fast_waveform" => {} // removed option: accepted so older config files still load
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

/// The config text with the password value blanked. Everything else is kept byte for byte.
pub fn strip_password(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| match line.split_once('=') {
            Some((key, _)) if key.trim() == "password" => {
                format!("{key}={}", if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" })
            }
            _ => line.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_defaults() {
        let c = parse("# my pc\r\nhost = 192.168.1.10\r\nuser=steve\r\npassword = p#a=s s \r\n\r\nscale=150\n").unwrap();
        assert_eq!(c.host, "192.168.1.10");
        assert_eq!(c.port, 3389);
        assert_eq!(c.password.as_deref(), Some("p#a=s s"));
        assert_eq!(c.scale, 150);
        assert_eq!(c.domain, None);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("host=a\nuser=b\nport=x").is_err());
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
        let added = set("host = pc\nuser = me", "gamma", "1.4");
        let c = parse(&added).unwrap();
        assert_eq!((c.host.as_str(), c.gamma), ("pc", 1.4));
        assert_eq!(parse(&set(&set(DEFAULT, "scale", "300"), "gamma", "1.2")).unwrap().scale, 300);
    }

    #[test]
    fn strips_only_password() {
        let text = "host = pc\r\npassword = s3cret=x\r\nuser = me";
        let out = strip_password(text);
        assert_eq!(out, "host = pc\r\npassword =\r\nuser = me");
        assert_eq!(parse(&out).unwrap().password, None);
    }
}
