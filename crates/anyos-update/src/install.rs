//! Swapping a downloaded release in for the running one.
//!
//! The release archive holds one folder with `anyos`, `anyos-gui` and, on
//! macOS, `AnyOS.app`. We replace the item the running program belongs to (its
//! own binary, or the whole `.app` bundle it runs from) plus any of the other
//! AnyOS items that sit beside it, so the CLI and the app stay in step.
//!
//! Each item is first unpacked into a staging folder inside the install folder
//! (same file system, so moves are renames), then swapped in:
//! `item` -> `item.old`, `staged` -> `item`, then `item.old` is deleted. Windows
//! refuses to delete a running `.exe` but allows renaming it, so there the
//! `.old` file stays until [`cleanup_leftovers`] removes it on the next launch.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;

const EXE: &str = std::env::consts::EXE_SUFFIX;
const BUNDLE: &str = "AnyOS.app";
const OLD: &str = ".old";
const STAGING_PREFIX: &str = ".anyos-update-";

/// Where the running AnyOS is installed and what to replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Folder holding the AnyOS items (the binary's folder, or the folder the
    /// `.app` bundle sits in).
    pub dir: PathBuf,
    /// Name in `dir` of the item the running program is part of: its binary,
    /// or its `.app` bundle (which may have been renamed).
    pub own_item: String,
    /// Name of that item inside the release archive.
    pub own_source: String,
}

impl Layout {
    /// Works out the layout from the running program's path.
    pub fn current() -> Result<Layout, Error> {
        let exe = std::env::current_exe().map_err(|e| Error::io(Path::new("current exe"), e))?;
        // Resolve symlinks (e.g. /usr/local/bin/anyos -> .../AnyOS.app/...).
        let exe = fs::canonicalize(&exe).map_err(|e| Error::io(&exe, e))?;
        Layout::for_exe(&exe)
    }

    /// Layout for a program at `exe` (already canonical).
    pub fn for_exe(exe: &Path) -> Result<Layout, Error> {
        let bad = || Error::Layout(exe.to_path_buf());
        if let Some(bundle) = enclosing_bundle(exe) {
            return Ok(Layout {
                dir: bundle.parent().ok_or_else(bad)?.to_path_buf(),
                own_item: name_of(&bundle).ok_or_else(bad)?,
                own_source: BUNDLE.to_string(),
            });
        }
        let name = name_of(exe).ok_or_else(bad)?;
        Ok(Layout {
            dir: exe.parent().ok_or_else(bad)?.to_path_buf(),
            own_source: name.clone(),
            own_item: name,
        })
    }

    /// The program to start after an update to get the new version running.
    pub fn relaunch_path(&self, exe_name: &str) -> PathBuf {
        let own = self.dir.join(&self.own_item);
        if self.own_source == BUNDLE {
            own.join("Contents/MacOS").join(exe_name)
        } else {
            self.dir.join(exe_name)
        }
    }

    /// Whether this layout runs from a macOS `.app` bundle.
    pub fn is_bundle(&self) -> bool {
        self.own_source == BUNDLE
    }

    /// `(name in dir, name in archive)` for everything to replace.
    fn items(&self, new_root: &Path) -> Vec<(String, String)> {
        let mut items = vec![(self.own_item.clone(), self.own_source.clone())];
        for name in [
            format!("anyos{EXE}"),
            format!("anyos-gui{EXE}"),
            BUNDLE.to_string(),
        ] {
            if items.iter().any(|(_, src)| *src == name) {
                continue;
            }
            if self.dir.join(&name).exists() && new_root.join(&name).exists() {
                items.push((name.clone(), name));
            }
        }
        items
    }
}

/// `/x/AnyOS.app` for `/x/AnyOS.app/Contents/MacOS/anyos-gui`.
fn enclosing_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_app = bundle
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| bundle.to_path_buf())
}

fn name_of(p: &Path) -> Option<String> {
    p.file_name().and_then(|n| n.to_str()).map(str::to_string)
}

/// A staging folder inside `dir`, removed when dropped.
pub(crate) struct Staging(PathBuf);

impl Staging {
    pub(crate) fn new(dir: &Path) -> Result<Staging, Error> {
        let p = dir.join(format!("{STAGING_PREFIX}{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir(&p).map_err(|e| Error::io(&p, e))?;
        Ok(Staging(p))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Unpacks `archive` into `into` and returns the release folder inside it.
pub(crate) fn unpack(archive: &Path, into: &Path) -> Result<PathBuf, Error> {
    let name = archive.to_string_lossy();
    let file = fs::File::open(archive).map_err(|e| Error::io(archive, e))?;
    let bad = |e: &dyn std::fmt::Display| Error::BadArchive(e.to_string());
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| bad(&e))?;
        zip.extract(into).map_err(|e| bad(&e))?;
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        tar.set_preserve_permissions(true);
        tar.unpack(into).map_err(|e| bad(&e))?;
    }
    // Archives hold exactly one folder, `anyos-<tag>-<target>`.
    let mut dirs = fs::read_dir(into)
        .map_err(|e| Error::io(into, e))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path());
    match (dirs.next(), dirs.next()) {
        (Some(d), None) => Ok(d),
        _ => Err(Error::BadArchive(
            "expected a single release folder in the archive".into(),
        )),
    }
}

/// Replaces the installed items described by `layout` with those in `new_root`.
/// Returns the names replaced.
pub(crate) fn swap_in(layout: &Layout, new_root: &Path) -> Result<Vec<String>, Error> {
    let items = layout.items(new_root);
    let (_own, own_src) = &items[0];
    if !new_root.join(own_src).exists() {
        return Err(Error::BadArchive(format!(
            "release archive is missing {own_src} — it may have been built \
             from an older commit that predates the macOS app. \
             Download the latest release manually from \
             https://github.com/RiceCrispyy/AnyOS/releases"
        )));
    }
    let mut done = Vec::new();
    for (name, src) in &items {
        let target = layout.dir.join(name);
        replace(&target, &new_root.join(src))?;
        done.push(name.clone());
    }
    Ok(done)
}

/// Moves `new` into place at `target`, keeping the old one until it is safe to
/// drop. On failure the old item is put back.
fn replace(target: &Path, new: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    if new.is_file() {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(new)
            .map_err(|e| Error::io(new, e))?
            .permissions();
        perm.set_mode(perm.mode() | 0o755);
        fs::set_permissions(new, perm).map_err(|e| Error::io(new, e))?;
    }
    let old = old_path(target);
    remove_any(&old);
    let had_old = target.exists();
    if had_old {
        fs::rename(target, &old).map_err(|e| Error::io(target, e))?;
    }
    if let Err(e) = fs::rename(new, target) {
        if had_old {
            let _ = fs::rename(&old, target);
        }
        return Err(Error::io(target, e));
    }
    // Fails on Windows for a running .exe; cleanup_leftovers gets it next time.
    remove_any(&old);
    Ok(())
}

fn old_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(OLD);
    PathBuf::from(s)
}

fn remove_any(p: &Path) {
    let _ = if p.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    };
}

/// Deletes what an earlier update left behind beside `layout`: `.old` copies
/// of AnyOS items (Windows keeps a running `.exe` until it exits) and staging
/// folders from an update that was cut short. Best effort.
pub fn cleanup_leftovers(layout: &Layout) {
    let Ok(entries) = fs::read_dir(&layout.dir) else {
        return;
    };
    let ours = [
        format!("anyos{EXE}"),
        format!("anyos-gui{EXE}"),
        BUNDLE.to_string(),
        layout.own_item.clone(),
    ];
    for e in entries.filter_map(Result::ok) {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let stale_old = name
            .strip_suffix(OLD)
            .is_some_and(|base| ours.iter().any(|o| o == base));
        let stale_staging = name
            .strip_prefix(STAGING_PREFIX)
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| pid != std::process::id());
        if stale_old || stale_staging {
            remove_any(&e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("anyos-update-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn plain_binary_layout() {
        let l = Layout::for_exe(Path::new("/opt/anyos/anyos")).unwrap();
        assert_eq!(l.dir, Path::new("/opt/anyos"));
        assert_eq!(l.own_item, "anyos");
        assert!(!l.is_bundle());
        assert_eq!(
            l.relaunch_path("anyos-gui"),
            Path::new("/opt/anyos/anyos-gui")
        );
    }

    #[test]
    fn app_bundle_layout_replaces_the_whole_bundle() {
        let l =
            Layout::for_exe(Path::new("/Applications/My AnyOS.app/Contents/MacOS/anyos")).unwrap();
        assert_eq!(l.dir, Path::new("/Applications"));
        assert_eq!(l.own_item, "My AnyOS.app");
        assert_eq!(l.own_source, "AnyOS.app");
        assert_eq!(
            l.relaunch_path("anyos-gui"),
            Path::new("/Applications/My AnyOS.app/Contents/MacOS/anyos-gui")
        );
    }

    #[test]
    fn swaps_own_binary_and_siblings_but_nothing_else() {
        let root = tmp("swap");
        let install = root.join("install");
        write(&install.join(format!("anyos{EXE}")), "old cli");
        write(&install.join(format!("anyos-gui{EXE}")), "old gui");
        write(&install.join("notes.txt"), "mine");
        let new = root.join("new");
        write(&new.join(format!("anyos{EXE}")), "new cli");
        write(&new.join(format!("anyos-gui{EXE}")), "new gui");
        write(&new.join("README.md"), "readme");

        let layout = Layout::for_exe(&install.join(format!("anyos{EXE}"))).unwrap();
        let done = swap_in(&layout, &new).unwrap();
        assert_eq!(done.len(), 2);
        let read = |n: &str| fs::read_to_string(install.join(n)).unwrap();
        assert_eq!(read(&format!("anyos{EXE}")), "new cli");
        assert_eq!(read(&format!("anyos-gui{EXE}")), "new gui");
        assert_eq!(read("notes.txt"), "mine");
        assert!(!install.join("README.md").exists());
        assert!(!install.join(format!("anyos{EXE}.old")).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swaps_a_renamed_bundle_as_a_whole() {
        let root = tmp("bundle");
        let apps = root.join("Applications");
        let exe = apps.join("Mine.app/Contents/MacOS/anyos-gui");
        write(&exe, "old gui");
        write(&apps.join("Mine.app/Contents/Resources/stale"), "stale");
        let new = root.join("new");
        write(&new.join("AnyOS.app/Contents/MacOS/anyos-gui"), "new gui");
        write(&new.join("AnyOS.app/Contents/MacOS/anyos"), "new cli");
        write(&new.join("anyos"), "new bare cli");

        let layout = Layout::for_exe(&exe).unwrap();
        swap_in(&layout, &new).unwrap();
        assert_eq!(fs::read_to_string(&exe).unwrap(), "new gui");
        assert!(apps.join("Mine.app/Contents/MacOS/anyos").exists());
        assert!(!apps.join("Mine.app/Contents/Resources/stale").exists());
        // No bare `anyos` was installed beside the bundle, so none is added.
        assert!(!apps.join("anyos").exists());
        assert!(!apps.join("Mine.app.old").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_own_item_in_release_changes_nothing() {
        let root = tmp("missing");
        let exe = root.join("install/anyos-tool");
        write(&exe, "old");
        let new = root.join("new");
        write(&new.join("anyos"), "new");
        let layout = Layout::for_exe(&exe).unwrap();
        assert!(matches!(swap_in(&layout, &new), Err(Error::BadArchive(_))));
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cleanup_removes_old_copies_and_stale_staging_only() {
        let root = tmp("cleanup");
        write(&root.join(format!("anyos{EXE}")), "cur");
        write(&root.join(format!("anyos{EXE}.old")), "old");
        write(&root.join("AnyOS.app.old/Contents/x"), "old");
        write(&root.join("other.old"), "not ours");
        write(&root.join(".anyos-update-1/x"), "stale");
        let layout = Layout::for_exe(&root.join(format!("anyos{EXE}"))).unwrap();
        cleanup_leftovers(&layout);
        assert!(root.join(format!("anyos{EXE}")).exists());
        assert!(!root.join(format!("anyos{EXE}.old")).exists());
        assert!(!root.join("AnyOS.app.old").exists());
        assert!(root.join("other.old").exists());
        assert!(!root.join(".anyos-update-1").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
