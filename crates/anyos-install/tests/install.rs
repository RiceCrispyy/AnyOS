//! End-to-end installs of packages built here in memory: an MSI with an
//! embedded cabinet, ZIP and 7-Zip archives, and a ZIP self-extractor. Each
//! test also checks that hostile paths stay inside the environment.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use anyos_env::{Env, RegValue};
use anyos_install::{install, uninstall, InstallError};
use msi::{Column, Insert, Package, PackageType, Value};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("anyos-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn env(&self) -> Env {
        Env::open_or_create(&self.0.join("home"), "test").unwrap()
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(env: &Env, win: &str) -> Vec<u8> {
    std::fs::read(env.host_path(win).unwrap()).unwrap_or_else(|e| panic!("{win}: {e}"))
}

/// Every regular file under `dir`.
fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(all_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

const TOOL_EXE: &[u8] = b"MZ pretend program";
const README: &[u8] = b"read me\r\n";

fn cabinet(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = cab::CabinetBuilder::new();
    {
        let folder = builder.add_folder(cab::CompressionType::MsZip);
        for (name, _) in files {
            folder.add_file(*name);
        }
    }
    let mut writer = builder.build(Cursor::new(Vec::new())).unwrap();
    let mut i = 0;
    while let Some(mut f) = writer.next_file().unwrap() {
        f.write_all(files[i].1).unwrap();
        i += 1;
    }
    writer.finish().unwrap().into_inner()
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}

fn sample_msi() -> Vec<u8> {
    let cab = cabinet(&[
        ("tool_exe", TOOL_EXE),
        ("readme", README),
        ("evil", b"escaped"),
    ]);
    let mut pkg = Package::create(PackageType::Installer, Cursor::new(Vec::new())).unwrap();
    pkg.summary_info_mut().set_word_count(2);
    let id = |n: &str| Column::build(n).id_string(72);
    let pk = |n: &str| Column::build(n).primary_key().id_string(72);
    let text = |n: &str| Column::build(n).nullable().text_string(255);

    pkg.create_table("Property", vec![pk("Property"), text("Value")])
        .unwrap();
    pkg.insert_rows(Insert::into("Property").rows(vec![
        vec![s("ProductName"), s("Sample Tool")],
        vec![s("ProductVersion"), s("1.2.3")],
        vec![s("Manufacturer"), s("AnyOS Tests")],
        vec![
            s("ProductCode"),
            s("{11111111-2222-3333-4444-555555555555}"),
        ],
        vec![s("ALLUSERS"), s("1")],
    ]))
    .unwrap();

    pkg.create_table(
        "Directory",
        vec![
            pk("Directory"),
            Column::build("Directory_Parent").nullable().id_string(72),
            Column::build("DefaultDir").text_string(255),
        ],
    )
    .unwrap();
    let dir = |k: &str, parent: Option<&str>, d: &str| {
        vec![s(k), parent.map(s).unwrap_or(Value::Null), s(d)]
    };
    pkg.insert_rows(Insert::into("Directory").rows(vec![
        dir("TARGETDIR", None, "SourceDir"),
        dir("ProgramFiles64Folder", Some("TARGETDIR"), "."),
        dir(
            "VendorDir",
            Some("ProgramFiles64Folder"),
            "VENDOR~1|Sample Vendor",
        ),
        dir("INSTALLDIR", Some("VendorDir"), "Tool:src"),
        dir("DocDir", Some("INSTALLDIR"), "docs"),
        dir("ProgramMenuFolder", Some("TARGETDIR"), "."),
        dir("EmptyDir", Some("INSTALLDIR"), "logs"),
    ]))
    .unwrap();

    pkg.create_table(
        "Component",
        vec![
            pk("Component"),
            Column::build("ComponentId").nullable().string(38),
            id("Directory_"),
            Column::build("Attributes").int16(),
            Column::build("Condition").nullable().text_string(255),
            Column::build("KeyPath").nullable().id_string(72),
        ],
    )
    .unwrap();
    let comp = |c: &str, d: &str, key: &str| {
        vec![s(c), Value::Null, s(d), Value::Int(0), Value::Null, s(key)]
    };
    pkg.insert_rows(Insert::into("Component").rows(vec![
        comp("Main", "INSTALLDIR", "tool_exe"),
        comp("Docs", "DocDir", "readme"),
        comp("Evil", "INSTALLDIR", "evil"),
    ]))
    .unwrap();

    pkg.create_table(
        "File",
        vec![
            pk("File"),
            id("Component_"),
            Column::build("FileName").text_string(255),
            Column::build("FileSize").int32(),
            Column::build("Version").nullable().string(72),
            Column::build("Language").nullable().string(20),
            Column::build("Attributes").nullable().int16(),
            Column::build("Sequence").int16(),
        ],
    )
    .unwrap();
    let file = |k: &str, c: &str, n: &str, size: usize, seq: i32| {
        vec![
            s(k),
            s(c),
            s(n),
            Value::Int(size as i32),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Int(seq),
        ]
    };
    pkg.insert_rows(Insert::into("File").rows(vec![
        file("tool_exe", "Main", "tool.exe", TOOL_EXE.len(), 1),
        file(
            "readme",
            "Docs",
            "README~1.TXT|Read Me.txt",
            README.len(),
            2,
        ),
        // A hostile name that tries to leave the environment.
        file("evil", "Evil", r"..\..\..\..\..\escaped.txt", 7, 3),
    ]))
    .unwrap();

    pkg.create_table(
        "Media",
        vec![
            Column::build("DiskId").primary_key().int16(),
            Column::build("LastSequence").int16(),
            text("DiskPrompt"),
            Column::build("Cabinet").nullable().string(255),
            text("VolumeLabel"),
            text("Source"),
        ],
    )
    .unwrap();
    pkg.insert_rows(Insert::into("Media").row(vec![
        Value::Int(1),
        Value::Int(3),
        Value::Null,
        s("#data.cab"),
        Value::Null,
        Value::Null,
    ]))
    .unwrap();

    pkg.create_table("CreateFolder", vec![pk("Directory_"), pk("Component_")])
        .unwrap();
    pkg.insert_rows(Insert::into("CreateFolder").row(vec![s("EmptyDir"), s("Main")]))
        .unwrap();

    pkg.create_table(
        "Registry",
        vec![
            pk("Registry"),
            Column::build("Root").int16(),
            Column::build("Key").formatted_string(255),
            Column::build("Name").nullable().formatted_string(255),
            Column::build("Value").nullable().formatted_string(0),
            id("Component_"),
        ],
    )
    .unwrap();
    let reg = |k: &str, root: i32, key: &str, name: Option<&str>, value: Option<&str>| {
        vec![
            s(k),
            Value::Int(root),
            s(key),
            name.map(s).unwrap_or(Value::Null),
            value.map(s).unwrap_or(Value::Null),
            s("Main"),
        ]
    };
    pkg.insert_rows(Insert::into("Registry").rows(vec![
        reg(
            "r1",
            2,
            r"Software\[Manufacturer]\Tool",
            Some("InstallPath"),
            Some("[INSTALLDIR]"),
        ),
        reg(
            "r2",
            2,
            r"Software\[Manufacturer]\Tool",
            Some("Count"),
            Some("#7"),
        ),
        reg(
            "r3",
            -1,
            r"Software\[Manufacturer]\Tool\Settings",
            None,
            None,
        ),
        reg(
            "r4",
            0,
            r".sample\shell\open\command",
            None,
            Some(r#""[#tool_exe]" "%1""#),
        ),
    ]))
    .unwrap();

    pkg.create_table(
        "Shortcut",
        vec![
            pk("Shortcut"),
            id("Directory_"),
            Column::build("Name").text_string(128),
            id("Component_"),
            Column::build("Target").string(72),
            Column::build("Arguments").nullable().formatted_string(255),
            text("Description"),
            Column::build("Hotkey").nullable().int16(),
            Column::build("Icon_").nullable().id_string(72),
            Column::build("IconIndex").nullable().int16(),
            Column::build("ShowCmd").nullable().int16(),
            Column::build("WkDir").nullable().id_string(72),
        ],
    )
    .unwrap();
    pkg.insert_rows(Insert::into("Shortcut").row(vec![
        s("menu"),
        s("ProgramMenuFolder"),
        s("SAMPLE~1|Sample Tool"),
        s("Main"),
        // A feature name: an advertised shortcut to the component's key file.
        s("Complete"),
        s("--start [ProductVersion]"),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        s("INSTALLDIR"),
    ]))
    .unwrap();

    {
        let mut stream = pkg.write_stream("data.cab").unwrap();
        stream.write_all(&cab).unwrap();
    }
    pkg.flush().unwrap();
    pkg.into_inner().unwrap().into_inner()
}

#[test]
fn msi_installs_files_registry_and_shortcuts_then_uninstalls() {
    let scratch = Scratch::new("msi");
    let env = scratch.env();
    let msi = scratch.file("sample.msi", &sample_msi());

    let r = install(&env, &msi).unwrap();
    let dir = r"C:\Program Files\Sample Vendor\Tool";
    assert_eq!(r.app.name, "Sample Tool");
    assert_eq!(r.app.id, "sample-tool");
    assert_eq!(r.app.version.as_deref(), Some("1.2.3"));
    assert_eq!(r.app.publisher.as_deref(), Some("AnyOS Tests"));
    assert_eq!(r.app.install_dir.as_deref(), Some(dir));
    assert_eq!(
        r.app.main.as_deref(),
        Some(r"C:\Program Files\Sample Vendor\Tool\tool.exe")
    );
    assert_eq!(read(&env, &format!(r"{dir}\tool.exe")), TOOL_EXE);
    assert_eq!(read(&env, &format!(r"{dir}\docs\Read Me.txt")), README);
    assert!(env.host_path(&format!(r"{dir}\logs")).unwrap().is_dir());
    assert_eq!(r.app.files.len(), 2);

    // The hostile file was refused and nothing escaped the environment.
    assert!(
        r.app.notes.iter().any(|n| n.contains("unsafe path")),
        "{:?}",
        r.app.notes
    );
    for f in all_files(&scratch.0) {
        assert!(f.starts_with(env.root()) || f == msi, "{}", f.display());
    }

    let shortcut = &r.app.shortcuts[0];
    assert_eq!(shortcut.name, "Sample Tool");
    assert_eq!(
        shortcut.folder,
        r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs"
    );
    assert_eq!(shortcut.target, format!(r"{dir}\tool.exe"));
    assert_eq!(shortcut.arguments.as_deref(), Some("--start 1.2.3"));
    assert_eq!(shortcut.working_dir.as_deref(), Some(dir));

    let reg = env.registry().unwrap();
    assert_eq!(
        reg.get(r"HKLM\Software\AnyOS Tests\Tool", "InstallPath"),
        Some(&RegValue::Sz(format!(r"{dir}\")))
    );
    assert_eq!(
        reg.get(r"HKLM\Software\AnyOS Tests\Tool", "Count"),
        Some(&RegValue::Dword(7))
    );
    assert!(reg.key_exists(r"HKLM\Software\AnyOS Tests\Tool\Settings"));
    assert_eq!(
        reg.get(r"HKCR\.sample\shell\open\command", ""),
        Some(&RegValue::Sz(format!(r#""{dir}\tool.exe" "%1""#)))
    );
    let arp = r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\{11111111-2222-3333-4444-555555555555}";
    assert_eq!(
        reg.get(arp, "DisplayName"),
        Some(&RegValue::Sz("Sample Tool".into()))
    );

    // Installing again is refused rather than overwriting.
    assert!(matches!(
        install(&env, &msi),
        Err(InstallError::AlreadyInstalled(_))
    ));

    let u = uninstall(&env, "Sample Tool").unwrap();
    assert_eq!(u.files_removed, 2);
    assert!(!env.host_path(dir).unwrap().exists());
    assert!(env.host_path(r"C:\Program Files").unwrap().is_dir());
    assert!(env.registry().unwrap().is_empty());
    assert!(env.apps().unwrap().apps.is_empty());
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

const GAME: &[(&str, &[u8])] = &[
    ("Game-1.0/unins000.exe", b"MZ uninstaller"),
    ("Game-1.0/game.exe", b"MZ game"),
    ("Game-1.0/data/level1.dat", b"level"),
];

#[test]
fn zip_archive_installs_as_portable_app() {
    let scratch = Scratch::new("zip");
    let env = scratch.env();
    let zip = scratch.file("game.zip", &zip_bytes(GAME));
    let r = install(&env, &zip).unwrap();
    assert_eq!(r.app.name, "Game-1.0");
    assert_eq!(
        r.app.install_dir.as_deref(),
        Some(r"C:\Program Files\Game-1.0")
    );
    assert_eq!(
        r.app.main.as_deref(),
        Some(r"C:\Program Files\Game-1.0\game.exe")
    );
    assert_eq!(
        read(&env, r"C:\Program Files\Game-1.0\data\level1.dat"),
        b"level"
    );

    // The index remembers when and how much was installed, and Windows'
    // Uninstall key lists the app for programs that look there.
    let before = anyos_env::clock::unix_now();
    let app = env.apps().unwrap().find("game-1-0").cloned().unwrap();
    let at = anyos_env::clock::parse_rfc3339(app.installed_at.as_deref().unwrap()).unwrap();
    assert!((before - at).abs() < 300, "{:?}", app.installed_at);
    assert_eq!(app.size_bytes, Some(r.bytes_written));
    let key = r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\AnyOS-game-1-0";
    let reg = env.registry().unwrap();
    assert_eq!(
        reg.get(key, "DisplayName"),
        Some(&RegValue::Sz("Game-1.0".into()))
    );
    assert!(matches!(reg.get(key, "InstallDate"), Some(RegValue::Sz(d)) if d.len() == 8));

    uninstall(&env, "game-1-0").unwrap();
    assert!(!env.registry().unwrap().key_exists(key));
    assert!(!env
        .host_path(r"C:\Program Files\Game-1.0")
        .unwrap()
        .exists());
}

#[test]
fn zip_without_top_folder_uses_file_name_and_refuses_escapes() {
    let scratch = Scratch::new("zipflat");
    let env = scratch.env();
    let zip = scratch.file(
        "Tool.zip",
        &zip_bytes(&[("tool.exe", b"MZ"), ("../../../../escaped.txt", b"x")]),
    );
    let r = install(&env, &zip).unwrap();
    assert_eq!(r.app.name, "Tool");
    assert_eq!(read(&env, r"C:\Program Files\Tool\tool.exe"), b"MZ");
    assert!(r.app.notes.iter().any(|n| n.contains("unsafe path")));
    for f in all_files(&scratch.0) {
        assert!(f.starts_with(env.root()) || f == zip, "{}", f.display());
    }
}

#[test]
fn zip_self_extractor_installs() {
    let scratch = Scratch::new("sfx");
    let env = scratch.env();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin/x64/hello_console.exe");
    let mut exe = std::fs::read(fixture).unwrap();
    exe.extend_from_slice(&zip_bytes(GAME));
    let sfx = scratch.file("game-setup.exe", &exe);
    let r = install(&env, &sfx).unwrap();
    assert_eq!(r.app.package, "ZIP self-extractor");
    assert_eq!(
        read(&env, r"C:\Program Files\Game-1.0\game.exe"),
        b"MZ game"
    );
}

#[test]
fn sevenz_archive_installs() {
    let scratch = Scratch::new("7z");
    let env = scratch.env();
    let mut w = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    for (name, data) in GAME {
        w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(*data))
            .unwrap();
    }
    let bytes = w.finish().unwrap().into_inner();
    let archive = scratch.file("game.7z", &bytes);
    let r = install(&env, &archive).unwrap();
    assert_eq!(r.app.name, "Game-1.0");
    assert_eq!(
        read(&env, r"C:\Program Files\Game-1.0\data\level1.dat"),
        b"level"
    );
    assert_eq!(
        r.app.main.as_deref(),
        Some(r"C:\Program Files\Game-1.0\game.exe")
    );
}

#[test]
fn unsupported_and_unknown_files_are_refused() {
    let scratch = Scratch::new("refuse");
    let env = scratch.env();
    let deb = scratch.file("tool.deb", b"!<arch>\ndebian-binary   0");
    assert!(matches!(
        install(&env, &deb),
        Err(InstallError::Malformed { .. })
    ));
    let text = scratch.file("notes.txt", b"hello");
    assert!(matches!(
        install(&env, &text),
        Err(InstallError::NotAPackage(_))
    ));
    assert!(env.apps().unwrap().apps.is_empty());
}

#[test]
fn nsis_installs_files_registry_and_shortcuts() {
    let scratch = Scratch::new("nsis");
    let env = scratch.env();

    // Build a minimal NSIS installer: real PE stub + NSIS header + data.
    // Format: non-solid stored (no compression; simpler to construct).
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/bin/x64/hello_console.exe");
    let mut exe = std::fs::read(fixture).unwrap();

    // String table (ANSI3 format, plain NUL-terminated strings at these offsets).
    const STR_EMPTY: u32 = 0; // ""
    const STR_INSTDIR: u32 = 1; // "C:\Program Files\TestApp"
    const STR_TOOL: u32 = 26; // "tool.exe"
    const STR_REG_KEY: u32 = 35; // "Software\...\Uninstall\TestApp"
    const STR_DISP_NAME: u32 = 95; // "DisplayName"
    const STR_APP_NAME: u32 = 107; // "TestApp"
    const STR_SHORTCUT: u32 = 115; // "C:\ProgramData\...\TestApp.lnk"
    const STR_TARGET: u32 = 180; // "C:\Program Files\TestApp\tool.exe"

    let mut strings = Vec::<u8>::new();
    for s in [
        &b""[..],
        b"C:\\Program Files\\TestApp",
        b"tool.exe",
        b"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\TestApp",
        b"DisplayName",
        b"TestApp",
        b"C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\TestApp.lnk",
        b"C:\\Program Files\\TestApp\\tool.exe",
    ] {
        strings.extend_from_slice(s);
        strings.push(0);
    }
    // Verify offset assumptions so test failures are obvious.
    assert_eq!(STR_INSTDIR as usize, 1);
    assert_eq!(STR_TOOL as usize, 1 + b"C:\\Program Files\\TestApp\0".len());
    assert_eq!(STR_TARGET as usize, 180, "string table offset mismatch");

    // Entries: 5 × [opcode, p0..p5] × 4 bytes each = 5 × 28 bytes.
    let mut entries = Vec::<u8>::new();
    let push_entry = |buf: &mut Vec<u8>, vals: [i32; 7]| {
        for v in vals {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    };
    push_entry(&mut entries, [25, 21, STR_INSTDIR as i32, 0, 0, 0, 0]); // AssignVar $INSTDIR
    push_entry(&mut entries, [11, STR_INSTDIR as i32, 1, 0, 0, 0, 0]); // SetOutPath
    push_entry(&mut entries, [20, 0, STR_TOOL as i32, 0, 0, 0, 0]); // ExtractFile
    push_entry(
        &mut entries,
        [
            51,
            0x80000002_u32 as i32, // HKLM
            STR_REG_KEY as i32,
            STR_DISP_NAME as i32,
            STR_APP_NAME as i32,
            1, // type = Sz
            0,
        ],
    ); // WriteRegStr
    push_entry(
        &mut entries,
        [
            45,
            STR_SHORTCUT as i32,
            STR_TARGET as i32,
            STR_EMPTY as i32,
            0,
            0,
            0,
        ],
    ); // CreateShortcut

    // NSIS header: u32 flags, 8 blocks × (offset u32, num u32), strings, entries.
    let strings_off = 4_u32 + 8 * 8; // = 68
    let entries_off = strings_off + strings.len() as u32;

    let mut header = Vec::<u8>::new();
    header.extend_from_slice(&0u32.to_le_bytes()); // flags
    for i in 0..8u32 {
        let (off, num) = match i {
            2 => (entries_off, entries.len() as u32 / 28),
            3 => (strings_off, 0u32),
            _ => (0u32, 0u32),
        };
        header.extend_from_slice(&off.to_le_bytes());
        header.extend_from_slice(&num.to_le_bytes());
    }
    header.extend_from_slice(&strings);
    header.extend_from_slice(&entries);
    let header_len = header.len() as u32;

    // Data area: one stored block (b"MZ tool") at offset 0.
    let file_data = b"MZ tool";
    let mut data = Vec::<u8>::new();
    data.extend_from_slice(&(file_data.len() as u32).to_le_bytes()); // stored
    data.extend_from_slice(file_data);

    // Body: non-solid stored header size word + header + data blocks.
    let mut body = Vec::<u8>::new();
    body.extend_from_slice(&header_len.to_le_bytes()); // no high bit = stored
    body.extend_from_slice(&header);
    body.extend_from_slice(&data);

    // NSIS first header (28 bytes): flags, magic, header_len, total_len.
    let total_len = 28u32 + body.len() as u32;
    let mut first_header = Vec::<u8>::new();
    first_header.extend_from_slice(&0u32.to_le_bytes()); // flags
    first_header.extend_from_slice(b"\xEF\xBE\xAD\xDENullsoftInst");
    first_header.extend_from_slice(&header_len.to_le_bytes());
    first_header.extend_from_slice(&total_len.to_le_bytes());
    assert_eq!(first_header.len(), 28);

    exe.extend_from_slice(&first_header);
    exe.extend_from_slice(&body);

    let installer = scratch.file("TestApp-setup.exe", &exe);
    let r = install(&env, &installer).unwrap();

    assert_eq!(r.app.name, "TestApp");
    assert_eq!(r.app.package, "NSIS installer");
    assert_eq!(
        r.app.main.as_deref(),
        Some(r"C:\Program Files\TestApp\tool.exe")
    );
    assert_eq!(read(&env, r"C:\Program Files\TestApp\tool.exe"), b"MZ tool");
    assert!(!r.app.shortcuts.is_empty(), "expected shortcut");
    assert!(
        r.app
            .registry_values
            .iter()
            .any(|[k, n]| { k.to_ascii_lowercase().contains("uninstall") && n == "DisplayName" }),
        "expected DisplayName registry entry"
    );

    uninstall(&env, "TestApp").unwrap();
    assert!(env.apps().unwrap().apps.is_empty(), "app not removed");
    let host = env.host_path(r"C:\Program Files\TestApp\tool.exe").unwrap();
    assert!(!host.exists(), "file not removed on uninstall");
}
