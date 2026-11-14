//! Saved password encryption and server certificate pinning. Pure logic; file I/O is in device.rs.
//!
//! The key is derived from the device serial, so the file is useless when copied off the
//! Kindle. Anyone with root on the Kindle can still recover the password.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn cipher(serial: &[u8], account: &str) -> ChaCha20Poly1305 {
    let key = Sha256::new().chain_update(b"kindlerdp-v1\0").chain_update(serial).chain_update([0]).chain_update(account).finalize();
    ChaCha20Poly1305::new(&key)
}

/// `account` (user@host:port) is bound into the key: a blob saved for one host never
/// decrypts for another. `nonce` must be fresh random bytes on every call.
pub fn seal(serial: &[u8], account: &str, nonce: [u8; 12], password: &str) -> Vec<u8> {
    let mut out = nonce.to_vec();
    out.extend(cipher(serial, account).encrypt(Nonce::from_slice(&nonce), password.as_bytes()).expect("encrypt"));
    out
}

pub fn open(serial: &[u8], account: &str, blob: &[u8]) -> Option<String> {
    let (nonce, ct) = blob.split_at_checked(12)?;
    let plain = cipher(serial, account).decrypt(Nonce::from_slice(nonce), ct).ok()?;
    String::from_utf8(plain).ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pin {
    /// First connection to this host: caller stores the fingerprint.
    New,
    Match,
    Mismatch,
}

/// `known` is the pin file: one "host:port fingerprint" pair per line.
pub fn check_pin(known: &str, host: &str, fingerprint: &str) -> Pin {
    match known.lines().filter_map(|l| l.split_once(' ')).find(|(h, _)| *h == host) {
        None => Pin::New,
        Some((_, f)) if f == fingerprint => Pin::Match,
        Some(_) => Pin::Mismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_rejections() {
        let blob = seal(b"G000SERIAL", "me@pc:3389", [7; 12], "pässword");
        assert_eq!(open(b"G000SERIAL", "me@pc:3389", &blob).as_deref(), Some("pässword"));
        assert_eq!(open(b"OTHERDEVICE", "me@pc:3389", &blob), None);
        assert_eq!(open(b"G000SERIAL", "me@evil:3389", &blob), None);
        let mut bad = blob.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(open(b"G000SERIAL", "me@pc:3389", &bad), None);
        assert_eq!(open(b"G000SERIAL", "me@pc:3389", &blob[..5]), None);
        assert!(!blob.windows(4).any(|w| w == b"word"));
    }

    #[test]
    fn pins() {
        let known = "pc:3389 aa\nnas:3389 bb\n";
        assert_eq!(check_pin(known, "pc:3389", "aa"), Pin::Match);
        assert_eq!(check_pin(known, "pc:3389", "cc"), Pin::Mismatch);
        assert_eq!(check_pin(known, "new:3389", "aa"), Pin::New);
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }
}
