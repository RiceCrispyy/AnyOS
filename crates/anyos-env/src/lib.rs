//! # anyos-env
//!
//! An AnyOS **environment** is where installed apps live: a private `C:` drive,
//! a registry, and an index of installed apps. Nothing an app installs touches
//! the host's own program folders or settings.
//!
//! ```text
//! <data dir>/envs/<name>/
//!   env.toml                  name and Windows user name
//!   apps.toml                 installed apps (see `apps`)
//!   windows/registry.toml     registry keys and values (see `registry`)
//!   windows/drive_c/          Program Files, ProgramData, Users/<user>, Windows
//! ```
//!
//! The data directory is `$ANYOS_HOME` when set, otherwise
//! `~/Library/Application Support/AnyOS` on macOS, `%LOCALAPPDATA%\AnyOS` on
//! Windows and `$XDG_DATA_HOME/anyos` (or `~/.local/share/anyos`) elsewhere.

pub mod apps;
pub mod registry;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use apps::{AppIndex, AppRecord, Shortcut};
pub use registry::{RegValue, Registry};

/// Environment variable that overrides the AnyOS data directory.
pub const DATA_DIR_VAR: &str = "ANYOS_HOME";
/// The environment used when none is named.
pub const DEFAULT_ENV: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("cannot find a home directory; set {DATA_DIR_VAR}")]
    NoDataDir,
    #[error("invalid environment name {0:?} (use letters, digits, '-', '_' and '.')")]
    BadName(String),
    #[error("path {0:?} cannot be placed in the environment")]
    BadPath(String),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> EnvError + '_ {
    move |source| EnvError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The AnyOS data directory.
pub fn data_dir() -> Result<PathBuf, EnvError> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(d) = var(DATA_DIR_VAR) {
        return Ok(d);
    }
    if cfg!(target_os = "macos") {
        return var("HOME")
            .map(|h| h.join("Library/Application Support/AnyOS"))
            .ok_or(EnvError::NoDataDir);
    }
    if cfg!(windows) {
        return var("LOCALAPPDATA")
            .map(|d| d.join("AnyOS"))
            .ok_or(EnvError::NoDataDir);
    }
    if let Some(x) = var("XDG_DATA_HOME") {
        return Ok(x.join("anyos"));
    }
    var("HOME")
        .map(|h| h.join(".local/share/anyos"))
        .ok_or(EnvError::NoDataDir)
}

/// Names of the environments under `data_dir`.
pub fn list(data_dir: &Path) -> Result<Vec<String>, EnvError> {
    let envs = data_dir.join("envs");
    let rd = match std::fs::read_dir(&envs) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(&envs)(e)),
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("env.toml").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names)
}

/// Well-known Windows folders, as the guest sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownFolder {
    Drive,
    ProgramFiles,
    ProgramFilesX86,
    CommonFiles,
    CommonFilesX86,
    ProgramData,
    Profile,
    AppData,
    LocalAppData,
    Desktop,
    Documents,
    StartMenu,
    Programs,
    Startup,
    CommonStartMenu,
    CommonPrograms,
    CommonDesktop,
    Windows,
    System32,
    SysWow64,
    Fonts,
    Temp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnvFile {
    name: String,
    user: String,
}

/// An opened environment.
#[derive(Debug, Clone)]
pub struct Env {
    name: String,
    user: String,
    root: PathBuf,
}

impl Env {
    /// Open environment `name` under `data_dir`, creating it if needed.
    pub fn open_or_create(data_dir: &Path, name: &str) -> Result<Env, EnvError> {
        let valid = !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            return Err(EnvError::BadName(name.to_string()));
        }
        let root = data_dir.join("envs").join(name);
        let meta = root.join("env.toml");
        let file = match std::fs::read_to_string(&meta) {
            Ok(text) => toml::from_str::<EnvFile>(&text).map_err(|e| EnvError::Parse {
                path: meta.clone(),
                message: e.to_string(),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let file = EnvFile {
                    name: name.to_string(),
                    user: default_user(),
                };
                std::fs::create_dir_all(&root).map_err(io_err(&root))?;
                let body = toml::to_string_pretty(&file).expect("env.toml serializes");
                std::fs::write(&meta, body).map_err(io_err(&meta))?;
                file
            }
            Err(e) => return Err(io_err(&meta)(e)),
        };
        let env = Env {
            name: name.to_string(),
            user: file.user,
            root,
        };
        env.create_skeleton()?;
        Ok(env)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The Windows user name apps see.
    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Host directory that backs the guest's `C:\`.
    pub fn drive_c(&self) -> PathBuf {
        self.root.join("windows").join("drive_c")
    }

    pub fn registry_path(&self) -> PathBuf {
        self.root.join("windows").join("registry.toml")
    }

    pub fn apps_path(&self) -> PathBuf {
        self.root.join("apps.toml")
    }

    pub fn registry(&self) -> Result<Registry, EnvError> {
        Registry::load(&self.registry_path())
    }

    pub fn apps(&self) -> Result<AppIndex, EnvError> {
        AppIndex::load(&self.apps_path())
    }

    /// Windows path of a known folder, without a trailing backslash
    /// (except `C:\`).
    pub fn known_folder(&self, f: KnownFolder) -> String {
        let profile = format!(r"C:\Users\{}", self.user);
        let roaming = format!(r"{profile}\AppData\Roaming");
        match f {
            KnownFolder::Drive => r"C:\".into(),
            KnownFolder::ProgramFiles => r"C:\Program Files".into(),
            KnownFolder::ProgramFilesX86 => r"C:\Program Files (x86)".into(),
            KnownFolder::CommonFiles => r"C:\Program Files\Common Files".into(),
            KnownFolder::CommonFilesX86 => r"C:\Program Files (x86)\Common Files".into(),
            KnownFolder::ProgramData => r"C:\ProgramData".into(),
            KnownFolder::AppData => roaming,
            KnownFolder::LocalAppData => format!(r"{profile}\AppData\Local"),
            KnownFolder::Desktop => format!(r"{profile}\Desktop"),
            KnownFolder::Documents => format!(r"{profile}\Documents"),
            KnownFolder::StartMenu => format!(r"{roaming}\Microsoft\Windows\Start Menu"),
            KnownFolder::Programs => format!(r"{roaming}\Microsoft\Windows\Start Menu\Programs"),
            KnownFolder::Startup => {
                format!(r"{roaming}\Microsoft\Windows\Start Menu\Programs\Startup")
            }
            KnownFolder::CommonStartMenu => r"C:\ProgramData\Microsoft\Windows\Start Menu".into(),
            KnownFolder::CommonPrograms => {
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs".into()
            }
            KnownFolder::CommonDesktop => r"C:\Users\Public\Desktop".into(),
            KnownFolder::Windows => r"C:\Windows".into(),
            KnownFolder::System32 => r"C:\Windows\System32".into(),
            KnownFolder::SysWow64 => r"C:\Windows\SysWOW64".into(),
            KnownFolder::Fonts => r"C:\Windows\Fonts".into(),
            KnownFolder::Temp => format!(r"{profile}\AppData\Local\Temp"),
            KnownFolder::Profile => profile,
        }
    }

    /// Folders created with the environment. Uninstalling never removes them.
    pub fn skeleton(&self) -> Vec<String> {
        use KnownFolder::*;
        [
            Drive,
            ProgramFiles,
            ProgramFilesX86,
            CommonFiles,
            CommonFilesX86,
            ProgramData,
            Profile,
            AppData,
            LocalAppData,
            Desktop,
            Documents,
            StartMenu,
            Programs,
            Startup,
            CommonStartMenu,
            CommonPrograms,
            CommonDesktop,
            Windows,
            System32,
            SysWow64,
            Fonts,
            Temp,
        ]
        .into_iter()
        .map(|f| self.known_folder(f))
        .collect()
    }

    fn create_skeleton(&self) -> Result<(), EnvError> {
        for dir in self.skeleton() {
            let p = self.host_path(&dir)?;
            std::fs::create_dir_all(&p).map_err(io_err(&p))?;
        }
        Ok(())
    }

    /// Host path for a Windows path on the guest's `C:` drive.
    ///
    /// Accepts `C:\a\b` and `C:/a/b`. Other drives, UNC paths and `..`
    /// components are rejected, so a path from an installer can never escape
    /// the environment. Each component matches an existing entry
    /// case-insensitively, the way Windows does, even on case-sensitive hosts.
    pub fn host_path(&self, windows_path: &str) -> Result<PathBuf, EnvError> {
        let bad = || EnvError::BadPath(windows_path.to_string());
        let rest = windows_path
            .strip_prefix("C:")
            .or_else(|| windows_path.strip_prefix("c:"))
            .ok_or_else(bad)?;
        let mut out = self.drive_c();
        for part in split_components(rest).ok_or_else(bad)? {
            out = match_case(&out, part);
        }
        Ok(out)
    }

    /// Host path for `relative` (a `\` or `/` separated path from an archive or
    /// installer table) inside the Windows directory `dir`.
    pub fn host_path_in(&self, dir: &str, relative: &str) -> Result<PathBuf, EnvError> {
        let bad = || EnvError::BadPath(relative.to_string());
        if relative.contains(':') {
            return Err(bad());
        }
        let mut out = self.host_path(dir)?;
        for part in split_components(relative).ok_or_else(bad)? {
            out = match_case(&out, part);
        }
        Ok(out)
    }
}

/// Join a Windows directory and a relative path with `\`.
pub fn win_join(dir: &str, rel: &str) -> String {
    let rel = rel.replace('/', "\\");
    let rel = rel.trim_start_matches('\\');
    if dir.ends_with('\\') {
        format!("{dir}{rel}")
    } else {
        format!("{dir}\\{rel}")
    }
}

/// Split a path on `\` and `/`, refusing anything that is not a plain name.
fn split_components(path: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    for part in path.split(['\\', '/']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains('\0') || part.contains(':') {
            return None;
        }
        parts.push(part);
    }
    Some(parts)
}

fn match_case(dir: &Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if exact.exists() {
        return exact;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                return e.path();
            }
        }
    }
    exact
}

fn default_user() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        "user".into()
    } else {
        clean
    }
}

pub(crate) fn read_toml<T: serde::de::DeserializeOwned + Default>(
    path: &Path,
) -> Result<T, EnvError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| EnvError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(io_err(path)(e)),
    }
}

pub(crate) fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), EnvError> {
    let body = toml::to_string_pretty(value).map_err(|e| EnvError::Parse {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    // Write then rename, so a crash never leaves a half-written file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).map_err(io_err(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io_err(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env(tag: &str) -> (PathBuf, Env) {
        let dir = std::env::temp_dir().join(format!("anyos-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let env = Env::open_or_create(&dir, "test").unwrap();
        (dir, env)
    }

    #[test]
    fn creates_layout_and_lists() {
        let (dir, env) = temp_env("layout");
        assert!(env.drive_c().join("Program Files (x86)").is_dir());
        assert!(env
            .host_path(&env.known_folder(KnownFolder::Programs))
            .unwrap()
            .is_dir());
        assert_eq!(list(&dir).unwrap(), vec!["test".to_string()]);
        // Reopening keeps the same user.
        let again = Env::open_or_create(&dir, "test").unwrap();
        assert_eq!(again.user(), env.user());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_stay_inside_and_ignore_case() {
        let (dir, env) = temp_env("paths");
        let p = env.host_path(r"c:\PROGRAM FILES\App\a.exe").unwrap();
        assert_eq!(
            p,
            env.drive_c()
                .join("Program Files")
                .join("App")
                .join("a.exe")
        );
        for bad in [
            r"D:\x",
            r"\\server\share",
            r"C:\..\..\etc",
            r"C:\a\..\b",
            "relative",
        ] {
            assert!(env.host_path(bad).is_err(), "{bad}");
        }
        assert!(env.host_path_in(r"C:\Program Files", "../../x").is_err());
        assert!(env.host_path_in(r"C:\Program Files", "C:/x").is_err());
        assert!(env.host_path_in(r"C:\Program Files", "a/b\\c").is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_names() {
        let dir = std::env::temp_dir();
        for bad in ["", "..", "a/b", "a b"] {
            assert!(Env::open_or_create(&dir, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn joins_windows_paths() {
        assert_eq!(win_join(r"C:\", "a/b"), r"C:\a\b");
        assert_eq!(win_join(r"C:\x", r"\y"), r"C:\x\y");
    }
}
