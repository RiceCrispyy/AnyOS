//! Runs a program in a child process and streams its output back.
//!
//! Programs run through the `anyos` command-line tool rather than inside the
//! GUI process, so a guest that faults or never returns cannot take the window
//! down with it, and the Stop button can always end it.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Stdout(String),
    Stderr(String),
    /// The process ended; `None` when a signal ended it.
    Exited(Option<i32>),
}

pub struct Job {
    child: Arc<Mutex<Child>>,
    rx: Receiver<Event>,
}

impl Job {
    /// Start `cmd`. `wake` is called from a background thread whenever new
    /// events are ready (the GUI passes a repaint request).
    pub fn start(
        mut cmd: Command,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Job> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let (tx, rx) = mpsc::channel();
        let readers = [
            child
                .stdout
                .take()
                .map(|s| pump(s, tx.clone(), wake.clone(), Event::Stdout)),
            child
                .stderr
                .take()
                .map(|s| pump(s, tx.clone(), wake.clone(), Event::Stderr)),
        ];
        let child = Arc::new(Mutex::new(child));
        let waiter = child.clone();
        thread::spawn(move || {
            // Poll instead of blocking in wait() so `stop` can take the lock.
            let status = loop {
                match waiter.lock().unwrap().try_wait() {
                    Ok(Some(s)) => break Some(s),
                    Ok(None) => {}
                    Err(_) => break None,
                }
                thread::sleep(Duration::from_millis(30));
            };
            // Deliver all output before the exit line.
            for r in readers.into_iter().flatten() {
                let _ = r.join();
            }
            let _ = tx.send(Event::Exited(status.and_then(|s| s.code())));
            wake();
        });
        Ok(Job { child, rx })
    }

    /// Events that arrived since the last call.
    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }

    /// End the process. Its `Exited` event still arrives.
    pub fn stop(&self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

fn pump(
    stream: impl Read + Send + 'static,
    tx: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    make: fn(String) -> Event,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut r = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n' | b'\r')) {
                        buf.pop();
                    }
                    if tx
                        .send(make(String::from_utf8_lossy(&buf).into_owned()))
                        .is_err()
                    {
                        break;
                    }
                    wake();
                }
            }
        }
    })
}

/// Where the `anyos` command-line tool is: `$ANYOS_CLI`, then beside this app
/// (release archives and the macOS app bundle ship both together), then `PATH`.
pub fn find_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ANYOS_CLI") {
        return Some(PathBuf::from(p));
    }
    let exe = format!("anyos{}", std::env::consts::EXE_SUFFIX);
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let p = dir.join(&exe);
        if p.is_file() {
            return Some(p);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(&exe))
            .find(|p| p.is_file())
    })
}

/// `anyos run [--trace] <program> -- <args>`
pub fn run_command(cli: &Path, program: &Path, args: &[String], trace: bool) -> Command {
    let mut c = Command::new(cli);
    c.arg("run");
    if trace {
        c.arg("--trace");
    }
    c.arg(program);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    with_program_dir(c, program)
}

/// `anyos install <installer>`
pub fn install_command(cli: &Path, installer: &Path) -> Command {
    let mut c = Command::new(cli);
    c.arg("install").arg(installer);
    c
}

/// `anyos uninstall <app_id> --env <env_name>`
pub fn uninstall_command(cli: &Path, app_id: &str, env_name: &str) -> Command {
    let mut c = Command::new(cli);
    c.arg("uninstall").arg(app_id).arg("--env").arg(env_name);
    c
}

/// A program built for this computer, started without AnyOS. A macOS `.app`
/// goes through `open` so it starts the way Finder would start it.
pub fn launch_command(picked: &Path, program: &Path, args: &[String]) -> Command {
    if cfg!(target_os = "macos") && picked.is_dir() {
        let mut c = Command::new("open");
        c.arg("-W").arg(picked);
        if !args.is_empty() {
            c.arg("--args").args(args);
        }
        return c;
    }
    let mut c = Command::new(program);
    c.args(args);
    with_program_dir(c, program)
}

/// Programs often expect to start in their own folder (to find data files and DLLs).
fn with_program_dir(mut c: Command, program: &Path) -> Command {
    if let Some(dir) = program.parent().filter(|d| !d.as_os_str().is_empty()) {
        c.current_dir(dir);
    }
    c
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn collect(job: &Job) -> Vec<Event> {
        let mut all = Vec::new();
        loop {
            let e = job.rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let done = matches!(e, Event::Exited(_));
            all.push(e);
            if done {
                return all;
            }
        }
    }

    #[test]
    fn streams_output_then_exit_code() {
        let mut c = Command::new("sh");
        c.args(["-c", "echo one; echo two >&2; printf three; exit 3"]);
        let job = Job::start(c, || {}).unwrap();
        let ev = collect(&job);
        assert!(ev.contains(&Event::Stdout("one".into())));
        assert!(ev.contains(&Event::Stderr("two".into())));
        assert!(ev.contains(&Event::Stdout("three".into())));
        assert_eq!(ev.last(), Some(&Event::Exited(Some(3))));
    }

    #[test]
    fn stop_ends_a_running_program() {
        let mut c = Command::new("sleep");
        c.arg("30");
        let job = Job::start(c, || {}).unwrap();
        job.stop();
        assert_eq!(collect(&job).last(), Some(&Event::Exited(None)));
    }
}
