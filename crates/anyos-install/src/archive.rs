//! ZIP and 7-Zip archives (plain or self-extracting) installed as portable
//! apps: everything goes into `C:\Program Files\<name>`.
//!
//! When every entry sits under one top-level folder (`App-1.2/...`), that
//! folder becomes the app's name and is not repeated inside Program Files.

use std::io::Cursor;

use anyos_env::KnownFolder;

use crate::{InstallError, Installed, Product, Writer};

fn malformed(what: &'static str) -> impl FnOnce(String) -> InstallError {
    move |message| InstallError::Malformed { what, message }
}

fn components(name: &str) -> Vec<&str> {
    name.split(['/', '\\']).filter(|p| !p.is_empty()).collect()
}

/// The single top-level folder all entries share, if there is one.
fn common_top<'a>(entries: impl Iterator<Item = (&'a str, bool)>) -> Option<String> {
    let mut top: Option<&str> = None;
    let mut any_inside = false;
    for (name, is_dir) in entries {
        let parts = components(name);
        let first = *parts.first()?;
        if parts.len() == 1 && !is_dir {
            return None;
        }
        any_inside |= parts.len() > 1;
        match top {
            None => top = Some(first),
            Some(t) if t == first => {}
            Some(_) => return None,
        }
    }
    top.filter(|_| any_inside).map(String::from)
}

fn product(top: Option<String>, stem: &str) -> Product {
    Product {
        name: top.unwrap_or_else(|| stem.to_string()),
        ..Product::default()
    }
}

/// Entry path inside the install folder, with the shared top folder removed.
fn inner_path(name: &str, strip: bool) -> String {
    let parts = components(name);
    let parts = if strip { &parts[1..] } else { &parts[..] };
    parts.join("\\")
}

fn install_dir(w: &Writer, p: &Product) -> String {
    anyos_env::win_join(&w.env.known_folder(KnownFolder::ProgramFiles), &p.name)
}

fn zip_archive(payload: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, InstallError> {
    zip::ZipArchive::new(Cursor::new(payload)).map_err(|e| malformed("ZIP archive")(e.to_string()))
}

fn zip_names(zip: &zip::ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>, InstallError> {
    zip.file_names()
        .map(|n| n.map(|n| n.into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|e| malformed("ZIP archive")(e.to_string()))
}

fn zip_top(names: &[String]) -> Option<String> {
    common_top(names.iter().map(|n| (n.as_str(), n.ends_with('/'))))
}

pub(crate) fn zip_product(payload: &[u8], stem: &str) -> Result<Product, InstallError> {
    let zip = zip_archive(payload)?;
    Ok(product(zip_top(&zip_names(&zip)?), stem))
}

pub(crate) fn install_zip(
    w: &mut Writer,
    payload: &[u8],
    p: &Product,
) -> Result<Installed, InstallError> {
    let mut zip = zip_archive(payload)?;
    let strip = zip_top(&zip_names(&zip)?).is_some();
    let dir = install_dir(w, p);
    w.mkdir(&dir)?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| malformed("ZIP archive")(e.to_string()))?;
        let name = entry
            .name()
            .map_err(|e| malformed("ZIP archive")(e.to_string()))?
            .into_owned();
        let rel = inner_path(&name, strip);
        if rel.is_empty() {
            continue;
        }
        if entry.is_symlink() {
            w.note(format!("skipped symbolic link {name}"));
            continue;
        }
        if entry.is_dir() {
            match w.env.host_path_in(&dir, &rel) {
                Ok(_) => w.mkdir(&anyos_env::win_join(&dir, &rel))?,
                Err(_) => w.note(format!("skipped unsafe path {name}")),
            }
            continue;
        }
        match w.write(&dir, &rel, &mut entry) {
            Ok(()) => {}
            Err(InstallError::Env(_)) => w.note(format!("skipped unsafe path {name}")),
            Err(e) => return Err(e),
        }
    }
    Ok(Installed {
        install_dir: Some(dir),
        ..Installed::default()
    })
}

fn sevenz_reader(
    payload: &[u8],
) -> Result<sevenz_rust2::ArchiveReader<Cursor<&[u8]>>, InstallError> {
    sevenz_rust2::ArchiveReader::new(Cursor::new(payload), sevenz_rust2::Password::empty())
        .map_err(|e| malformed("7-Zip archive")(e.to_string()))
}

pub(crate) fn sevenz_product(payload: &[u8], stem: &str) -> Result<Product, InstallError> {
    let reader = sevenz_reader(payload)?;
    let top = common_top(
        reader
            .archive()
            .files
            .iter()
            .map(|e| (e.name(), e.is_directory())),
    );
    Ok(product(top, stem))
}

pub(crate) fn install_7z(
    w: &mut Writer,
    payload: &[u8],
    p: &Product,
) -> Result<Installed, InstallError> {
    let mut reader = sevenz_reader(payload)?;
    let strip = common_top(
        reader
            .archive()
            .files
            .iter()
            .map(|e| (e.name(), e.is_directory())),
    )
    .is_some();
    let dir = install_dir(w, p);
    w.mkdir(&dir)?;
    let mut failure = None;
    reader
        .for_each_entries(|entry, data| {
            let rel = inner_path(entry.name(), strip);
            if rel.is_empty() || entry.is_anti_item() {
                return Ok(true);
            }
            let result = if entry.is_directory() {
                match w.env.host_path_in(&dir, &rel) {
                    Ok(_) => w.mkdir(&anyos_env::win_join(&dir, &rel)),
                    Err(e) => Err(e.into()),
                }
            } else {
                w.write(&dir, &rel, data)
            };
            match result {
                Ok(()) => Ok(true),
                Err(InstallError::Env(_)) => {
                    w.note(format!("skipped unsafe path {}", entry.name()));
                    // The entry's data must still be consumed to keep going.
                    std::io::copy(data, &mut std::io::sink())?;
                    Ok(true)
                }
                Err(e) => {
                    failure = Some(e);
                    Ok(false)
                }
            }
        })
        .map_err(|e| malformed("7-Zip archive")(e.to_string()))?;
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(Installed {
        install_dir: Some(dir),
        ..Installed::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_folder_detection() {
        let e = |v: &[(&'static str, bool)]| common_top(v.iter().copied());
        assert_eq!(
            e(&[("App/", true), ("App/a.exe", false), ("App/x/y", false)]),
            Some("App".into())
        );
        assert_eq!(e(&[("App/a.exe", false), ("b.txt", false)]), None);
        assert_eq!(e(&[("a.exe", false)]), None);
        assert_eq!(e(&[("Empty/", true)]), None);
        assert_eq!(inner_path("App/x/y.dll", true), r"x\y.dll");
    }
}
