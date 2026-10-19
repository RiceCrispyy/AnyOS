//! `anyos`: command-line front end for the AnyOS runtime.
//!
//! * `anyos inspect <file>` reads a program (Windows PE in full detail; Linux ELF
//!   and macOS Mach-O in summary) and reports what it is and what AnyOS would need
//!   to run it. Installers and app packages (NSIS, Inno Setup, MSI, .deb, .pkg,
//!   ...) are recognised too. Nothing is executed.
//! * `anyos load <file>` goes one step further: it maps the image (and application
//!   DLLs beside it) into an emulated guest address space, relocates and binds it,
//!   and prints the resulting process layout. Still nothing is executed.
//! * `anyos run <file>` runs a program. Windows x86 and x64 `.exe` files run on
//!   AnyOS's portable interpreter on any host, with Windows API calls served
//!   through the call gate. A Linux x86-64 ELF runs on an x86-64 host: natively
//!   with Linux syscalls intercepted on Windows, directly on Linux.
//! * `anyos install <installer>` unpacks an installer (MSI, ZIP, 7-Zip) into an
//!   AnyOS environment; `anyos apps`, `anyos uninstall` and `anyos env` manage
//!   what is installed. No installer code runs.
//! * `anyos redirect ...` edits a program's own network redirects, stored in
//!   `<program>.anyos.toml` beside it. They apply only to that program when AnyOS
//!   runs it; the host's hosts file and DNS are never touched.
//! * `anyos update` installs the newest AnyOS release from GitHub (checked
//!   against its SHA-256), replacing this copy and the app beside it. Release
//!   builds also check once a day after a command and mention a new version
//!   on stderr; `--no-update-check` or `ANYOS_NO_UPDATE_CHECK=1` turns that off.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyos_env::{AppRecord, Env};
use anyos_exec::{run as exec_run, RunOptions};
use anyos_format::{Detected, GuestMode, ProgramSummary};
use anyos_loader::{BasePolicy, Binding, LoadOptions, Process};
use anyos_net::{profile_path, AppProfile, NetPolicy};
use anyos_package::{InstallRoute, Package, PackageKind};
use anyos_pe::catalog::classify;
use anyos_pe::report::{
    self, CompatibilityReport, DependencyLocation, HostArch, HostOs, HostPlatform, Status,
};
use anyos_pe::{consts, ExportTarget, PeImage};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "anyos",
    version = anyos_update::current_version(),
    about = "AnyOS: run programs from any OS on any OS (inspector and loader preview)"
)]
struct Cli {
    /// Do not look for a newer AnyOS release after this command.
    #[arg(long, global = true)]
    no_update_check: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect a program (Windows, Linux or macOS) without running it.
    Inspect(InspectArgs),
    /// Map, relocate and bind an image into an emulated guest process (dry run).
    Load(LoadArgs),
    /// Run a program: a Windows .exe (x86 or x64), or a Linux x86-64 ELF.
    Run(RunArgs),
    /// Per-program network redirects, applied inside AnyOS (not the host OS).
    #[command(subcommand)]
    Redirect(RedirectCommand),
    /// Install an app from its installer into an AnyOS environment.
    ///
    /// Supports .msi packages, ZIP and 7-Zip archives, and self-extracting
    /// ZIP/7-Zip .exe files. No installer code runs; nothing is written
    /// outside the environment.
    Install(InstallArgs),
    /// List the apps installed in an environment.
    Apps(AppsArgs),
    /// Remove an installed app from an environment.
    Uninstall(UninstallArgs),
    /// List environments, or show where one lives.
    Env(EnvArgs),
    /// Update AnyOS to the newest release.
    ///
    /// Downloads this platform's release from GitHub, checks its SHA-256
    /// against the release's SHA256SUMS, and replaces this copy of anyos and
    /// the AnyOS app installed beside it (the whole AnyOS.app on macOS).
    Update(UpdateArgs),
}

#[derive(Args)]
struct UpdateArgs {
    /// Only say whether a newer release exists; install nothing.
    #[arg(long)]
    check: bool,
}

#[derive(Args)]
struct EnvOpt {
    /// Environment to use (created when missing).
    #[arg(long = "env", default_value = anyos_env::DEFAULT_ENV)]
    name: String,
}

#[derive(Args)]
struct InstallArgs {
    /// The installer or package.
    installer: PathBuf,
    #[command(flatten)]
    env: EnvOpt,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct AppsArgs {
    #[command(flatten)]
    env: EnvOpt,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct UninstallArgs {
    /// App id or name, as `anyos apps` shows it.
    app: String,
    #[command(flatten)]
    env: EnvOpt,
}

#[derive(Args)]
struct EnvArgs {
    /// Show this environment's folders instead of listing all of them.
    name: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    /// Path to the program (Windows .exe or Linux ELF).
    program: PathBuf,
    /// Windows programs: print each Windows API call and its result to stderr.
    #[arg(long)]
    trace: bool,
    /// Arguments passed to the guest program (argv[1..]).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum RedirectCommand {
    /// Send a program's traffic for a domain (and its subdomains) or IP elsewhere.
    ///
    /// Example: anyos redirect add Game.exe oldgame.example 203.0.113.10
    Add {
        program: PathBuf,
        /// Domain or IP address the program asks for.
        from: String,
        /// Where to send it instead: host, IP, host:port or [IPv6]:port.
        to: String,
    },
    /// Remove a redirect.
    Remove { program: PathBuf, from: String },
    /// Show a program's redirects.
    List {
        program: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Show where the program's connection to HOST[:PORT] would go.
    Test {
        program: PathBuf,
        /// Host the program asks for, optionally with :port (default 80).
        host: String,
        /// Also look the result up in DNS.
        #[arg(long)]
        resolve: bool,
    },
}

#[derive(Args)]
struct InspectArgs {
    file: PathBuf,
    /// Emit machine-readable JSON (image details plus compatibility report).
    #[arg(long)]
    json: bool,
    /// List every section.
    #[arg(long)]
    sections: bool,
    /// List every imported function.
    #[arg(long)]
    imports: bool,
    /// List every exported function.
    #[arg(long)]
    exports: bool,
    /// List every base relocation (can be long).
    #[arg(long)]
    relocs: bool,
    /// Same as --sections --imports --exports.
    #[arg(long, short)]
    all: bool,
    /// Plan for another host instead of this machine:
    /// macos-arm64, macos-x64, linux-x64 or linux-arm64.
    #[arg(long, value_parser = parse_host)]
    host: Option<HostPlatform>,
}

#[derive(Args)]
struct LoadArgs {
    file: PathBuf,
    #[arg(long)]
    json: bool,
    /// Load at a different base than preferred, to exercise relocation.
    #[arg(long, conflicts_with = "base")]
    rebase: bool,
    /// Load at this base address (hex with 0x, or decimal).
    #[arg(long, value_parser = parse_u64)]
    base: Option<u64>,
    /// Do not load application DLLs from the executable's directory.
    #[arg(long)]
    no_deps: bool,
    /// Extra directories to search for application DLLs.
    #[arg(long = "search-dir")]
    search_dirs: Vec<PathBuf>,
    /// Print every bound import.
    #[arg(long)]
    imports: bool,
}

fn main() -> ExitCode {
    // Behave like other Unix tools when piped into `head`: exit quietly on EPIPE.
    #[cfg(unix)]
    // SAFETY: restoring the default disposition of SIGPIPE before any threads start.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Ok(layout) = anyos_update::Layout::current() {
        anyos_update::cleanup_leftovers(&layout);
    }
    let check_after = !cli.no_update_check && !matches!(cli.command, Command::Update(_));
    let result = match cli.command {
        Command::Inspect(a) => inspect(a),
        Command::Load(a) => load(a),
        Command::Run(a) => run_guest(a),
        Command::Redirect(c) => redirect(c),
        Command::Install(a) => install_app(a),
        Command::Apps(a) => list_apps(a),
        Command::Uninstall(a) => uninstall_app(a),
        Command::Env(a) => env_info(a),
        Command::Update(a) => update(a),
    };
    if check_after {
        update_notice();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("anyos: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn update_cache() -> Option<anyos_update::CheckCache> {
    let dir = anyos_env::data_dir().ok()?;
    Some(anyos_update::CheckCache::new(dir.join("update-check")))
}

/// Once a day, in release builds and only when someone is watching stderr,
/// asks GitHub for the latest release and mentions it if it is newer.
fn update_notice() {
    if !anyos_update::auto_check_enabled() || !std::io::stderr().is_terminal() {
        return;
    }
    let Some(cache) = update_cache() else { return };
    if cache.is_due() {
        match anyos_update::check(Duration::from_secs(3)) {
            Ok(u) => cache.record(u.as_ref().map(|u| &u.version)),
            // Offline: keep what we knew and try again tomorrow.
            Err(_) => cache.record(cache.newer_version().as_ref()),
        }
    }
    if let Some(v) = cache.newer_version() {
        eprintln!(
            "\nanyos: AnyOS {v} is available (this is {}). Run `anyos update` to install it.",
            anyos_update::current_version()
        );
    }
}

fn update(a: UpdateArgs) -> Result<(), String> {
    let current = anyos_update::current_version();
    println!("AnyOS {current}: checking for a newer release...");
    let found = anyos_update::check(Duration::from_secs(30)).map_err(|e| e.to_string())?;
    if let Some(cache) = update_cache() {
        cache.record(found.as_ref().map(|u| &u.version));
    }
    let Some(u) = found else {
        println!("This is the latest version.");
        return Ok(());
    };
    println!("AnyOS {} is available: {}", u.version, u.page_url);
    if a.check {
        println!("Run `anyos update` to install it.");
        return Ok(());
    }
    if !anyos_update::is_release_build() {
        eprintln!("note: this is a local build; it will be replaced by the release build.");
    }

    let show = std::io::stderr().is_terminal();
    let mut last_pct = None;
    let installed = anyos_update::install(&u, |done, total| {
        let Some(total) = total.filter(|t| *t > 0 && show) else {
            return;
        };
        let pct = done * 100 / total;
        if last_pct != Some(pct) {
            last_pct = Some(pct);
            eprint!("\rDownloading {} ... {pct}%", u.archive_name);
            let _ = std::io::stderr().flush();
        }
    });
    if last_pct.is_some() {
        eprintln!();
    }
    let installed = installed.map_err(|e| {
        if e.is_permission_denied() {
            format!(
                "{e}\nAnyOS is installed in a folder you cannot write to; \
                 run the update again as an administrator (e.g. with sudo)."
            )
        } else {
            e.to_string()
        }
    })?;
    println!(
        "Updated to AnyOS {} in {} ({}).",
        installed.version,
        installed.layout.dir.display(),
        installed.replaced.join(", ")
    );
    Ok(())
}

fn run_guest(a: RunArgs) -> Result<(), String> {
    let bytes = std::fs::read(&a.program).map_err(|e| format!("{}: {e}", a.program.display()))?;
    if bytes.starts_with(b"MZ") {
        return run_windows(&a);
    }

    let name = a.program.display().to_string();
    let mut argv: Vec<&str> = vec![&name];
    for s in &a.args {
        argv.push(s.as_str());
    }

    let result = exec_run(
        &bytes,
        &RunOptions {
            argv: &argv,
            envp: &[],
        },
    )
    .map_err(|e| format!("run failed: {e}"))?;

    if result.exit_code != 0 {
        return Err(format!("guest exited with code {}", result.exit_code));
    }
    Ok(())
}

fn run_windows(a: &RunArgs) -> Result<(), String> {
    let opts = anyos_win32::RunOptions {
        args: a.args.clone(),
        trace: a.trace,
        ..Default::default()
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let r = anyos_win32::run_executable(
        &a.program,
        &opts,
        anyos_win32::Stdio {
            stdout: &mut out,
            stderr: &mut err,
        },
    )
    .map_err(|e| e.to_string())?;
    if a.trace {
        eprintln!(
            "[anyos] exit code {} after {} instructions and {} API calls",
            r.exit_code, r.instructions, r.api_calls
        );
    }
    if r.exit_code != 0 {
        return Err(format!("program exited with code {}", r.exit_code));
    }
    Ok(())
}

fn open_env(name: &str) -> Result<Env, String> {
    let dir = anyos_env::data_dir().map_err(|e| e.to_string())?;
    Env::open_or_create(&dir, name).map_err(|e| e.to_string())
}

fn install_app(a: InstallArgs) -> Result<(), String> {
    let env = open_env(&a.env.name)?;
    let r = anyos_install::install(&env, &a.installer).map_err(|e| e.to_string())?;
    if a.json {
        let out = serde_json::json!({
            "env": env.name(),
            "app": app_json(&env, &r.app),
            "bytes_written": r.bytes_written,
            "registry_values": r.registry_values,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let app = &r.app;
    println!(
        "Installed {}{} into environment \"{}\"",
        app.name,
        app.version
            .as_ref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default(),
        env.name()
    );
    println!("  Id:              {}", app.id);
    println!("  From:            {} ({})", app.source, app.package);
    println!(
        "  Files:           {} ({} bytes)",
        app.files.len(),
        r.bytes_written
    );
    if r.registry_values > 0 {
        println!("  Registry values: {}", r.registry_values);
    }
    print_app_paths(&env, app);
    for s in &app.shortcuts {
        println!("  Shortcut:        {} -> {}", s.name, s.target);
    }
    if !app.notes.is_empty() {
        println!("\nSkipped");
        for n in &app.notes {
            println!("  - {n}");
        }
    }
    println!("\nAnyOS cannot start Windows programs yet; the app is ready for when it can.");
    Ok(())
}

fn print_app_paths(env: &Env, app: &AppRecord) {
    let host = |w: &str| {
        env.host_path(w)
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    if let Some(d) = &app.install_dir {
        println!("  Folder:          {d}");
        println!("                   {}", host(d));
    }
    if let Some(m) = &app.main {
        println!("  Program:         {m}");
    }
}

fn app_json(env: &Env, app: &AppRecord) -> serde_json::Value {
    let mut v = serde_json::to_value(app).unwrap_or_default();
    if let Some(m) = &app.main {
        v["main_host_path"] =
            serde_json::json!(env.host_path(m).map(|p| p.display().to_string()).ok());
    }
    v
}

fn list_apps(a: AppsArgs) -> Result<(), String> {
    let env = open_env(&a.env.name)?;
    let index = env.apps().map_err(|e| e.to_string())?;
    if a.json {
        let apps: Vec<_> = index.apps.iter().map(|app| app_json(&env, app)).collect();
        let out = serde_json::json!({ "env": env.name(), "apps": apps });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if index.apps.is_empty() {
        println!("No apps installed in environment \"{}\".", env.name());
        return Ok(());
    }
    println!("Apps in environment \"{}\"", env.name());
    for app in &index.apps {
        println!(
            "\n{} ({}){}",
            app.name,
            app.id,
            app.version
                .as_ref()
                .map(|v| format!(", version {v}"))
                .unwrap_or_default()
        );
        print_app_paths(&env, app);
    }
    Ok(())
}

fn uninstall_app(a: UninstallArgs) -> Result<(), String> {
    let env = open_env(&a.env.name)?;
    let r = anyos_install::uninstall(&env, &a.app).map_err(|e| e.to_string())?;
    println!(
        "Removed {} from environment \"{}\": {} file(s){}{}",
        r.app.name,
        env.name(),
        r.files_removed,
        if r.files_shared > 0 {
            format!(", {} kept for other apps", r.files_shared)
        } else {
            String::new()
        },
        if r.registry_keys_removed > 0 {
            format!(", {} registry key(s)", r.registry_keys_removed)
        } else {
            String::new()
        }
    );
    Ok(())
}

fn env_info(a: EnvArgs) -> Result<(), String> {
    let dir = anyos_env::data_dir().map_err(|e| e.to_string())?;
    match a.name {
        None => {
            let names = anyos_env::list(&dir).map_err(|e| e.to_string())?;
            if names.is_empty() {
                println!("No environments yet. `anyos install` creates \"default\".");
            }
            for n in names {
                println!("{n}");
            }
            println!("\nData folder: {}", dir.display());
        }
        Some(name) => {
            let env = open_env(&name)?;
            let apps = env.apps().map_err(|e| e.to_string())?;
            println!("Environment \"{}\"", env.name());
            println!("  Folder:          {}", env.root().display());
            println!("  C: drive:        {}", env.drive_c().display());
            println!("  Windows user:    {}", env.user());
            println!("  Apps installed:  {}", apps.apps.len());
        }
    }
    Ok(())
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16),
        None => s.replace('_', "").parse(),
    };
    r.map_err(|e| e.to_string())
}

fn parse_host(s: &str) -> Result<HostPlatform, String> {
    let (os, arch) = s
        .split_once('-')
        .ok_or("expected <os>-<arch>, e.g. macos-arm64")?;
    let os = match os {
        "macos" | "mac" => HostOs::MacOs,
        "linux" | "chromeos" => HostOs::Linux,
        _ => return Err(format!("unknown OS {os:?} (macos, linux)")),
    };
    let arch = match arch {
        "arm64" | "aarch64" => HostArch::Aarch64,
        "x64" | "x86_64" | "x86-64" => HostArch::X86_64,
        _ => return Err(format!("unknown CPU {arch:?} (arm64, x64)")),
    };
    Ok(HostPlatform::new(os, arch))
}

fn host_platform(a: &InspectArgs) -> HostPlatform {
    a.host.unwrap_or_else(HostPlatform::current)
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Whether a DLL named `dll` sits next to `path` (case-insensitive, like Windows).
fn beside(path: &Path) -> impl Fn(&str) -> bool {
    let names: Vec<String> = std::fs::read_dir(parent_dir(path))
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    move |dll: &str| names.contains(&dll.to_ascii_lowercase())
}

fn load_policy(program: &Path) -> Result<NetPolicy, String> {
    AppProfile::load_for(program)
        .and_then(|p| p.net_policy())
        .map_err(|e| e.to_string())
}

fn redirect(c: RedirectCommand) -> Result<(), String> {
    match c {
        RedirectCommand::Add { program, from, to } => {
            let mut profile = AppProfile::load_for(&program).map_err(|e| e.to_string())?;
            let mut policy = profile.net_policy().map_err(|e| e.to_string())?;
            policy.add_redirect(&from, &to).map_err(|e| e.to_string())?;
            profile.set_net_policy(&policy);
            let path = profile.save_for(&program).map_err(|e| e.to_string())?;
            println!(
                "{}: {from} (and its subdomains) -> {to}",
                file_name(&program)
            );
            println!("Saved in {}", path.display());
        }
        RedirectCommand::Remove { program, from } => {
            let mut profile = AppProfile::load_for(&program).map_err(|e| e.to_string())?;
            let mut policy = profile.net_policy().map_err(|e| e.to_string())?;
            if !policy.remove_redirect(&from) {
                return Err(format!(
                    "{} has no redirect for {from}",
                    file_name(&program)
                ));
            }
            profile.set_net_policy(&policy);
            let path = profile.save_for(&program).map_err(|e| e.to_string())?;
            println!("Removed {from}; saved in {}", path.display());
        }
        RedirectCommand::List { program, json } => {
            let policy = load_policy(&program)?;
            if json {
                let map: serde_json::Map<String, serde_json::Value> = policy
                    .redirects()
                    .map(|(k, v)| (k.to_string(), v.to_string().into()))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "program": program.display().to_string(),
                        "profile": profile_path(&program).display().to_string(),
                        "redirects": map,
                    }))
                    .map_err(|e| e.to_string())?
                );
            } else if policy.is_empty() {
                println!("{} has no redirects", file_name(&program));
            } else {
                print_redirects(&program, &policy);
            }
        }
        RedirectCommand::Test {
            program,
            host,
            resolve,
        } => {
            let policy = load_policy(&program)?;
            let (name, port) = split_host_port(&host)?;
            let r = policy.rewrite(&name, port);
            match &r.rule {
                Some(rule) => println!("{name}:{port} -> {}:{}  (rule {rule})", r.host, r.port),
                None => println!("{name}:{port} -> unchanged (no rule matches)"),
            }
            if resolve {
                let addrs = policy
                    .resolve(&name, port)
                    .map_err(|e| format!("lookup of {}: {e}", r.host))?;
                for a in addrs {
                    println!("  connects to {a}");
                }
            }
        }
    }
    Ok(())
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`; port defaults to 80.
fn split_host_port(s: &str) -> Result<(String, u16), String> {
    let t = anyos_net::Target::parse(s).map_err(|e| e.to_string())?;
    Ok((t.host, t.port.unwrap_or(80)))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn print_redirects(program: &Path, policy: &NetPolicy) {
    println!(
        "\nNetwork redirects (inside AnyOS only, from {})",
        profile_path(program)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    for (from, to) in policy.redirects() {
        println!("  {from:<36} -> {to}");
    }
}

fn redirects_json(policy: &NetPolicy) -> serde_json::Value {
    policy
        .redirects()
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v.to_string())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn inspect(a: InspectArgs) -> Result<(), String> {
    let bytes = std::fs::read(&a.file).map_err(|e| format!("{}: {e}", a.file.display()))?;
    let detected = match anyos_format::detect(&bytes) {
        Ok(d) => d,
        Err(e) => match anyos_package::identify(&bytes, None) {
            Some(package) => return inspect_package(&a, bytes.len(), &package),
            None => return Err(format!("{}: {e}", a.file.display())),
        },
    };
    let policy = load_policy(&a.file)?;
    let image = match detected {
        Detected::Pe(image) => *image,
        Detected::Other(summary) => {
            let package = anyos_package::identify(&bytes, None);
            return inspect_other(&a, bytes.len(), &summary, package.as_ref(), &policy);
        }
    };
    let package = anyos_package::identify(&bytes, Some(&image));
    let report = report::build(&image, host_platform(&a), &beside(&a.file));

    if a.json {
        let mut img = serde_json::to_value(&image).map_err(|e| e.to_string())?;
        if !a.relocs {
            img["relocations"] = serde_json::json!(image.relocations.len());
        }
        let out = serde_json::json!({
            "file": a.file.display().to_string(),
            "image": img,
            "report": report,
            "package": package,
            "redirects": redirects_json(&policy),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    print_summary(&a.file, &image, &report);
    if a.sections || a.all {
        print_sections(&image);
    }
    if a.imports || a.all {
        print_imports(&image);
    }
    if a.exports || a.all {
        print_exports(&image);
    }
    if a.relocs {
        println!("\nBase relocations ({})", image.relocations.len());
        for r in &image.relocations {
            println!("  0x{:08x}  {:?}", r.rva, r.kind);
        }
    }
    print_report(&report);
    if let Some(p) = &package {
        print_package(p);
    }
    if !policy.is_empty() {
        print_redirects(&a.file, &policy);
    }
    Ok(())
}

fn inspect_package(a: &InspectArgs, size: usize, p: &Package) -> Result<(), String> {
    if a.json {
        let out = serde_json::json!({
            "file": a.file.display().to_string(),
            "package": p,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!("AnyOS executable inspector");
    println!("  File:            {} ({size} bytes)", file_name(&a.file));
    print_package(p);
    Ok(())
}

fn print_package(p: &Package) {
    println!("\nInstaller");
    println!(
        "  Kind:            {} (installs {} software)",
        p.kind.display_name(),
        p.kind.os().display_name()
    );
    if let Some(off) = p.payload_offset {
        println!("  Payload at:      0x{off:x}");
    }
    let route = match p.kind.route() {
        InstallRoute::Extract => "unpack the payload directly (no guest code runs)",
        InstallRoute::RunInstaller => "run the installer as a guest program",
    };
    println!("  AnyOS route:     {route}");
    let silent = p.kind.silent_args();
    if !silent.is_empty() {
        println!("  Silent install:  {}", silent.join(" "));
    }
    if !matches!(
        p.kind,
        PackageKind::Msi
            | PackageKind::Nsis
            | PackageKind::Zip
            | PackageKind::ZipSfx
            | PackageKind::SevenZip
            | PackageKind::SevenZipSfx
            | PackageKind::Deb
            | PackageKind::MacPkg
    ) {
        println!("  `anyos install` does not support this kind yet.");
    }
}

fn inspect_other(
    a: &InspectArgs,
    size: usize,
    p: &ProgramSummary,
    package: Option<&Package>,
    policy: &NetPolicy,
) -> Result<(), String> {
    let plan = anyos_format::plan(p, host_platform(a));
    if a.json {
        let out = serde_json::json!({
            "file": a.file.display().to_string(),
            "program": p,
            "plan": plan,
            "package": package,
            "redirects": redirects_json(policy),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!("AnyOS executable inspector");
    println!("  File:            {} ({size} bytes)", file_name(&a.file));
    println!(
        "  Format:          {}{}",
        p.format,
        if p.slices.len() > 1 {
            format!(" (universal, {} architectures)", p.slices.len())
        } else {
            String::new()
        }
    );
    println!(
        "  Built for:       {}{}",
        p.os.display_name(),
        p.os_note
            .as_ref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default()
    );
    for (i, s) in p.slices.iter().enumerate() {
        if p.slices.len() > 1 {
            println!("\nArchitecture {}", i + 1);
        }
        println!(
            "  Architecture:    {} ({}-bit), {:?}",
            s.cpu.display_name(),
            s.bits,
            s.kind
        );
        if let Some(e) = s.entry {
            println!("  Entry:           0x{e:x}");
        }
        if let Some(v) = &s.min_os_version {
            println!("  Minimum OS:      {} {v}", p.os.display_name());
        }
        match &s.interpreter {
            Some(i) => println!("  Loader:          {i}"),
            None if s.statically_linked => println!("  Linking:         static"),
            None => {}
        }
        if !s.libraries.is_empty() {
            println!("  Libraries:       {}", s.libraries.join(", "));
        }
    }
    let mode = match plan.mode {
        GuestMode::HostNative => "runs directly",
        GuestMode::NativeCpu => "native CPU, AnyOS OS layer",
        GuestMode::Translated => "CPU translation and AnyOS OS layer",
        GuestMode::Unsupported => "not supported",
    };
    println!("\nExecution");
    println!("  Host {}: {mode}", plan.host.display_name());
    println!("  {}", plan.note);
    if let Some(layer) = plan.personality {
        println!("  OS layer needed: {layer}");
    }
    if let Some(pkg) = package {
        print_package(pkg);
    }
    if !policy.is_empty() {
        print_redirects(&a.file, policy);
    }
    Ok(())
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn print_summary(path: &Path, image: &PeImage, report: &CompatibilityReport) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("AnyOS executable inspector");
    println!("  File:            {name} ({} bytes)", image.file_size);
    println!(
        "  Format:          {} {}",
        image.format.display_name(),
        if image.is_dll { "DLL" } else { "executable" }
    );
    println!("  Architecture:    {}", image.machine.display_name());
    println!("  Subsystem:       {}", image.subsystem.display_name());
    match &image.clr {
        Some(c) => println!(
            "  Managed (.NET):  yes, metadata {}{}{}",
            c.metadata_version.as_deref().unwrap_or("?"),
            if c.il_only {
                ", IL-only"
            } else {
                ", mixed-mode"
            },
            if c.requires_32bit {
                ", 32-bit required"
            } else {
                ""
            }
        ),
        None => println!("  Managed (.NET):  no"),
    }
    println!(
        "  Image:           base 0x{:x}, size 0x{:x}, entry {}",
        image.image_base,
        image.size_of_image,
        image
            .entry_point_rva
            .map_or("none".to_string(), |e| format!("RVA 0x{e:x}"))
    );
    println!(
        "  Targets:         Windows {}.{} (subsystem {}.{}), linker {}.{}",
        image.os_version.0,
        image.os_version.1,
        image.subsystem_version.0,
        image.subsystem_version.1,
        image.linker_version.0,
        image.linker_version.1
    );
    let mut hardening = Vec::new();
    for (flag, label) in [
        (consts::IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE, "ASLR"),
        (
            consts::IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA,
            "high-entropy VA",
        ),
        (consts::IMAGE_DLLCHARACTERISTICS_NX_COMPAT, "DEP/NX"),
        (
            consts::IMAGE_DLLCHARACTERISTICS_GUARD_CF,
            "Control Flow Guard",
        ),
        (
            consts::IMAGE_DLLCHARACTERISTICS_APPCONTAINER,
            "AppContainer",
        ),
    ] {
        if image.dll_characteristics & flag != 0 {
            hardening.push(label);
        }
    }
    println!(
        "  Hardening:       {}",
        if hardening.is_empty() {
            "none".to_string()
        } else {
            hardening.join(", ")
        }
    );
    println!(
        "  Signed:          {}   Resources: {}   Debug info: {}   Overlay: {} bytes",
        yes_no(image.has_signature),
        yes_no(image.has_resources),
        yes_no(image.has_debug_info),
        image.overlay_size
    );
    let delay = image.imports.iter().filter(|m| m.delay_load).count();
    println!(
        "  Imports:         {} function(s) from {} DLL(s), {} delay-loaded",
        image.import_count(),
        image.imports.len(),
        delay
    );
    let missing = report.missing_dependencies().count();
    if missing > 0 {
        println!("  Unresolved DLLs: {missing} application DLL(s) not found beside the file");
    }
    match &image.exports {
        Some(e) => println!(
            "  Exports:         {} ({} by name){}",
            e.exports.len(),
            e.exports.iter().filter(|x| x.name.is_some()).count(),
            e.dll_name
                .as_ref()
                .map(|n| format!(", module name {n}"))
                .unwrap_or_default()
        ),
        None => println!("  Exports:         none"),
    }
    println!(
        "  Relocations:     {}{}",
        image.relocations.len(),
        if image.relocations_stripped() {
            " (stripped: fixed base only)"
        } else {
            ""
        }
    );
    if let Some(t) = &image.tls {
        println!(
            "  TLS:             {} callback(s), {} byte template",
            t.callbacks.len(),
            t.raw_data_end_va.saturating_sub(t.raw_data_start_va)
        );
    }
    println!(
        "  Sections:        {}",
        image
            .sections
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
}

fn print_sections(image: &PeImage) {
    println!("\nSections");
    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}  access",
        "name", "rva", "vsize", "raw off", "raw size"
    );
    for s in &image.sections {
        println!(
            "  {:<10} 0x{:08x} 0x{:08x} 0x{:08x} 0x{:08x}  {}",
            s.name,
            s.virtual_address,
            s.virtual_size,
            s.raw_offset,
            s.raw_size,
            s.access.flags()
        );
    }
}

fn print_imports(image: &PeImage) {
    println!("\nImports");
    for m in &image.imports {
        println!(
            "  {}{}  [{}]",
            m.dll,
            if m.delay_load { " (delay-load)" } else { "" },
            classify(&m.dll).display_name()
        );
        for s in &m.symbols {
            println!("      {}  (IAT RVA 0x{:x})", s.name, s.iat_rva);
        }
    }
}

fn print_exports(image: &PeImage) {
    let Some(t) = &image.exports else { return };
    println!("\nExports (ordinal base {})", t.ordinal_base);
    for e in &t.exports {
        let target = match &e.target {
            ExportTarget::Rva { rva } => format!("RVA 0x{rva:x}"),
            ExportTarget::Forwarder { target } => format!("-> {target}"),
        };
        println!(
            "  {:>5}  {:<40} {}",
            e.ordinal,
            e.name.as_deref().unwrap_or("(no name)"),
            target
        );
    }
}

fn print_report(report: &CompatibilityReport) {
    println!("\nDependencies");
    if report.dependencies.is_empty() {
        println!("  none");
    }
    for d in &report.dependencies {
        let loc = match d.location {
            DependencyLocation::ProvidedByRuntime => "AnyOS must provide",
            DependencyLocation::FoundBesideImage => "found beside file",
            DependencyLocation::Missing => "MISSING",
        };
        println!(
            "  {:<40} {:<36} {:>5} fn  {}{}",
            d.dll,
            d.family.display_name(),
            d.symbols,
            loc,
            if d.delay_load { " (delay-load)" } else { "" }
        );
    }
    println!("\nExecution");
    println!(
        "  Host {}, guest {}: {}",
        report.execution.host.display_name(),
        report.execution.guest.display_name(),
        report.execution.note
    );
    println!("\nRequired capabilities");
    for c in &report.capabilities {
        let mark = match c.status {
            Status::Implemented => "[x]",
            Status::Planned => "[ ]",
            Status::Unsupported => "[-]",
        };
        println!("  {mark} {:<40} {}", c.name, c.detail);
    }
    println!("\nReadiness: {}", report.summary);
    if !report.issues.is_empty() {
        println!("\nIssues");
        for i in &report.issues {
            println!("  - {i}");
        }
    }
}

fn load(a: LoadArgs) -> Result<(), String> {
    let mut search_dirs = vec![parent_dir(&a.file)];
    search_dirs.extend(a.search_dirs.iter().cloned());
    let opts = LoadOptions {
        base: match (a.rebase, a.base) {
            (true, _) => BasePolicy::ForceRelocation,
            (_, Some(b)) => BasePolicy::At(b),
            _ => BasePolicy::Preferred,
        },
        search_dirs,
        load_dependencies: !a.no_deps,
    };
    let process = Process::load_executable(&a.file, &opts)
        .map_err(|e| format!("{}: {e}", a.file.display()))?;

    if a.json {
        let memory: Vec<_> = process
            .memory
            .regions()
            .iter()
            .map(|r| {
                serde_json::json!({
                    "base": r.base,
                    "size": r.size,
                    "tag": r.tag,
                    "protections": r.protections.iter()
                        .map(|(o, l, p)| serde_json::json!({"offset": o, "size": l, "protection": p.to_string()}))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let out = serde_json::json!({
            "file": a.file.display().to_string(),
            "machine": process.machine,
            "format": process.format,
            "modules": process.modules,
            "call_gate": process.call_gate.entries(),
            "memory": memory,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    let (low, high) = process.memory.bounds();
    println!("AnyOS loader (dry run: the image is prepared, nothing is executed)");
    println!(
        "  Process:  {} {}, guest address space 0x{low:x}..0x{high:x}",
        process.format.display_name(),
        process.machine.display_name()
    );
    for m in &process.modules {
        println!("\nModule {}", m.name);
        println!(
            "  Base:        0x{:x}{}  size 0x{:x}",
            m.base,
            if m.rebased() {
                format!(" (rebased from 0x{:x})", m.preferred_base)
            } else {
                " (preferred)".into()
            },
            m.size
        );
        println!(
            "  Entry point: {}",
            m.entry_point.map_or("none".into(), |e| format!("0x{e:x}"))
        );
        println!("  Relocations: {} applied", m.relocations_applied);
        for s in &m.sections {
            println!(
                "    {:<10} 0x{:x}  0x{:<8x} {}",
                s.name, s.address, s.size, s.protection
            );
        }
        let to_modules = m
            .imports
            .iter()
            .filter(|i| matches!(i.binding, Binding::Module { .. }))
            .count();
        println!(
            "  Imports:     {} bound ({} to guest modules, {} to the API call gate)",
            m.imports.len(),
            to_modules,
            m.imports.len() - to_modules
        );
        if a.imports {
            for i in &m.imports {
                let how = match &i.binding {
                    Binding::Module { module } => format!("export of {module}"),
                    Binding::CallGate { stub } => format!("call gate stub #{stub}"),
                };
                println!(
                    "    {}!{}  IAT 0x{:x} -> 0x{:x}  ({how})",
                    i.dll, i.symbol, i.iat_address, i.target
                );
            }
        }
        if !m.delay_load_dlls.is_empty() {
            println!(
                "  Delay-load:  {} (resolved on first call)",
                m.delay_load_dlls.join(", ")
            );
        }
        if let Some(t) = &m.tls {
            println!(
                "  TLS:         slot {}, template 0x{:x}..0x{:x}, callbacks [{}]",
                t.index,
                t.template_start,
                t.template_end,
                t.callbacks
                    .iter()
                    .map(|c| format!("0x{c:x}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!(
            "  Stack:       reserve 0x{:x}, commit 0x{:x}",
            m.stack_reserve, m.stack_commit
        );
    }
    println!(
        "\nAPI call gate: {} stub(s) for runtime-provided functions",
        process.call_gate.entries().len()
    );
    println!("\nMemory map");
    for r in process.memory.regions() {
        let prots: Vec<String> = r
            .protections
            .iter()
            .map(|(o, l, p)| format!("+0x{o:x}/0x{l:x} {p}"))
            .collect();
        println!(
            "  0x{:012x}-0x{:012x}  {:<28} {}",
            r.base,
            r.base + r.size,
            r.tag,
            prots.join("  ")
        );
    }
    println!(
        "\nNext step: an execution backend (not built yet) would start at the entry point above."
    );
    Ok(())
}
