//! The apps installed in AnyOS environments, as the start screen lists them.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub name: String,
    pub version: Option<String>,
    /// `Installed 2026-10-14 06:15 UTC · 12.3 MB`, as far as it is known.
    pub detail: String,
    /// The environment it is installed in, when there is more than one.
    pub env: Option<String>,
    /// The environment name, always set (needed to address the app for uninstall).
    pub env_name: String,
    /// The app id from AppIndex (needed to address the app for uninstall).
    pub id: String,
    /// Programs it can start, best first: the one the installer named, then
    /// the other `.exe` files in its folder. Each is (label, host path).
    pub programs: Vec<(String, PathBuf)>,
}

/// Apps in every environment, most recently installed first. Nothing is
/// created when no environment exists yet.
pub fn load() -> Vec<InstalledApp> {
    let Ok(dir) = anyos_env::data_dir() else {
        return Vec::new();
    };
    let names = anyos_env::list(&dir).unwrap_or_default();
    let show_env = names.len() > 1;
    let mut apps: Vec<(Option<String>, InstalledApp)> = Vec::new();
    for name in &names {
        let Ok(env) = anyos_env::Env::open_or_create(&dir, name) else {
            continue;
        };
        let Ok(index) = env.apps() else { continue };
        for a in index.by_install_date() {
            apps.push((
                a.installed_at.clone(),
                InstalledApp {
                    name: a.name.clone(),
                    version: a.version.clone(),
                    detail: detail(a),
                    env: show_env.then(|| name.clone()),
                    env_name: name.clone(),
                    id: a.id.clone(),
                    programs: programs(&env, a),
                },
            ));
        }
    }
    // RFC 3339 UTC times sort as text; apps without a time go last.
    apps.sort_by(|x, y| y.0.cmp(&x.0));
    apps.into_iter().map(|(_, a)| a).collect()
}

/// What an app can start: its main program, then the other programs in its
/// install folder (not uninstallers or setup helpers).
fn programs(env: &anyos_env::Env, a: &anyos_env::AppRecord) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Some(p) = a.main.as_deref().and_then(|m| env.host_path(m).ok()) {
        if p.is_file() {
            out.push((file_label(&p), p));
        }
    }
    let mut add = |p: PathBuf| {
        if !out.iter().any(|(_, q)| q == &p) {
            out.push((file_label(&p), p));
        }
    };
    // Shortcuts point at what the installer meant people to start.
    for s in &a.shortcuts {
        if let Ok(p) = env.host_path(&s.target) {
            if is_program(&p) {
                add(p);
            }
        }
    }
    let mut found: Vec<PathBuf> = a
        .files
        .iter()
        .filter(|f| f.to_ascii_lowercase().ends_with(".exe"))
        .filter_map(|f| env.host_path(f).ok())
        .filter(|p| is_program(p))
        .collect();
    // Programs near the top of the install folder first.
    found.sort_by_key(|p| (p.components().count(), file_label(p).to_ascii_lowercase()));
    for p in found {
        add(p);
    }
    out
}

fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// An `.exe` that exists and is not an uninstaller, updater or crash reporter.
fn is_program(p: &Path) -> bool {
    let name = file_label(p).to_ascii_lowercase();
    let helper = ["unins", "uninst", "setup", "update", "crash", "helper"]
        .iter()
        .any(|h| name.starts_with(h) || name.contains(&format!("_{h}")));
    name.ends_with(".exe") && !helper && p.is_file()
}

fn detail(a: &anyos_env::AppRecord) -> String {
    let when = a
        .installed_at
        .as_deref()
        .and_then(anyos_env::clock::parse_rfc3339)
        .map(|t| {
            let s = anyos_env::clock::format_rfc3339(t);
            format!("Installed {} {} UTC", &s[..10], &s[11..16])
        });
    let size = a.size_bytes.map(crate::human_size);
    match (when, size) {
        (Some(w), Some(s)) => format!("{w} · {s}"),
        (Some(w), None) => w,
        (None, Some(s)) => s,
        (None, None) => a.package.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::is_program;

    #[test]
    fn uninstallers_and_helpers_are_not_offered() {
        let dir = std::env::temp_dir().join(format!("anyos-gui-installed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "App.exe",
            "unins000.exe",
            "Uninstall.exe",
            "app_helper.exe",
            "readme.txt",
        ] {
            std::fs::write(dir.join(f), b"MZ").unwrap();
        }
        assert!(is_program(&dir.join("App.exe")));
        assert!(!is_program(&dir.join("unins000.exe")));
        assert!(!is_program(&dir.join("Uninstall.exe")));
        assert!(!is_program(&dir.join("app_helper.exe")));
        assert!(!is_program(&dir.join("readme.txt")));
        assert!(!is_program(&dir.join("missing.exe")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
