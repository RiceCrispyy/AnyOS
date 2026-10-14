//! # anyos-install
//!
//! Installs apps into an AnyOS environment by unpacking their installers in
//! Rust. No installer code runs: AnyOS reads the package itself, writes the
//! files into the environment's `C:` drive, applies registry entries and
//! records shortcuts. Installer logic that only code could do (MSI custom
//! actions, scripts) is skipped and listed in the app's notes.
//!
//! Supported now: Windows Installer packages (`.msi`), NSIS installers, ZIP
//! and 7-Zip archives, and ZIP or 7-Zip self-extracting `.exe` files. Other installers are
//! recognised (see `anyos-package`) and refused with the reason.
//!
//! Every file is placed through `Env::host_path`, so a hostile package cannot
//! write outside the environment.

mod archive;
mod msi;
mod nsis;

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyos_env::{AppRecord, Env, EnvError, RegValue, Shortcut};
use anyos_package::{identify, PackageKind};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Env(#[from] EnvError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}: not an installer or package AnyOS recognises")]
    NotAPackage(PathBuf),
    #[error("{kind} cannot be installed yet: {reason}")]
    Unsupported { kind: &'static str, reason: String },
    #[error("malformed {what}: {message}")]
    Malformed { what: &'static str, message: String },
    #[error("{0} is already installed in this environment (uninstall it first)")]
    AlreadyInstalled(String),
    #[error("no installed app called {0:?} in this environment")]
    NotInstalled(String),
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What an install did.
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub app: AppRecord,
    pub bytes_written: u64,
    pub registry_values: usize,
}

/// What an uninstall did.
#[derive(Debug, Clone)]
pub struct UninstallReport {
    pub app: AppRecord,
    pub files_removed: usize,
    /// Files still used by another installed app, so left in place.
    pub files_shared: usize,
    pub registry_keys_removed: usize,
}

/// Name, version and so on, as an installer format reports them.
#[derive(Debug, Clone, Default)]
pub(crate) struct Product {
    pub name: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
}

/// What a format installer hands back once its files are written.
#[derive(Debug, Default)]
pub(crate) struct Installed {
    pub install_dir: Option<String>,
    pub main: Option<String>,
    pub shortcuts: Vec<Shortcut>,
    pub registry: Vec<(String, String, RegValue)>,
    /// Keys to create even without values.
    pub keys: Vec<String>,
}

/// Writes files into the environment and remembers them, so a failed install
/// can be rolled back and a finished one uninstalled.
pub(crate) struct Writer<'a> {
    pub env: &'a Env,
    pub files: Vec<String>,
    pub folders: Vec<String>,
    created: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub bytes: u64,
}

impl<'a> Writer<'a> {
    fn new(env: &'a Env) -> Self {
        Writer {
            env,
            files: Vec::new(),
            folders: Vec::new(),
            created: Vec::new(),
            notes: Vec::new(),
            bytes: 0,
        }
    }

    pub fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }

    /// Write `data` to the Windows path `dir\relative`.
    pub fn write(
        &mut self,
        dir: &str,
        relative: &str,
        data: &mut dyn Read,
    ) -> Result<(), InstallError> {
        let host = self.env.host_path_in(dir, relative)?;
        if let Some(parent) = host.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let existed = host.exists();
        let mut out = std::fs::File::create(&host).map_err(io_err(&host))?;
        let n = std::io::copy(data, &mut out).map_err(io_err(&host))?;
        if !existed {
            self.created.push(host);
        }
        self.bytes += n;
        let win = anyos_env::win_join(dir, relative);
        if !self.files.iter().any(|f| f.eq_ignore_ascii_case(&win)) {
            self.files.push(win);
        }
        Ok(())
    }

    /// Create an empty Windows directory (MSI `CreateFolder`, archive folders).
    pub fn mkdir(&mut self, dir: &str) -> Result<(), InstallError> {
        let host = self.env.host_path(dir)?;
        std::fs::create_dir_all(&host).map_err(io_err(&host))?;
        if !self.folders.iter().any(|f| f.eq_ignore_ascii_case(dir)) {
            self.folders.push(dir.to_string());
        }
        Ok(())
    }

    fn rollback(self) {
        for f in self.created.iter().rev() {
            let _ = std::fs::remove_file(f);
        }
        prune_dirs(self.env, &self.files, &self.folders);
    }
}

/// Identify `path` and install it into `env`.
pub fn install(env: &Env, path: &Path) -> Result<InstallReport, InstallError> {
    let bytes = std::fs::read(path).map_err(io_err(path))?;
    let image = if bytes.starts_with(b"MZ") {
        anyos_pe::parse(&bytes).ok()
    } else {
        None
    };
    let package =
        identify(&bytes, image.as_ref()).ok_or_else(|| InstallError::NotAPackage(path.into()))?;
    let source = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "App".into());

    let payload = &bytes[package.payload_offset.unwrap_or(0) as usize..];
    let mut w = Writer::new(env);
    let mut nsis = None;
    let product = match package.kind {
        PackageKind::Msi => msi::product(&bytes)?,
        PackageKind::Nsis => {
            let offset = package.payload_offset.unwrap_or(0) as usize;
            let n = nsis.insert(nsis::Nsis::parse(&bytes, offset, &w)?);
            n.product(&stem)
        }
        PackageKind::Zip | PackageKind::ZipSfx => archive::zip_product(payload, &stem)?,
        PackageKind::SevenZip | PackageKind::SevenZipSfx => {
            archive::sevenz_product(payload, &stem)?
        }
        other => return Err(unsupported(other)),
    };

    let mut index = env.apps()?;
    if index.find(&product.name).is_some() {
        return Err(InstallError::AlreadyInstalled(product.name));
    }

    let result = match (package.kind, nsis.as_mut()) {
        (PackageKind::Msi, _) => msi::install(&mut w, path, &bytes, &product),
        (_, Some(n)) => n.install(&mut w, &product),
        (PackageKind::Zip | PackageKind::ZipSfx, _) => {
            archive::install_zip(&mut w, payload, &product)
        }
        _ => archive::install_7z(&mut w, payload, &product),
    };
    let installed = match result {
        Ok(i) => i,
        Err(e) => {
            w.rollback();
            return Err(e);
        }
    };

    let mut registry = env.registry()?;
    let mut created_keys: Vec<String> = Vec::new();
    let mut note_key = |registry: &mut anyos_env::Registry, key: &str| {
        if !registry.key_exists(key) {
            created_keys.push(registry.create_key(key));
        }
    };
    let mut values: Vec<[String; 2]> = Vec::new();
    for (key, name, value) in &installed.registry {
        note_key(&mut registry, key);
        registry.set(key, name, value.clone());
        let entry = [anyos_env::registry::normalize_key(key), name.clone()];
        if !values.contains(&entry) {
            values.push(entry);
        }
    }
    for key in &installed.keys {
        note_key(&mut registry, key);
    }
    registry.save(&env.registry_path())?;

    let main = installed
        .main
        .clone()
        .or_else(|| guess_main(&w.files, &product.name));
    let app = AppRecord {
        id: index.unused_id(&product.name),
        name: product.name.clone(),
        version: product.version.clone(),
        publisher: product.publisher.clone(),
        source,
        package: package.kind.display_name().to_string(),
        install_dir: installed.install_dir.clone(),
        main,
        shortcuts: installed.shortcuts,
        files: w.files,
        folders: w.folders,
        registry_keys: created_keys,
        registry_values: values,
        notes: w.notes,
    };
    index.add(app.clone());
    index.save(&env.apps_path())?;
    Ok(InstallReport {
        app,
        bytes_written: w.bytes,
        registry_values: installed.registry.len(),
    })
}

/// Remove an installed app's files, registry keys and index entry.
pub fn uninstall(env: &Env, id_or_name: &str) -> Result<UninstallReport, InstallError> {
    let mut index = env.apps()?;
    let app = index
        .find(id_or_name)
        .cloned()
        .ok_or_else(|| InstallError::NotInstalled(id_or_name.to_string()))?;
    let shared: HashSet<String> = index
        .apps
        .iter()
        .filter(|a| a.id != app.id)
        .flat_map(|a| a.files.iter().map(|f| f.to_ascii_lowercase()))
        .collect();

    let mut removed = 0;
    let mut kept = 0;
    let mut gone = Vec::new();
    for f in &app.files {
        if shared.contains(&f.to_ascii_lowercase()) {
            kept += 1;
            continue;
        }
        if let Ok(host) = env.host_path(f) {
            if std::fs::remove_file(&host).is_ok() {
                removed += 1;
            }
        }
        gone.push(f.clone());
    }
    prune_dirs(env, &gone, &app.folders);

    let mut registry = env.registry()?;
    for [key, name] in &app.registry_values {
        registry.delete_value(key, name);
    }
    let mut keys = app.registry_keys.clone();
    keys.sort_by_key(|k| std::cmp::Reverse(k.matches('\\').count()));
    let keys_removed = keys.iter().filter(|k| registry.delete_if_empty(k)).count();
    registry.save(&env.registry_path())?;

    index.remove(&app.id);
    index.save(&env.apps_path())?;
    Ok(UninstallReport {
        app,
        files_removed: removed,
        files_shared: kept,
        registry_keys_removed: keys_removed,
    })
}

fn unsupported(kind: PackageKind) -> InstallError {
    let reason = match kind {
        PackageKind::InnoSetup => "unpacking this installer format is planned next (see issue #5)",
        PackageKind::WixBurn | PackageKind::InstallShield => {
            "this installer has to run as a program, which needs Windows program support"
        }
        PackageKind::MsiPatch | PackageKind::MsiTransform => {
            "patches and transforms apply to an installed MSI; install the main .msi"
        }
        PackageKind::Deb
        | PackageKind::Rpm
        | PackageKind::AppImage
        | PackageKind::MacPkg
        | PackageKind::Dmg => "Linux and macOS packages are planned after the Windows formats",
        _ => "not supported",
    };
    InstallError::Unsupported {
        kind: kind.display_name(),
        reason: reason.into(),
    }
}

/// Remove `folders` and the parents of `files` once they are empty, deepest
/// first, never touching the environment's standard folders.
fn prune_dirs(env: &Env, files: &[String], folders: &[String]) {
    let keep: HashSet<String> = env
        .skeleton()
        .into_iter()
        .map(|d| d.to_ascii_lowercase())
        .collect();
    let mut dirs: Vec<String> = Vec::new();
    // A folder is handled like a file inside it.
    let inside: Vec<String> = folders.iter().map(|d| format!("{d}\\-")).collect();
    for f in files.iter().chain(&inside) {
        let mut cur = f.as_str();
        while let Some(i) = cur.rfind('\\') {
            cur = &cur[..i];
            if cur.len() <= 2 || keep.contains(&cur.to_ascii_lowercase()) {
                break;
            }
            if !dirs.iter().any(|d| d.eq_ignore_ascii_case(cur)) {
                dirs.push(cur.to_string());
            }
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('\\').count()));
    for d in dirs {
        if let Ok(host) = env.host_path(&d) {
            // Fails, as intended, when the folder still has anything in it.
            let _ = std::fs::remove_dir(host);
        }
    }
}

/// Pick the program a user most likely wants to start: an `.exe` that is not
/// an uninstaller, updater or helper, preferring shallow paths and names that
/// look like the app's name.
pub(crate) fn guess_main(files: &[String], app_name: &str) -> Option<String> {
    const NOT_MAIN: &[&str] = &[
        "unins", "uninst", "setup", "install", "update", "crash", "helper", "report", "vcredist",
        "dotnet", "elevat",
    ];
    let wanted = anyos_env::apps::make_id(app_name);
    files
        .iter()
        .filter(|f| f.to_ascii_lowercase().ends_with(".exe"))
        .filter(|f| {
            let base = file_base(f).to_ascii_lowercase();
            !NOT_MAIN.iter().any(|w| base.contains(w))
        })
        .min_by_key(|f| {
            let base = anyos_env::apps::make_id(file_base(f).trim_end_matches(".exe"));
            let named = !(wanted.contains(&base) || base.contains(&wanted));
            (named, f.matches('\\').count(), f.len())
        })
        .cloned()
}

fn file_base(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_program_guess() {
        let files: Vec<String> = [
            r"C:\Program Files\Tool\unins000.exe",
            r"C:\Program Files\Tool\bin\helper.exe",
            r"C:\Program Files\Tool\other.exe",
            r"C:\Program Files\Tool\tool.exe",
            r"C:\Program Files\Tool\readme.txt",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            guess_main(&files, "Tool").as_deref(),
            Some(r"C:\Program Files\Tool\tool.exe")
        );
        assert_eq!(
            guess_main(&files, "Something Else").as_deref(),
            Some(r"C:\Program Files\Tool\tool.exe")
        );
        assert_eq!(guess_main(&files[4..], "Tool"), None);
    }
}
