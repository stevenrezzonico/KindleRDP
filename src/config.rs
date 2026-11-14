//! config.ini parsing. The file lives on the USB-visible partition, so the password
//! is moved out of it after the first successful login (see cred.rs).

#[derive(Debug, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub domain: Option<String>,
    pub password: Option<String>,
    /// Windows display scaling in percent (100-500).
    pub scale: u32,
    /// Waveform name for screen changes (see device::waveform).
    pub fast_waveform: String,
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
        fast_waveform: "GL16".into(),
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
            "fast_waveform" => c.fast_waveform = value.to_uppercase(),
            "gamma" => c.gamma = value.parse().ok().filter(|g| (0.5..=3.0).contains(g)).ok_or(number("gamma (0.5-3.0)"))?,
            "fbink" => c.fbink = Some(value),
            _ => return Err(format!("line {}: unknown key '{key}'", n + 1)),
        }
    }
    if c.host.is_empty() || c.user.is_empty() {
        return Err("config needs host and user".into());
    }
    Ok(c)
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
        assert!(parse("user=a").is_err());
        assert!(parse("host=a\nuser=b\nport=x").is_err());
        assert!(parse("host=a\nuser=b\nscale=50").is_err());
        assert!(parse("host=a\nuser=b\ngamma=9").is_err());
        assert_eq!(parse("host=a\nuser=b\ngamma=1.5\nfast_waveform=du").unwrap().fast_waveform, "DU");
        assert!(parse("host=a\nuser=b\nhots=c").is_err());
        assert!(parse("host=a\nuser=b\njunk").is_err());
    }

    #[test]
    fn strips_only_password() {
        let text = "host = pc\r\npassword = s3cret=x\r\nuser = me";
        let out = strip_password(text);
        assert_eq!(out, "host = pc\r\npassword =\r\nuser = me");
        assert_eq!(parse(&out).unwrap().password, None);
    }
}
