//! The apps installed in the default environment, as the start screen lists them.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub name: String,
    pub version: Option<String>,
    /// `Installed 2026-10-14 06:15 UTC · 12.3 MB`, as far as it is known.
    pub detail: String,
    /// Host path of the program to start, when the install named one.
    pub program: Option<PathBuf>,
}

/// Apps in the default environment, most recently installed first. Nothing is
/// created when the environment does not exist yet.
pub fn load() -> Vec<InstalledApp> {
    let Ok(dir) = anyos_env::data_dir() else {
        return Vec::new();
    };
    let exists = anyos_env::list(&dir)
        .map(|names| names.iter().any(|n| n == anyos_env::DEFAULT_ENV))
        .unwrap_or(false);
    if !exists {
        return Vec::new();
    }
    let Ok(env) = anyos_env::Env::open_or_create(&dir, anyos_env::DEFAULT_ENV) else {
        return Vec::new();
    };
    let Ok(index) = env.apps() else {
        return Vec::new();
    };
    index
        .by_install_date()
        .into_iter()
        .map(|a| InstalledApp {
            name: a.name.clone(),
            version: a.version.clone(),
            detail: detail(a),
            program: a.main.as_deref().and_then(|m| env.host_path(m).ok()),
        })
        .collect()
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
