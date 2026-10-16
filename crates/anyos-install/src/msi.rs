//! Windows Installer packages, installed by reading their database directly.
//!
//! What is applied: the `Directory`, `Component` and `File` tables (files come
//! out of the cabinets listed in `Media`, embedded or beside the `.msi`),
//! `CreateFolder`, `Registry` and `Shortcut`, plus an Add/Remove Programs
//! entry. Every feature and component is installed; conditions, custom
//! actions and other tables are not evaluated, and the app's notes say so.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use anyos_env::{KnownFolder, RegValue, Shortcut};
use msi::{Package, Select, Value};

use crate::{InstallError, Installed, Product, Writer};

type Pkg<'a> = Package<Cursor<&'a [u8]>>;
type Row = HashMap<String, Value>;

const FILE_NONCOMPRESSED: i32 = 0x2000;
const FILE_COMPRESSED: i32 = 0x4000;
/// Word count bit in the summary information: files are compressed by default.
const WORDCOUNT_COMPRESSED: i32 = 0x2;

fn malformed(message: impl ToString) -> InstallError {
    InstallError::Malformed {
        what: "MSI package",
        message: message.to_string(),
    }
}

fn open(bytes: &[u8]) -> Result<Pkg<'_>, InstallError> {
    Package::open(Cursor::new(bytes)).map_err(malformed)
}

fn table(pkg: &mut Pkg, name: &str) -> Result<Vec<Row>, InstallError> {
    let Some(columns) = pkg.get_table(name).map(|t| {
        t.columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect::<Vec<_>>()
    }) else {
        return Ok(Vec::new());
    };
    let rows = pkg.select_rows(Select::table(name)).map_err(malformed)?;
    Ok(rows
        .map(|r| {
            columns
                .iter()
                .enumerate()
                .map(|(i, c)| (c.clone(), r[i].clone()))
                .collect()
        })
        .collect())
}

fn s(row: &Row, col: &str) -> Option<String> {
    row.get(col).and_then(|v| v.as_str()).map(String::from)
}

fn int(row: &Row, col: &str) -> Option<i32> {
    row.get(col).and_then(|v| v.as_int())
}

/// Long name from an MSI `short|long` filename, or the name itself.
fn long_name(name: &str) -> &str {
    name.rsplit_once('|').map(|(_, l)| l).unwrap_or(name)
}

fn properties(pkg: &mut Pkg) -> Result<HashMap<String, String>, InstallError> {
    Ok(table(pkg, "Property")?
        .iter()
        .filter_map(|r| Some((s(r, "Property")?, s(r, "Value").unwrap_or_default())))
        .collect())
}

pub(crate) fn product(bytes: &[u8]) -> Result<Product, InstallError> {
    let mut pkg = open(bytes)?;
    let props = properties(&mut pkg)?;
    let name = props
        .get("ProductName")
        .cloned()
        .or_else(|| pkg.summary_info().subject().map(String::from))
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| malformed("no ProductName"))?;
    Ok(Product {
        name,
        version: props.get("ProductVersion").cloned(),
        publisher: props.get("Manufacturer").cloned(),
    })
}

/// Resolved paths and the bits needed to expand MSI formatted strings.
struct Ctx {
    props: HashMap<String, String>,
    /// Directory key -> Windows path (no trailing backslash, except `C:\`).
    dirs: HashMap<String, String>,
    /// File key -> Windows path.
    files: HashMap<String, String>,
    /// Component key -> Windows directory.
    components: HashMap<String, String>,
}

impl Ctx {
    /// Expand an MSI formatted string: `[Property]`, `[DirKey]`, `[#FileKey]`,
    /// `[!FileKey]`, `[$ComponentKey]`, `[%ENV]` (empty), `[\x]` (literal x)
    /// and `[~]` (a NUL, used by multi-string values).
    fn format(&self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(open) = rest.find('[') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find(']') else {
                out.push_str(&rest[open..]);
                return out;
            };
            let inner = &after[..close];
            rest = &after[close + 1..];
            let with_slash = |p: &String| {
                if p.ends_with('\\') {
                    p.clone()
                } else {
                    format!("{p}\\")
                }
            };
            let value = if let Some(c) = inner.strip_prefix('\\') {
                c.to_string()
            } else if inner == "~" {
                "\0".to_string()
            } else if let Some(k) = inner.strip_prefix('#').or_else(|| inner.strip_prefix('!')) {
                self.files.get(k).cloned().unwrap_or_default()
            } else if let Some(k) = inner.strip_prefix('$') {
                self.components.get(k).map(with_slash).unwrap_or_default()
            } else if inner.starts_with('%') {
                String::new()
            } else if let Some(d) = self.dirs.get(inner) {
                with_slash(d)
            } else {
                self.props.get(inner).cloned().unwrap_or_default()
            };
            out.push_str(&value);
        }
        out.push_str(rest);
        out
    }
}

/// Where Windows puts a standard MSI folder property (64-bit Windows rules).
fn standard_dir(w: &Writer, key: &str, per_machine: bool) -> Option<String> {
    use KnownFolder::*;
    let f = match key {
        "TARGETDIR" | "ROOTDRIVE" | "WindowsVolume" => Drive,
        "ProgramFilesFolder" => ProgramFilesX86,
        "ProgramFiles64Folder" => ProgramFiles,
        "CommonFilesFolder" => CommonFilesX86,
        "CommonFiles64Folder" => CommonFiles,
        "CommonAppDataFolder" => ProgramData,
        "AppDataFolder" => AppData,
        "LocalAppDataFolder" => LocalAppData,
        "PersonalFolder" => Documents,
        "DesktopFolder" if per_machine => CommonDesktop,
        "DesktopFolder" => Desktop,
        "ProgramMenuFolder" if per_machine => CommonPrograms,
        "ProgramMenuFolder" => Programs,
        "StartMenuFolder" if per_machine => CommonStartMenu,
        "StartMenuFolder" => StartMenu,
        "StartupFolder" => Startup,
        "WindowsFolder" => Windows,
        "SystemFolder" => SysWow64,
        "System64Folder" => System32,
        "FontsFolder" => Fonts,
        "TempFolder" => Temp,
        _ => return None,
    };
    Some(w.env.known_folder(f))
}

fn resolve_dirs(
    w: &Writer,
    rows: &[Row],
    per_machine: bool,
) -> Result<HashMap<String, String>, InstallError> {
    let defs: HashMap<String, (Option<String>, String)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                s(r, "Directory")?,
                (
                    s(r, "Directory_Parent"),
                    s(r, "DefaultDir").unwrap_or_default(),
                ),
            ))
        })
        .collect();
    let mut out: HashMap<String, String> = HashMap::new();
    for key in defs.keys() {
        let mut chain = vec![key.clone()];
        // Walk up to a root or a standard folder, then build back down.
        let base = loop {
            let cur = chain.last().unwrap().clone();
            if let Some(p) = out.get(&cur) {
                chain.pop();
                break p.clone();
            }
            if let Some(p) = standard_dir(w, &cur, per_machine) {
                chain.pop();
                out.insert(cur, p.clone());
                break p;
            }
            match defs.get(&cur).and_then(|(parent, _)| parent.clone()) {
                Some(parent) if parent != cur && !chain.contains(&parent) => chain.push(parent),
                Some(parent) if parent != cur => {
                    return Err(malformed(format!("Directory table loops at {parent}")))
                }
                // A root that is not TARGETDIR: put it at C:\ as well.
                _ => {
                    chain.pop();
                    let drive = w.env.known_folder(KnownFolder::Drive);
                    out.insert(cur, drive.clone());
                    break drive;
                }
            }
        };
        let mut path = base;
        while let Some(k) = chain.pop() {
            let default_dir = defs.get(&k).map(|d| d.1.as_str()).unwrap_or(".");
            let target = default_dir.split(':').next().unwrap_or(".");
            let name = long_name(target);
            if name != "." && !name.is_empty() {
                path = anyos_env::win_join(&path, name);
            }
            out.insert(k, path.clone());
        }
    }
    Ok(out)
}

pub(crate) fn install(
    w: &mut Writer,
    msi_path: &Path,
    bytes: &[u8],
    product: &Product,
) -> Result<Installed, InstallError> {
    let mut pkg = open(bytes)?;
    let props = properties(&mut pkg)?;
    let per_machine = props.get("ALLUSERS").is_some_and(|v| !v.trim().is_empty());
    let compressed_default =
        pkg.summary_info().word_count().unwrap_or(0) & WORDCOUNT_COMPRESSED != 0;

    let dirs = resolve_dirs(w, &table(&mut pkg, "Directory")?, per_machine)?;
    let mut components = HashMap::new();
    let mut key_paths = HashMap::new();
    for r in table(&mut pkg, "Component")? {
        let (Some(c), Some(d)) = (s(&r, "Component"), s(&r, "Directory_")) else {
            continue;
        };
        if let Some(path) = dirs.get(&d) {
            components.insert(c.clone(), path.clone());
        }
        if let Some(k) = s(&r, "KeyPath") {
            key_paths.insert(c, k);
        }
    }

    struct FileRow {
        key: String,
        dir: String,
        name: String,
        sequence: i32,
        attributes: i32,
    }
    let mut files = Vec::new();
    let mut file_paths = HashMap::new();
    for r in table(&mut pkg, "File")? {
        let (Some(key), Some(comp), Some(fname)) =
            (s(&r, "File"), s(&r, "Component_"), s(&r, "FileName"))
        else {
            continue;
        };
        let Some(dir) = components.get(&comp) else {
            w.note(format!(
                "skipped file {key}: component {comp} has no folder"
            ));
            continue;
        };
        let name = long_name(&fname).to_string();
        file_paths.insert(key.clone(), anyos_env::win_join(dir, &name));
        files.push(FileRow {
            key,
            dir: dir.clone(),
            name,
            sequence: int(&r, "Sequence").unwrap_or(0),
            attributes: int(&r, "Attributes").unwrap_or(0),
        });
    }

    let ctx = Ctx {
        props,
        dirs,
        files: file_paths,
        components,
    };

    // Files come out of each cabinet in sequence order.
    let mut media: Vec<(i32, Option<String>)> = table(&mut pkg, "Media")?
        .iter()
        .map(|r| (int(r, "LastSequence").unwrap_or(0), s(r, "Cabinet")))
        .collect();
    media.sort_by_key(|m| m.0);
    files.sort_by_key(|f| f.sequence);
    let mut by_cab: Vec<(String, Vec<&FileRow>)> = Vec::new();
    let mut loose = 0;
    for f in &files {
        let compressed = if f.attributes & FILE_NONCOMPRESSED != 0 {
            false
        } else {
            f.attributes & FILE_COMPRESSED != 0 || compressed_default
        };
        let cab = media
            .iter()
            .find(|m| m.0 >= f.sequence)
            .and_then(|m| m.1.clone());
        match cab {
            Some(c) if compressed => match by_cab.iter_mut().find(|(n, _)| *n == c) {
                Some((_, list)) => list.push(f),
                None => by_cab.push((c, vec![f])),
            },
            _ => loose += 1,
        }
    }
    if loose > 0 {
        w.note(format!(
            "skipped {loose} uncompressed file(s) stored beside the package (not supported yet)"
        ));
    }

    for (cab_name, list) in &by_cab {
        let cab_bytes = match cab_name.strip_prefix('#') {
            Some(stream) => {
                let mut buf = Vec::new();
                pkg.read_stream(stream)
                    .and_then(|mut r| r.read_to_end(&mut buf))
                    .map_err(|e| malformed(format!("cabinet {stream}: {e}")))?;
                buf
            }
            None => {
                let beside = msi_path.with_file_name(cab_name);
                match std::fs::read(&beside) {
                    Ok(b) => b,
                    Err(_) => {
                        w.note(format!(
                            "skipped {} file(s): cabinet {cab_name} not found beside the package",
                            list.len()
                        ));
                        continue;
                    }
                }
            }
        };
        let mut cab = cab::Cabinet::new(Cursor::new(cab_bytes))
            .map_err(|e| malformed(format!("cabinet {cab_name}: {e}")))?;
        for f in list {
            let mut reader = match cab.read_file(&f.key) {
                Ok(r) => r,
                Err(_) => {
                    w.note(format!("skipped {}: not in cabinet {cab_name}", f.name));
                    continue;
                }
            };
            match w.write(&f.dir, &f.name, &mut reader) {
                Ok(()) => {}
                Err(InstallError::Env(_)) => w.note(format!("skipped unsafe path {}", f.name)),
                Err(e) => return Err(e),
            }
        }
    }

    for r in table(&mut pkg, "CreateFolder")? {
        if let Some(d) = s(&r, "Directory_").and_then(|d| ctx.dirs.get(&d).cloned()) {
            w.mkdir(&d)?;
        }
    }

    let mut installed = Installed::default();
    registry(&mut pkg, &ctx, per_machine, &mut installed)?;
    shortcuts(&mut pkg, &ctx, &key_paths, &mut installed)?;

    // Program folder: where the main program lives, else the folder holding
    // most of the files.
    installed.main = installed
        .shortcuts
        .iter()
        .map(|s| s.target.clone())
        .find(|t| t.to_ascii_lowercase().ends_with(".exe"))
        .or_else(|| crate::guess_main(&w.files, &product.name));
    installed.install_dir = installed
        .main
        .as_ref()
        .and_then(|m| m.rsplit_once('\\').map(|(d, _)| d.to_string()))
        .or_else(|| busiest_dir(&w.files));

    // Add/Remove Programs entry, which some apps and updaters look for.
    if let Some(code) = ctx.props.get("ProductCode") {
        let root = if per_machine { "HKLM" } else { "HKCU" };
        let key = format!(r"{root}\Software\Microsoft\Windows\CurrentVersion\Uninstall\{code}");
        let mut put = |name: &str, v: Option<&String>| {
            if let Some(v) = v {
                installed
                    .registry
                    .push((key.clone(), name.into(), RegValue::Sz(v.clone())));
            }
        };
        put("DisplayName", Some(&product.name));
        put("DisplayVersion", product.version.as_ref());
        put("Publisher", product.publisher.as_ref());
        put("InstallLocation", installed.install_dir.as_ref());
    }

    let actions = table(&mut pkg, "CustomAction")?.len();
    if actions > 0 {
        w.note(format!(
            "{actions} custom action(s) in the package were not run"
        ));
    }
    let conditions =
        table(&mut pkg, "Condition")?.len() + table(&mut pkg, "LaunchCondition")?.len();
    if conditions > 0 {
        w.note("install conditions were not checked; every component was installed");
    }
    for t in [
        "ServiceInstall",
        "IniFile",
        "Environment",
        "Font",
        "TypeLib",
        "Class",
    ] {
        if !table(&mut pkg, t)?.is_empty() {
            w.note(format!("the {t} table was not applied"));
        }
    }
    Ok(installed)
}

fn busiest_dir(files: &[String]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        if let Some((d, _)) = f.rsplit_once('\\') {
            *counts.entry(d).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(d, n)| (*n, std::cmp::Reverse(d.len())))
        .map(|(d, _)| d.to_string())
}

fn registry(
    pkg: &mut Pkg,
    ctx: &Ctx,
    per_machine: bool,
    out: &mut Installed,
) -> Result<(), InstallError> {
    for r in table(pkg, "Registry")? {
        let root = match int(&r, "Root") {
            Some(-1) if per_machine => "HKLM",
            Some(-1) => "HKCU",
            Some(0) => "HKCR",
            Some(1) => "HKCU",
            Some(2) => "HKLM",
            Some(3) => "HKU",
            _ => continue,
        };
        let Some(key) = s(&r, "Key") else { continue };
        let key = format!("{root}\\{}", ctx.format(&key));
        let name = s(&r, "Name").map(|n| ctx.format(&n));
        let value = s(&r, "Value");
        match (name.as_deref(), value) {
            // "+", "*" and "-" with no value only create or remove keys.
            (Some("-"), None) => {}
            (_, None) => out.keys.push(key),
            (name, Some(v)) => {
                let name = name.unwrap_or("").to_string();
                out.registry.push((key, name, parse_value(&ctx.format(&v))));
            }
        }
    }
    Ok(())
}

/// Registry value from an MSI `Registry.Value` after formatting.
fn parse_value(v: &str) -> RegValue {
    if let Some(hex) = v.strip_prefix("#x").or_else(|| v.strip_prefix("#X")) {
        let bytes = (0..hex.len() / 2)
            .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
            .collect();
        return RegValue::Binary(bytes);
    }
    if let Some(rest) = v.strip_prefix("#%") {
        return RegValue::ExpandSz(rest.to_string());
    }
    if let Some(rest) = v.strip_prefix("##") {
        return RegValue::Sz(format!("#{rest}"));
    }
    if let Some(num) = v.strip_prefix('#') {
        let num = num.trim_start_matches('+');
        return match num.parse::<i64>() {
            Ok(n) => RegValue::Dword(n as u32),
            Err(_) => RegValue::Sz(v.to_string()),
        };
    }
    if v.contains('\0') {
        return RegValue::MultiSz(
            v.split('\0')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect(),
        );
    }
    RegValue::Sz(v.to_string())
}

fn shortcuts(
    pkg: &mut Pkg,
    ctx: &Ctx,
    key_paths: &HashMap<String, String>,
    out: &mut Installed,
) -> Result<(), InstallError> {
    for r in table(pkg, "Shortcut")? {
        let (Some(dir), Some(name), Some(target)) =
            (s(&r, "Directory_"), s(&r, "Name"), s(&r, "Target"))
        else {
            continue;
        };
        let Some(folder) = ctx.dirs.get(&dir).cloned() else {
            continue;
        };
        // A bracketed target is a path; anything else names a feature, and the
        // shortcut opens its component's key file ("advertised").
        let target = if target.starts_with('[') {
            ctx.format(&target)
        } else {
            match s(&r, "Component_")
                .and_then(|c| key_paths.get(&c))
                .and_then(|k| ctx.files.get(k))
            {
                Some(t) => t.clone(),
                None => continue,
            }
        };
        out.shortcuts.push(Shortcut {
            name: long_name(&name).to_string(),
            folder,
            target,
            arguments: s(&r, "Arguments").map(|a| ctx.format(&a)),
            working_dir: s(&r, "WkDir").and_then(|d| ctx.dirs.get(&d).cloned()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_values() {
        assert_eq!(parse_value("#42"), RegValue::Dword(42));
        assert_eq!(parse_value("#-1"), RegValue::Dword(u32::MAX));
        assert_eq!(parse_value("#x01ff"), RegValue::Binary(vec![1, 255]));
        assert_eq!(parse_value("#%%PATH%"), RegValue::ExpandSz("%PATH%".into()));
        assert_eq!(parse_value("##x"), RegValue::Sz("#x".into()));
        assert_eq!(
            parse_value("a\0b\0"),
            RegValue::MultiSz(vec!["a".into(), "b".into()])
        );
        assert_eq!(parse_value("plain"), RegValue::Sz("plain".into()));
    }

    #[test]
    fn formatted_strings() {
        let ctx = Ctx {
            props: [("ProductName".to_string(), "Tool".to_string())].into(),
            dirs: [(
                "INSTALLDIR".to_string(),
                r"C:\Program Files\Tool".to_string(),
            )]
            .into(),
            files: [(
                "tool.exe".to_string(),
                r"C:\Program Files\Tool\tool.exe".to_string(),
            )]
            .into(),
            components: HashMap::new(),
        };
        assert_eq!(
            ctx.format(r#""[#tool.exe]" --name [ProductName] [INSTALLDIR]x [\[] [%TEMP][Nope]"#),
            r#""C:\Program Files\Tool\tool.exe" --name Tool C:\Program Files\Tool\x [ "#
        );
        assert_eq!(ctx.format("open [bracket"), "open [bracket");
    }
}
