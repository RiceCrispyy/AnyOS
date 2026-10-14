//! The app's side of self-update: checks and installs run on a background
//! thread and report back here; the header shows the result.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use anyos_update::{Installed, Update};

pub enum State {
    Idle,
    Checking {
        manual: bool,
    },
    /// Shown only after a check the user asked for.
    UpToDate,
    Available(Update),
    Downloading {
        version: String,
        done: u64,
        total: Option<u64>,
    },
    Failed(String),
}

enum Msg {
    Checked {
        manual: bool,
        result: Result<Option<Update>, String>,
    },
    Progress(u64, Option<u64>),
    Installed(Result<Installed, String>),
}

pub struct Updater {
    pub state: State,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Updater {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Updater {
            state: State::Idle,
            tx,
            rx,
        }
    }

    pub fn busy(&self) -> bool {
        matches!(
            self.state,
            State::Checking { .. } | State::Downloading { .. }
        )
    }

    /// Looks for a newer release. Automatic checks stay quiet unless one exists.
    pub fn check(&mut self, manual: bool, repaint: impl Fn() + Send + 'static) {
        if self.busy() {
            return;
        }
        self.state = State::Checking { manual };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = anyos_update::check(Duration::from_secs(20)).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Checked { manual, result });
            repaint();
        });
    }

    /// Downloads and installs the available update.
    pub fn install(&mut self, repaint: impl Fn() + Send + Sync + 'static) {
        let State::Available(u) = &self.state else {
            return;
        };
        let u = u.clone();
        self.state = State::Downloading {
            version: u.version.to_string(),
            done: 0,
            total: None,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let result = anyos_update::install(&u, |done, total| {
                let _ = progress_tx.send(Msg::Progress(done, total));
                repaint();
            })
            .map_err(|e| {
                if e.is_permission_denied() {
                    format!("{e}. AnyOS is in a folder this account cannot change; move it somewhere you own, or update with `sudo anyos update`.")
                } else {
                    e.to_string()
                }
            });
            let _ = tx.send(Msg::Installed(result));
            repaint();
        });
    }

    pub fn dismiss(&mut self) {
        if !self.busy() {
            self.state = State::Idle;
        }
    }

    /// Applies background results. Returns the finished install, if one just
    /// completed, so the app can restart into it.
    pub fn poll(&mut self) -> Option<Installed> {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Checked { manual, result } => {
                    self.state = match result {
                        Ok(Some(u)) => State::Available(u),
                        Ok(None) if manual => State::UpToDate,
                        Err(e) if manual => State::Failed(e),
                        _ => State::Idle,
                    };
                }
                Msg::Progress(d, t) => {
                    if let State::Downloading { done, total, .. } = &mut self.state {
                        *done = d;
                        *total = t;
                    }
                }
                Msg::Installed(Ok(i)) => return Some(i),
                Msg::Installed(Err(e)) => self.state = State::Failed(e),
            }
        }
        None
    }
}

/// Starts the freshly installed app. On macOS a bundle is opened through
/// LaunchServices so it starts as a proper app.
pub fn relaunch(installed: &Installed) -> std::io::Result<()> {
    let exe = format!("anyos-gui{}", std::env::consts::EXE_SUFFIX);
    let layout = &installed.layout;
    if cfg!(target_os = "macos") && layout.is_bundle() {
        std::process::Command::new("open")
            .arg("-n")
            .arg(layout.dir.join(&layout.own_item))
            .spawn()?;
    } else {
        std::process::Command::new(layout.relaunch_path(&exe)).spawn()?;
    }
    Ok(())
}
