//! Tokens, access codes, rate limiting and HTML escaping.
//!
//! Tokens are JWTs (HS256) signed with the server's secret, which never leaves the server: it
//! comes from `DECKCRAFT_JWT_SECRET` or a random key the server creates in its data folder.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// What a token says. Roles are not trusted from here: the server looks the participant up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claims {
    /// Session id (`*` for the host dashboard login).
    pub sid: String,
    /// Participant id.
    pub pid: String,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn sign(secret: &[u8], claims: &Claims) -> String {
    let header = B64.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = B64.encode(serde_json::to_vec(claims).unwrap_or_default());
    let msg = format!("{header}.{payload}");
    let sig = B64.encode(mac(secret, msg.as_bytes()));
    format!("{msg}.{sig}")
}

fn mac(secret: &[u8], msg: &[u8]) -> Vec<u8> {
    match HmacSha256::new_from_slice(secret) {
        Ok(mut m) => {
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Err(_) => vec![],
    }
}

/// The claims of a valid, unexpired token signed with `secret`.
pub fn verify(secret: &[u8], token: &str) -> Option<Claims> {
    if token.len() > 4096 {
        return None;
    }
    let mut parts = token.split('.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: serde_json::Value = serde_json::from_slice(&B64.decode(h).ok()?).ok()?;
    if header.get("alg")?.as_str()? != "HS256" {
        return None;
    }
    let sig = B64.decode(s).ok()?;
    let mut m = HmacSha256::new_from_slice(secret).ok()?;
    m.update(format!("{h}.{p}").as_bytes());
    // Constant-time comparison.
    m.verify_slice(&sig).ok()?;
    let claims: Claims = serde_json::from_slice(&B64.decode(p).ok()?).ok()?;
    (claims.exp > now()).then_some(claims)
}

/// A keyed check value (the dashboard's CSRF token).
pub fn tag(secret: &[u8], what: &str) -> String {
    B64.encode(mac(secret, what.as_bytes()))
}

/// Constant-time string comparison.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    if getrandom::fill(&mut b).is_err() {
        // No OS randomness: refuse to make weak codes and secrets.
        eprintln!("deckcraft-server: the operating system gave no random numbers; stopping");
        std::process::exit(2);
    }
    b
}

/// A random id for sessions and participants (128 bits, URL safe).
pub fn random_id() -> String {
    B64.encode(random_bytes::<16>())
}

/// Characters of access codes: no 0/O, 1/I/L, U.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ23456789";

/// A new access code: 10 characters from 30 (49 bits), shown as `ABCDE-FGHJK`. With the rate
/// limit below, guessing one is out of reach.
pub fn new_code() -> String {
    let raw = random_bytes::<10>();
    let chars: String = raw.iter().map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char).collect();
    format!("{}-{}", &chars[..5], &chars[5..])
}

/// A typed code made comparable: upper case, no spaces or dashes, look-alikes mapped.
pub fn normalize_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .take(32)
        .collect()
}

/// Failed attempts per address: at most `MAX_FAILURES` in `WINDOW`.
#[derive(Default)]
pub struct Limiter {
    failures: HashMap<IpAddr, (Instant, u32)>,
}

pub const MAX_FAILURES: u32 = 10;
pub const WINDOW: Duration = Duration::from_secs(600);

impl Limiter {
    /// Whether `ip` may try again.
    pub fn allowed(&mut self, ip: IpAddr) -> bool {
        self.prune();
        self.failures.get(&ip).is_none_or(|(_, n)| *n < MAX_FAILURES)
    }
    pub fn failed(&mut self, ip: IpAddr) {
        let e = self.failures.entry(ip).or_insert((Instant::now(), 0));
        e.1 = e.1.saturating_add(1);
    }
    fn prune(&mut self) {
        self.failures.retain(|_, (t, _)| t.elapsed() < WINDOW);
        // A flood of addresses can't grow this without bound.
        if self.failures.len() > 100_000 {
            self.failures.clear();
        }
    }
}

pub fn html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// A display name: trimmed, printable, at most 60 characters.
pub fn clean_name(s: &str) -> Option<String> {
    let n: String = s.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    (!n.is_empty()).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_verify_and_reject_tampering() {
        let secret = [7u8; 32];
        let c = Claims { sid: "s".into(), pid: "p".into(), exp: now() + 60 };
        let t = sign(&secret, &c);
        assert_eq!(verify(&secret, &t), Some(c.clone()));
        assert_eq!(verify(&[8u8; 32], &t), None, "other secret");
        let forged = t.replacen('.', ".e30.", 1);
        assert_eq!(verify(&secret, &forged), None);
        let expired = sign(&secret, &Claims { exp: now() - 1, ..c });
        assert_eq!(verify(&secret, &expired), None);
        // alg=none is refused.
        let none = format!("{}.{}.", B64.encode(br#"{"alg":"none"}"#), t.split('.').nth(1).unwrap());
        assert_eq!(verify(&secret, &none), None);
    }

    #[test]
    fn codes_are_well_formed_and_forgiving() {
        let c = new_code();
        assert_eq!(c.len(), 11);
        assert_ne!(new_code(), c);
        assert_eq!(normalize_code(&c.to_lowercase().replace('-', " ")), normalize_code(&c));
        assert_eq!(normalize_code("o0-Il1"), "00111");
    }

    #[test]
    fn limiter_blocks_after_failures() {
        let mut l = Limiter::default();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(l.allowed(ip));
            l.failed(ip);
        }
        assert!(!l.allowed(ip));
        assert!(l.allowed("10.0.0.2".parse().unwrap()));
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(html("<a href=\"x\">'&"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;");
    }
}
