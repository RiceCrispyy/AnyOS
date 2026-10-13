//! # anyos-net
//!
//! Network policy that AnyOS applies *inside the runtime* for each guest
//! program, so the host OS (its hosts file, DNS settings, other apps) is never
//! touched.
//!
//! The first feature is **host redirects**: when a guest looks up or connects
//! to `oldgame.example` (or any subdomain such as `game.oldgame.example`),
//! AnyOS hands it the redirect target instead. The guest's networking APIs
//! (Winsock `getaddrinfo`/`gethostbyname`/`connect`, WinINet/WinHTTP
//! `InternetConnect`/`WinHttpConnect`, and later Linux/Darwin socket syscalls)
//! all resolve names through [`NetPolicy::resolve`], so one rule covers every
//! path a program might use.
//!
//! Policies live in a per-app profile file, `<program>.anyos.toml`, next to
//! the program:
//!
//! ```toml
//! [network.redirects]
//! "oldgame.example" = "203.0.113.10"
//! "updates.example.com" = "127.0.0.1:8080"
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("{0:?} is not a valid host name or IP address")]
    BadHost(String),
    #[error("{0:?} is not a valid redirect target (expected host, IP, or host:port)")]
    BadTarget(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
}

/// Where redirected traffic goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// IP address or host name.
    pub host: String,
    /// Replacement port; `None` keeps the port the program asked for.
    pub port: Option<u16>,
}

impl Target {
    pub fn parse(s: &str) -> Result<Self, NetError> {
        let s = s.trim();
        let bad = || NetError::BadTarget(s.to_string());
        // [v6]:port, v6, host:port, host
        if let Some(rest) = s.strip_prefix('[') {
            let (ip, after) = rest.split_once(']').ok_or_else(bad)?;
            ip.parse::<std::net::Ipv6Addr>().map_err(|_| bad())?;
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p.parse().map_err(|_| bad())?),
                None if after.is_empty() => None,
                None => return Err(bad()),
            };
            return Ok(Target {
                host: ip.to_string(),
                port,
            });
        }
        if s.parse::<IpAddr>().is_ok() {
            return Ok(Target {
                host: s.to_string(),
                port: None,
            });
        }
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h, Some(p.parse().map_err(|_| bad())?)),
            None => (s, None),
        };
        Ok(Target {
            host: normalize_host(host).map_err(|_| bad())?,
            port,
        })
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v6 = self.host.contains(':');
        match (self.port, v6) {
            (Some(p), true) => write!(f, "[{}]:{p}", self.host),
            (Some(p), false) => write!(f, "{}:{p}", self.host),
            (None, _) => f.write_str(&self.host),
        }
    }
}

/// Lower-case, strip one trailing dot, and check the characters.
fn normalize_host(host: &str) -> Result<String, NetError> {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    if ok || h.parse::<IpAddr>().is_ok() {
        Ok(h)
    } else {
        Err(NetError::BadHost(host.to_string()))
    }
}

/// The per-program network policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetPolicy {
    /// Normalized source host (domain or IP) → target.
    redirects: BTreeMap<String, Target>,
}

/// Result of applying the policy to one host:port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub host: String,
    pub port: u16,
    /// The rule that matched, if any.
    pub rule: Option<String>,
}

impl NetPolicy {
    /// Redirect `from` (a domain, which also covers its subdomains, or an IP
    /// address) to `to`. Replaces an existing rule for the same source.
    pub fn add_redirect(&mut self, from: &str, to: &str) -> Result<(), NetError> {
        let from = normalize_host(from.trim_start_matches("*."))?;
        let to = Target::parse(to)?;
        self.redirects.insert(from, to);
        Ok(())
    }

    pub fn remove_redirect(&mut self, from: &str) -> bool {
        normalize_host(from.trim_start_matches("*."))
            .map(|f| self.redirects.remove(&f).is_some())
            .unwrap_or(false)
    }

    pub fn redirects(&self) -> impl Iterator<Item = (&str, &Target)> {
        self.redirects.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn is_empty(&self) -> bool {
        self.redirects.is_empty()
    }

    /// The most specific rule covering `host`: an exact match, else the longest
    /// parent domain (`game.oldgame.example` → `oldgame.example`). IP
    /// addresses only match exactly.
    fn rule_for(&self, host: &str) -> Option<(&str, &Target)> {
        let host = normalize_host(host).ok()?;
        if let Some((k, v)) = self.redirects.get_key_value(&host) {
            return Some((k.as_str(), v));
        }
        if host.parse::<IpAddr>().is_ok() {
            return None;
        }
        let mut rest = host.as_str();
        while let Some((_, parent)) = rest.split_once('.') {
            if let Some((k, v)) = self.redirects.get_key_value(parent) {
                return Some((k.as_str(), v));
            }
            rest = parent;
        }
        None
    }

    /// Apply redirects to a requested host and port, without any DNS lookup.
    pub fn rewrite(&self, host: &str, port: u16) -> Rewrite {
        match self.rule_for(host) {
            Some((rule, t)) => Rewrite {
                host: t.host.clone(),
                port: t.port.unwrap_or(port),
                rule: Some(rule.to_string()),
            },
            None => Rewrite {
                host: host.to_string(),
                port,
                rule: None,
            },
        }
    }

    /// What a guest's name lookup or connect should use: the rewritten host,
    /// resolved through the host's normal DNS when it is a name.
    pub fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        let r = self.rewrite(host, port);
        if let Ok(ip) = r.host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, r.port)]);
        }
        Ok((r.host.as_str(), r.port).to_socket_addrs()?.collect())
    }
}

/// On-disk per-app profile (`<program>.anyos.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppProfile {
    #[serde(default, skip_serializing_if = "NetworkSection::is_empty")]
    pub network: NetworkSection,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSection {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub redirects: BTreeMap<String, String>,
}

impl NetworkSection {
    fn is_empty(&self) -> bool {
        self.redirects.is_empty()
    }
}

/// Profile path for a program: `Game.exe` → `Game.anyos.toml` beside it.
pub fn profile_path(program: &Path) -> PathBuf {
    let stem = program
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    program.with_file_name(format!("{stem}.anyos.toml"))
}

impl AppProfile {
    /// Load the profile for `program`; a missing file is an empty profile.
    pub fn load_for(program: &Path) -> Result<Self, NetError> {
        let path = profile_path(program);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| NetError::Parse {
                path,
                message: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AppProfile::default()),
            Err(source) => Err(NetError::Io { path, source }),
        }
    }

    pub fn save_for(&self, program: &Path) -> Result<PathBuf, NetError> {
        let path = profile_path(program);
        let name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let body = toml::to_string_pretty(self).map_err(|e| NetError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        let text = format!("# AnyOS settings for {name}\n\n{body}");
        std::fs::write(&path, text).map_err(|source| NetError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// The validated network policy described by this profile.
    pub fn net_policy(&self) -> Result<NetPolicy, NetError> {
        let mut p = NetPolicy::default();
        for (from, to) in &self.network.redirects {
            p.add_redirect(from, to)?;
        }
        Ok(p)
    }

    /// Store `policy`'s redirects back into the profile.
    pub fn set_net_policy(&mut self, policy: &NetPolicy) {
        self.network.redirects = policy
            .redirects()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NetPolicy {
        let mut p = NetPolicy::default();
        p.add_redirect("oldgame.example", "203.0.113.10").unwrap();
        p
    }

    #[test]
    fn domain_rule_covers_subdomains_and_keeps_port() {
        let p = sample();
        let r = p.rewrite("game.oldgame.example", 80);
        assert_eq!((r.host.as_str(), r.port), ("203.0.113.10", 80));
        assert_eq!(r.rule.as_deref(), Some("oldgame.example"));
        assert_eq!(p.rewrite("OLDGAME.EXAMPLE.", 443).host, "203.0.113.10");
        assert_eq!(p.rewrite("www.oldgame.example", 443).port, 443);
        // Lookalikes are not matched.
        assert_eq!(p.rewrite("notoldgame.example", 80).rule, None);
        assert_eq!(p.rewrite("oldgame.example.evil.net", 80).rule, None);
    }

    #[test]
    fn most_specific_rule_wins_and_ports_can_change() {
        let mut p = sample();
        p.add_redirect("*.chat.oldgame.example", "chat.example.net:7000")
            .unwrap();
        let r = p.rewrite("eu.chat.oldgame.example", 6667);
        assert_eq!((r.host.as_str(), r.port), ("chat.example.net", 7000));
        assert_eq!(p.rewrite("game.oldgame.example", 80).host, "203.0.113.10");
    }

    #[test]
    fn ip_sources_match_exactly() {
        let mut p = NetPolicy::default();
        p.add_redirect("203.0.113.5", "[2001:db8::1]:9000").unwrap();
        let r = p.rewrite("203.0.113.5", 80);
        assert_eq!((r.host.as_str(), r.port), ("2001:db8::1", 9000));
        assert_eq!(p.rewrite("203.0.113.50", 80).rule, None);
        assert_eq!(
            p.resolve("203.0.113.5", 80).unwrap(),
            vec!["[2001:db8::1]:9000".parse().unwrap()]
        );
    }

    #[test]
    fn resolve_uses_ip_targets_without_dns() {
        let addrs = sample().resolve("game.oldgame.example", 80).unwrap();
        assert_eq!(addrs, vec!["203.0.113.10:80".parse().unwrap()]);
    }

    #[test]
    fn rejects_bad_input() {
        let mut p = NetPolicy::default();
        assert!(p.add_redirect("bad host", "1.2.3.4").is_err());
        assert!(p.add_redirect("a..b", "1.2.3.4").is_err());
        assert!(p.add_redirect("ok.com", "1.2.3.4:notaport").is_err());
        assert!(p.add_redirect("ok.com", "[::1").is_err());
        assert!(p.is_empty());
    }

    #[test]
    fn profile_round_trips_beside_the_program() {
        let dir = std::env::temp_dir().join(format!("anyos-net-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("Game.exe");
        assert_eq!(profile_path(&exe), dir.join("Game.anyos.toml"));
        assert!(AppProfile::load_for(&exe)
            .unwrap()
            .net_policy()
            .unwrap()
            .is_empty());

        let mut profile = AppProfile::default();
        profile.set_net_policy(&sample());
        let path = profile.save_for(&exe).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[network.redirects]"), "{text}");
        assert!(
            text.contains("\"oldgame.example\" = \"203.0.113.10\""),
            "{text}"
        );
        assert_eq!(
            AppProfile::load_for(&exe).unwrap().net_policy().unwrap(),
            sample()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
