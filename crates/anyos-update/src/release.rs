//! Talking to GitHub: the latest release, its assets, and downloading them.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Error, Version};

/// Largest archive we are willing to download (the real ones are 5-15 MB).
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
/// Largest API response or checksum file we read.
const MAX_SMALL: u64 = 4 * 1024 * 1024;

/// A newer release that can be installed on this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    /// The tag it was published under, e.g. `v0.1.13`.
    pub tag: String,
    /// The release page, for "what's new".
    pub page_url: String,
    /// `anyos-v0.1.13-<target>.tar.gz` (or `.zip` on Windows).
    pub archive_name: String,
    pub archive_url: String,
    /// The release's `SHA256SUMS` file.
    pub sums_url: String,
}

/// The release target this build downloads: the suffix of the release archive
/// names. `None` on hosts AnyOS does not publish builds for.
pub fn release_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        // One universal build covers Apple Silicon and Intel Macs.
        ("macos", _) => Some("universal2-apple-darwin"),
        _ => None,
    }
}

/// Archive extension used for `target`'s release.
pub(crate) fn archive_ext(target: &str) -> &'static str {
    if target.contains("windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

pub(crate) fn agent(timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(timeout.min(Duration::from_secs(10))))
        .user_agent(format!("anyos-updater/{}", crate::current_version()))
        // Redirects are followed by `get` below.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// GETs `url`, following redirects (release downloads redirect to a CDN).
/// Done by hand rather than by ureq, which reads the body of each redirect
/// response first and fails on some proxies' empty chunked bodies.
fn get(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let mut url = url.to_string();
    for _ in 0..8 {
        let resp = agent
            .get(&url)
            .header("Accept", accept)
            .call()
            .map_err(|e| Error::Network(format!("{url}: {e}")))?;
        let status = resp.status();
        if status.is_redirection() {
            let next = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::Network(format!("{url}: redirect without a location")))?;
            url = resolve(&url, next);
            continue;
        }
        if !status.is_success() {
            return Err(Error::Network(format!("{url}: HTTP {status}")));
        }
        return Ok(resp);
    }
    Err(Error::Network(format!("{url}: too many redirects")))
}

/// Resolves a `Location` header against the URL it came from.
fn resolve(base: &str, loc: &str) -> String {
    if loc.contains("://") {
        return loc.to_string();
    }
    let scheme_end = base.find("://").map_or(0, |i| i + 3);
    let host_end = base[scheme_end..]
        .find('/')
        .map_or(base.len(), |i| scheme_end + i);
    if loc.starts_with('/') {
        format!("{}{loc}", &base[..host_end])
    } else {
        let dir_end = base
            .rfind('/')
            .filter(|&i| i >= host_end)
            .unwrap_or(host_end);
        format!("{}/{loc}", &base[..dir_end])
    }
}

/// Reads GitHub's "latest release" JSON and picks the asset for `target`.
/// Returns `Ok(None)` when that release is not newer than `current`.
pub fn parse_latest(json: &str, current: &Version, target: &str) -> Result<Option<Update>, Error> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| Error::BadRelease(format!("not JSON: {e}")))?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| Error::BadRelease("no tag_name".into()))?
        .to_string();
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return Ok(None);
    }
    let version = Version::parse(&tag)
        .ok_or_else(|| Error::BadRelease(format!("tag {tag:?} is not a version")))?;
    if version <= *current {
        return Ok(None);
    }
    let archive_name = format!("anyos-{tag}-{target}.{}", archive_ext(target));
    let assets = v["assets"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let url_of = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let archive_url = url_of(&archive_name).ok_or_else(|| Error::NoAsset {
        tag: tag.clone(),
        name: archive_name.clone(),
    })?;
    let sums_url = url_of("SHA256SUMS").ok_or_else(|| Error::NoAsset {
        tag: tag.clone(),
        name: "SHA256SUMS".into(),
    })?;
    Ok(Some(Update {
        version,
        page_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        tag,
        archive_name,
        archive_url,
        sums_url,
    }))
}

/// Fetches `url` as text (the API response or `SHA256SUMS`).
pub(crate) fn get_text(agent: &ureq::Agent, url: &str) -> Result<String, Error> {
    let mut resp = get(agent, url, "application/vnd.github+json")?;
    resp.body_mut()
        .with_config()
        .limit(MAX_SMALL)
        .read_to_string()
        .map_err(|e| Error::Network(format!("{url}: {e}")))
}

/// Finds `name`'s hash in a `sha256sum`-style file (`<hex>  <name>` per line).
pub fn expected_sha256(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Downloads `url` into `dest`, returning the SHA-256 of what was written.
/// `progress(done, total)` is called as bytes arrive.
pub(crate) fn download(
    agent: &ureq::Agent,
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, Error> {
    let mut resp = get(agent, url, "application/octet-stream")?;
    let total = resp.body().content_length();
    if total.is_some_and(|t| t > MAX_ARCHIVE) {
        return Err(Error::Network(format!("{url}: download is too large")));
    }
    let mut reader = resp.body_mut().with_config().limit(MAX_ARCHIVE).reader();
    let mut out = File::create(dest).map_err(|e| Error::io(dest, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    progress(0, total);
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Network(format!("{url}: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| Error::io(dest, e))?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all().map_err(|e| Error::io(dest, e))?;
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "x86_64-unknown-linux-gnu";

    fn release_json(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://example.invalid/r/{tag}","draft":false,"prerelease":false,
            "assets":[
              {{"name":"anyos-{tag}-x86_64-unknown-linux-gnu.tar.gz","browser_download_url":"https://example.invalid/linux.tgz"}},
              {{"name":"anyos-{tag}-x86_64-pc-windows-msvc.zip","browser_download_url":"https://example.invalid/win.zip"}},
              {{"name":"SHA256SUMS","browser_download_url":"https://example.invalid/SHA256SUMS"}}]}}"#
        )
    }

    #[test]
    fn picks_this_platforms_archive_from_a_newer_release() {
        let cur = Version::parse("0.1.12").unwrap();
        let u = parse_latest(&release_json("v0.1.13"), &cur, TARGET)
            .unwrap()
            .unwrap();
        assert_eq!(u.tag, "v0.1.13");
        assert_eq!(
            u.archive_name,
            "anyos-v0.1.13-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(u.archive_url, "https://example.invalid/linux.tgz");
        assert_eq!(u.sums_url, "https://example.invalid/SHA256SUMS");

        let win = parse_latest(&release_json("v0.1.13"), &cur, "x86_64-pc-windows-msvc")
            .unwrap()
            .unwrap();
        assert_eq!(win.archive_url, "https://example.invalid/win.zip");
    }

    #[test]
    fn same_or_older_release_is_not_an_update() {
        let cur = Version::parse("0.1.12").unwrap();
        assert_eq!(
            parse_latest(&release_json("v0.1.12"), &cur, TARGET).unwrap(),
            None
        );
        assert_eq!(
            parse_latest(&release_json("v0.1.9"), &cur, TARGET).unwrap(),
            None
        );
    }

    #[test]
    fn missing_asset_is_an_error() {
        let cur = Version::parse("0.1.0").unwrap();
        let e = parse_latest(&release_json("v0.2.0"), &cur, "universal2-apple-darwin").unwrap_err();
        assert!(matches!(e, Error::NoAsset { .. }), "{e}");
    }

    #[test]
    fn resolves_redirect_locations() {
        let b = "https://github.com/o/r/releases/download/v1/x.zip";
        assert_eq!(
            resolve(b, "https://cdn.example/a?b=1"),
            "https://cdn.example/a?b=1"
        );
        assert_eq!(resolve(b, "/other/y"), "https://github.com/other/y");
        assert_eq!(
            resolve(b, "y.zip"),
            "https://github.com/o/r/releases/download/v1/y.zip"
        );
        assert_eq!(resolve("http://h:1", "p"), "http://h:1/p");
    }

    #[test]
    fn reads_sha256sums() {
        let h = "a".repeat(64);
        let sums = format!(
            "{h}  anyos-v1-x.tar.gz\n{}  *anyos-v1-y.zip\n",
            "B".repeat(64)
        );
        assert_eq!(expected_sha256(&sums, "anyos-v1-x.tar.gz"), Some(h));
        assert_eq!(
            expected_sha256(&sums, "anyos-v1-y.zip"),
            Some("b".repeat(64))
        );
        assert_eq!(expected_sha256(&sums, "anyos-v1-z.zip"), None);
        assert_eq!(
            expected_sha256("short  anyos-v1-x.tar.gz", "anyos-v1-x.tar.gz"),
            None
        );
    }
}
