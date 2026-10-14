//! The index of apps installed in an environment (`apps.toml`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::EnvError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub name: String,
    /// Windows folder the shortcut was placed in (Start menu, Desktop, ...).
    pub folder: String,
    /// Windows path the shortcut opens.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRecord {
    /// Unique, lower-case id used on the command line.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// File name of the installer it came from.
    pub source: String,
    /// Kind of installer, as `anyos inspect` names it.
    pub package: String,
    /// Windows folder the app was installed into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_dir: Option<String>,
    /// Windows path of the program to start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortcuts: Vec<Shortcut>,
    /// Every file written, as Windows paths, so uninstall can remove them.
    #[serde(default)]
    pub files: Vec<String>,
    /// Folders created without files in them, removed on uninstall when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// Registry keys this install created; removed on uninstall once empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry_keys: Vec<String>,
    /// Registry values this install wrote, as `[key, name]`; removed on uninstall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry_values: Vec<[String; 2]>,
    /// Things the installer would have done that AnyOS skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppIndex {
    #[serde(default, rename = "app")]
    pub apps: Vec<AppRecord>,
}

/// Lower-case id made of letters, digits and dashes.
pub fn make_id(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    let id = id.trim_end_matches('-').to_string();
    if id.is_empty() {
        "app".into()
    } else {
        id
    }
}

impl AppIndex {
    pub fn load(path: &Path) -> Result<AppIndex, EnvError> {
        crate::read_toml(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), EnvError> {
        crate::write_toml(path, self)
    }

    /// Find an app by id or by name, ignoring case.
    pub fn find(&self, id_or_name: &str) -> Option<&AppRecord> {
        self.apps
            .iter()
            .find(|a| a.id.eq_ignore_ascii_case(id_or_name))
            .or_else(|| {
                self.apps
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case(id_or_name))
            })
    }

    /// An id based on `name` that no installed app uses yet.
    pub fn unused_id(&self, name: &str) -> String {
        let base = make_id(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.apps.iter().any(|a| a.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    pub fn add(&mut self, app: AppRecord) {
        self.apps.retain(|a| a.id != app.id);
        self.apps.push(app);
        self.apps.sort_by(|a, b| a.id.cmp(&b.id));
    }

    pub fn remove(&mut self, id: &str) -> Option<AppRecord> {
        let i = self.apps.iter().position(|a| a.id == id)?;
        Some(self.apps.remove(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_slugs_and_unique() {
        assert_eq!(make_id("Notepad++ (64-bit)"), "notepad-64-bit");
        assert_eq!(make_id("!!!"), "app");
        let mut idx = AppIndex::default();
        idx.add(AppRecord {
            id: "tool".into(),
            name: "Tool".into(),
            ..Default::default()
        });
        assert_eq!(idx.unused_id("Tool"), "tool-2");
        assert_eq!(idx.find("TOOL").unwrap().id, "tool");
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("anyos-apps-{}", std::process::id()));
        let path = dir.join("apps.toml");
        let mut idx = AppIndex::default();
        idx.add(AppRecord {
            id: "a".into(),
            name: "A".into(),
            version: Some("1.0".into()),
            source: "a.msi".into(),
            package: "MSI".into(),
            main: Some(r"C:\Program Files\A\a.exe".into()),
            shortcuts: vec![Shortcut {
                name: "A".into(),
                folder: r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs".into(),
                target: r"C:\Program Files\A\a.exe".into(),
                arguments: None,
                working_dir: None,
            }],
            files: vec![r"C:\Program Files\A\a.exe".into()],
            ..Default::default()
        });
        idx.save(&path).unwrap();
        assert_eq!(AppIndex::load(&path).unwrap(), idx);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
