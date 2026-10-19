//! The environment's registry: keys and values, stored as TOML.
//!
//! Key paths use full root names (`HKEY_LOCAL_MACHINE\Software\Vendor`).
//! Abbreviated roots (`HKLM`, `HKCU`, `HKU`) are accepted, and
//! `HKEY_CLASSES_ROOT` is stored under `HKEY_LOCAL_MACHINE\Software\Classes`.
//! Lookups ignore case, like Windows; the first spelling written is kept.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::EnvError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegValue {
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    Dword(u32),
    /// Stored as a hex string, since TOML integers are signed 64-bit.
    Qword(#[serde(with = "hex_u64")] u64),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// Key path -> value name (`""` is the default value) -> value.
    #[serde(default)]
    keys: BTreeMap<String, BTreeMap<String, RegValue>>,
}

/// Canonical spelling of a key path's root.
pub fn normalize_key(path: &str) -> String {
    let path = path.trim_matches('\\');
    let (root, rest) = path.split_once('\\').unwrap_or((path, ""));
    let (root, prefix) = match root.to_ascii_uppercase().as_str() {
        "HKLM" | "HKEY_LOCAL_MACHINE" => ("HKEY_LOCAL_MACHINE", ""),
        "HKCU" | "HKEY_CURRENT_USER" => ("HKEY_CURRENT_USER", ""),
        "HKU" | "HKEY_USERS" => ("HKEY_USERS", ""),
        "HKCR" | "HKEY_CLASSES_ROOT" => ("HKEY_LOCAL_MACHINE", "Software\\Classes"),
        _ => return path.to_string(),
    };
    [root, prefix, rest]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\\")
}

impl Registry {
    pub fn load(path: &Path) -> Result<Registry, EnvError> {
        crate::read_toml(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), EnvError> {
        crate::write_toml(path, self)
    }

    fn find(&self, key: &str) -> Option<&String> {
        self.keys.keys().find(|k| k.eq_ignore_ascii_case(key))
    }

    /// Create `key` (and nothing else) if it does not exist. Returns the
    /// stored spelling.
    pub fn create_key(&mut self, key: &str) -> String {
        let key = normalize_key(key);
        if let Some(k) = self.find(&key) {
            return k.clone();
        }
        self.keys.insert(key.clone(), BTreeMap::new());
        key
    }

    pub fn set(&mut self, key: &str, name: &str, value: RegValue) {
        let key = self.create_key(key);
        let values = self.keys.get_mut(&key).expect("key just created");
        let existing = values
            .keys()
            .find(|n| n.eq_ignore_ascii_case(name))
            .cloned();
        values.insert(existing.unwrap_or_else(|| name.to_string()), value);
    }

    pub fn get(&self, key: &str, name: &str) -> Option<&RegValue> {
        let key = self.find(&normalize_key(key))?;
        self.keys[key]
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    pub fn key_exists(&self, key: &str) -> bool {
        self.find(&normalize_key(key)).is_some()
    }

    /// Delete `key` and every key below it. Returns how many keys were removed.
    pub fn delete_tree(&mut self, key: &str) -> usize {
        let key = normalize_key(key).to_ascii_lowercase();
        let below = format!("{key}\\");
        let before = self.keys.len();
        self.keys.retain(|k, _| {
            let k = k.to_ascii_lowercase();
            k != key && !k.starts_with(&below)
        });
        before - self.keys.len()
    }

    /// Delete `key` if it has no values and no subkeys.
    pub fn delete_if_empty(&mut self, key: &str) -> bool {
        let Some(found) = self.find(&normalize_key(key)).cloned() else {
            return false;
        };
        let below = format!("{}\\", found.to_ascii_lowercase());
        let has_children = self
            .keys
            .keys()
            .any(|k| k.to_ascii_lowercase().starts_with(&below));
        if has_children || !self.keys[&found].is_empty() {
            return false;
        }
        self.keys.remove(&found);
        true
    }

    pub fn delete_value(&mut self, key: &str, name: &str) -> bool {
        let Some(key) = self.find(&normalize_key(key)).cloned() else {
            return false;
        };
        let values = self.keys.get_mut(&key).expect("found");
        let before = values.len();
        values.retain(|n, _| !n.eq_ignore_ascii_case(name));
        values.len() != before
    }

    /// Names of the keys directly below `key`, including keys that only exist
    /// because something deeper does, in sorted order.
    pub fn subkeys(&self, key: &str) -> Vec<String> {
        let below = format!("{}\\", normalize_key(key).to_ascii_lowercase());
        let mut out: Vec<String> = Vec::new();
        for k in self.keys.keys() {
            if k.len() <= below.len() || !k.to_ascii_lowercase().starts_with(&below) {
                continue;
            }
            let child = k[below.len()..].split('\\').next().unwrap_or_default();
            if !child.is_empty() && !out.iter().any(|c| c.eq_ignore_ascii_case(child)) {
                out.push(child.to_string());
            }
        }
        out
    }

    /// The values of `key`, in name order.
    pub fn values(&self, key: &str) -> Vec<(String, RegValue)> {
        self.find(&normalize_key(key))
            .map(|k| {
                self.keys[k]
                    .iter()
                    .map(|(n, v)| (n.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

mod hex_u64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{v:x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        let hex = s.strip_prefix("0x").unwrap_or(&s);
        u64::from_str_radix(hex, 16).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subkeys_and_values_enumerate() {
        let mut r = Registry::default();
        r.set(r"HKCU\Software\A\Deep\Er", "x", RegValue::Dword(1));
        r.set(r"HKCU\Software\b", "", RegValue::Sz("v".into()));
        r.set(r"HKCU\Software\b", "n", RegValue::Dword(2));
        assert_eq!(r.subkeys(r"HKEY_CURRENT_USER\software"), ["A", "b"]);
        assert_eq!(r.subkeys(r"HKCU\Software\A"), ["Deep"]);
        assert!(r.subkeys(r"HKCU\Software\b").is_empty());
        let names: Vec<_> = r
            .values(r"hkcu\software\B")
            .into_iter()
            .map(|v| v.0)
            .collect();
        assert_eq!(names, ["", "n"]);
    }

    #[test]
    fn roots_are_normalized() {
        assert_eq!(
            normalize_key(r"HKLM\Software\X"),
            r"HKEY_LOCAL_MACHINE\Software\X"
        );
        assert_eq!(
            normalize_key(r"HKCR\.txt"),
            r"HKEY_LOCAL_MACHINE\Software\Classes\.txt"
        );
        assert_eq!(normalize_key(r"hkcu\"), "HKEY_CURRENT_USER");
    }

    #[test]
    fn set_get_delete_ignore_case() {
        let mut r = Registry::default();
        r.set(
            r"HKLM\Software\Vendor\App",
            "Path",
            RegValue::Sz("C:\\x".into()),
        );
        r.set(r"HKLM\Software\Vendor\App\Sub", "", RegValue::Dword(1));
        r.set(
            r"HKEY_LOCAL_MACHINE\SOFTWARE\vendor\app",
            "path",
            RegValue::Sz("C:\\y".into()),
        );
        assert_eq!(
            r.get(r"hklm\software\vendor\APP", "PATH"),
            Some(&RegValue::Sz("C:\\y".into()))
        );
        assert_eq!(r.len(), 2);
        assert_eq!(r.delete_tree(r"HKLM\Software\Vendor\App"), 2);
        assert!(r.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("anyos-reg-{}", std::process::id()));
        let path = dir.join("registry.toml");
        let mut r = Registry::default();
        r.set(r"HKCU\Software\A", "", RegValue::Sz("default".into()));
        r.set(r"HKCU\Software\A", "Bin", RegValue::Binary(vec![1, 2, 255]));
        r.set(
            r"HKCU\Software\A",
            "Multi",
            RegValue::MultiSz(vec!["a".into(), "b".into()]),
        );
        r.set(r"HKCU\Software\A", "Big", RegValue::Qword(u64::MAX));
        r.create_key(r"HKCU\Software\Empty");
        r.save(&path).unwrap();
        assert_eq!(Registry::load(&path).unwrap(), r);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
