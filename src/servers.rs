//! The server list, stored as servers.ini: one `[name]` section per server. Pure logic.
//! Passwords are never in this file (see cred.rs).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub domain: String,
}

impl Server {
    /// Identifies the saved password and the encryption key binding.
    pub fn account(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

pub fn parse(text: &str) -> Result<Vec<Server>, String> {
    let mut servers: Vec<Server> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        let bad = |what: &str| format!("servers.ini line {}: {what}", n + 1);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            servers.push(Server { name: name.trim().to_string(), host: String::new(), port: 3389, user: String::new(), domain: String::new() });
            continue;
        }
        let (key, value) = line.split_once('=').ok_or(bad("expected key = value"))?;
        let (key, value) = (key.trim(), value.trim().to_string());
        let server = servers.last_mut().ok_or(bad("expected [server name] first"))?;
        match key {
            "host" => server.host = value,
            "port" => server.port = value.parse().map_err(|_| bad("bad port"))?,
            "user" => server.user = value,
            "domain" => server.domain = value,
            _ => return Err(bad("unknown key")),
        }
    }
    Ok(servers)
}

pub fn serialize(servers: &[Server]) -> String {
    servers.iter().map(|s| format!("[{}]\nhost = {}\nport = {}\nuser = {}\ndomain = {}\n\n", s.name, s.host, s.port, s.user, s.domain)).collect()
}

/// Checks the editor's fields (name, host, port, user, domain) and builds the server.
pub fn from_fields(fields: &[String]) -> Result<Server, String> {
    let [name, host, port, user, domain] = [0, 1, 2, 3, 4].map(|i| fields[i].trim().to_string());
    if host.is_empty() || user.is_empty() {
        return Err("Host and user are required".into());
    }
    if name.contains(['[', ']']) {
        return Err("Name cannot contain [ or ]".into());
    }
    let port = if port.is_empty() { 3389 } else { port.parse().map_err(|_| "Port must be a number (1-65535)")? };
    Ok(Server { name: if name.is_empty() { host.clone() } else { name }, host, port, user, domain })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(v: [&str; 5]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn roundtrip() {
        let list = vec![
            Server { name: "Office PC".into(), host: "10.0.0.5".into(), port: 3389, user: "me".into(), domain: "CORP".into() },
            Server { name: "nas".into(), host: "nas.local".into(), port: 3390, user: "admin".into(), domain: String::new() },
        ];
        assert_eq!(parse(&serialize(&list)).unwrap(), list);
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(list[1].account(), "admin@nas.local:3390");
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse("host = a").is_err());
        assert!(parse("[a]\nport = x").is_err());
        assert!(parse("[a]\npassword = x").is_err());
    }

    #[test]
    fn editor_fields() {
        let s = from_fields(&fields(["", " pc ", "", "me", ""])).unwrap();
        assert_eq!((s.name.as_str(), s.host.as_str(), s.port), ("pc", "pc", 3389));
        assert!(from_fields(&fields(["a", "", "", "me", ""])).is_err());
        assert!(from_fields(&fields(["a", "pc", "99999", "me", ""])).is_err());
        assert!(from_fields(&fields(["a]", "pc", "", "me", ""])).is_err());
    }
}
