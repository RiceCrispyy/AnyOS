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
    uninstall(&env, "game-1-0").unwrap();
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
        Err(InstallError::Unsupported { .. })
    ));
    let text = scratch.file("notes.txt", b"hello");
    assert!(matches!(
        install(&env, &text),
        Err(InstallError::NotAPackage(_))
    ));
    assert!(env.apps().unwrap().apps.is_empty());
}
