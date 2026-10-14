//! Self-update for the AnyOS tools (`anyos` and the AnyOS app).
//!
//! 1. [`check`] asks GitHub for the latest release of RiceCrispyy/AnyOS and
//!    compares its tag with the version this build was released as.
//! 2. [`install`] downloads this platform's archive and the release's
//!    `SHA256SUMS`, refuses the archive unless the hashes match, unpacks it
//!    beside the installed copy and swaps the new files in (the whole
//!    `AnyOS.app` on macOS; see [`Layout`]).
//! 3. The caller restarts (the app) or simply finishes (the CLI).
//!
//! Only builds made by the release workflow know their version (it sets
//! `ANYOS_VERSION` from the tag), so automatic checks are off in local builds;
//! [`is_release_build`] tells which is which. `ANYOS_NO_UPDATE_CHECK=1` turns
//! automatic checks off, and `ANYOS_UPDATE_URL` points the check at another
//! "latest release" URL (for testing).

mod install;
mod release;
mod version;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub use install::{cleanup_leftovers, Layout};
pub use release::{expected_sha256, parse_latest, release_target, Update};
pub use version::Version;

/// GitHub's "latest release" endpoint for AnyOS. Public repo, so no token.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/RiceCrispyy/AnyOS/releases/latest";

/// How often automatic checks ask GitHub (manual checks always do).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not reach the update server: {0}")]
    Network(String),
    #[error("unexpected release data: {0}")]
    BadRelease(String),
    #[error("release {tag} has no {name}")]
    NoAsset { tag: String, name: String },
    #[error("AnyOS does not publish builds for {0}")]
    UnsupportedPlatform(String),
    #[error("download of {name} is corrupt: SHA-256 {actual}, expected {expected}")]
    ChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("SHA256SUMS does not list {0}")]
    NoChecksum(String),
    #[error("bad release archive: {0}")]
    BadArchive(String),
    #[error("cannot tell where AnyOS is installed from {0}")]
    Layout(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl Error {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Error {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// True when the install folder is not writable by this user (e.g. a
    /// system folder), so the caller can suggest re-running with more rights.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied)
    }
}

/// The version this build was released as (`0.1.13`), or the crate version in
/// a local build.
pub fn current_version() -> &'static str {
    match option_env!("ANYOS_VERSION") {
        Some(v) if !v.is_empty() => {
            // The workflow passes the tag; drop its `v`.
            match v.as_bytes()[0] {
                b'v' | b'V' => v.split_at(1).1,
                _ => v,
            }
        }
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// True for builds made by the release workflow. Local builds do not know
/// which release they correspond to, so they never check on their own.
pub fn is_release_build() -> bool {
    option_env!("ANYOS_VERSION").is_some_and(|v| Version::parse(v).is_some())
}

/// Whether automatic checks are allowed: a release build, and not turned off
/// with `ANYOS_NO_UPDATE_CHECK`.
pub fn auto_check_enabled() -> bool {
    is_release_build()
        && std::env::var_os("ANYOS_NO_UPDATE_CHECK").is_none_or(|v| v.is_empty() || v == "0")
}

fn latest_url() -> String {
    std::env::var("ANYOS_UPDATE_URL").unwrap_or_else(|_| LATEST_RELEASE_URL.to_string())
}

fn current() -> Version {
    Version::parse(current_version()).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
        pre: None,
    })
}

/// Asks GitHub whether a newer release exists for this platform.
/// `timeout` bounds the whole request.
pub fn check(timeout: Duration) -> Result<Option<Update>, Error> {
    let target = release_target().ok_or_else(|| {
        Error::UnsupportedPlatform(format!(
            "{}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let agent = release::agent(timeout);
    let json = release::get_text(&agent, &latest_url())?;
    parse_latest(&json, &current(), target)
}

/// What [`install`] did.
#[derive(Clone, Debug)]
pub struct Installed {
    pub version: Version,
    pub layout: Layout,
    /// Names replaced in `layout.dir`.
    pub replaced: Vec<String>,
}

/// Downloads `update`, checks it against the release's `SHA256SUMS` and swaps
/// it in for the running installation. `progress(done, total)` reports the
/// archive download.
pub fn install(
    update: &Update,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Installed, Error> {
    install_into(update, &Layout::current()?, &mut progress)
}

/// [`install`] for an explicit layout.
pub fn install_into(
    update: &Update,
    layout: &Layout,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Installed, Error> {
    let agent = release::agent(Duration::from_secs(15 * 60));
    let sums = release::get_text(&agent, &update.sums_url)?;
    let expected = expected_sha256(&sums, &update.archive_name)
        .ok_or_else(|| Error::NoChecksum(update.archive_name.clone()))?;

    // Stage inside the install folder: same file system, and failing here
    // (read-only folder) means nothing has been touched yet.
    let staging = install::Staging::new(&layout.dir)?;
    let archive = staging.path().join(&update.archive_name);
    let actual = release::download(&agent, &update.archive_url, &archive, progress)?;
    if actual != expected {
        return Err(Error::ChecksumMismatch {
            name: update.archive_name.clone(),
            expected,
            actual,
        });
    }
    let unpacked = staging.path().join("unpacked");
    std::fs::create_dir(&unpacked).map_err(|e| Error::io(&unpacked, e))?;
    let root = install::unpack(&archive, &unpacked)?;
    let replaced = install::swap_in(layout, &root)?;
    Ok(Installed {
        version: update.version.clone(),
        layout: layout.clone(),
        replaced,
    })
}

/// Remembered result of the last automatic check, so the CLI asks GitHub at
/// most once per [`CHECK_INTERVAL`]. Stored as two lines: the Unix time of the
/// check and the newest version seen (empty when none).
pub struct CheckCache {
    path: PathBuf,
}

impl CheckCache {
    pub fn new(path: PathBuf) -> CheckCache {
        CheckCache { path }
    }

    /// The newest release seen by the last check, if newer than this build.
    pub fn newer_version(&self) -> Option<Version> {
        let (_, v) = self.read()?;
        v.filter(|v| *v > current())
    }

    /// True when the last check is older than [`CHECK_INTERVAL`] (or missing).
    pub fn is_due(&self) -> bool {
        match self.read() {
            Some((at, _)) => now().saturating_sub(at) >= CHECK_INTERVAL.as_secs(),
            None => true,
        }
    }

    /// Records a check that found `latest` (or nothing newer).
    pub fn record(&self, latest: Option<&Version>) {
        let body = format!(
            "{}\n{}\n",
            now(),
            latest.map(Version::to_string).unwrap_or_default()
        );
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Write then rename, so a half-written file is never read.
        let tmp = self
            .path
            .with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn read(&self) -> Option<(u64, Option<Version>)> {
        let s = std::fs::read_to_string(&self.path).ok()?;
        let mut lines = s.lines();
        let at = lines.next()?.trim().parse().ok()?;
        Some((at, lines.next().and_then(Version::parse)))
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_parses() {
        assert!(Version::parse(current_version()).is_some());
    }

    #[test]
    fn check_cache_round_trip() {
        let p = std::env::temp_dir().join(format!("anyos-update-cache-{}", std::process::id()));
        let c = CheckCache::new(p.clone());
        let _ = std::fs::remove_file(&p);
        assert!(c.is_due());
        assert_eq!(c.newer_version(), None);

        c.record(Some(&Version::parse("999.0.0").unwrap()));
        assert!(!c.is_due());
        assert_eq!(c.newer_version().unwrap().to_string(), "999.0.0");

        c.record(Some(&Version::parse("0.0.1").unwrap()));
        assert_eq!(c.newer_version(), None);
        c.record(None);
        assert_eq!(c.newer_version(), None);
        let _ = std::fs::remove_file(&p);
    }
}
