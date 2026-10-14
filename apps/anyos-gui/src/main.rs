//! AnyOS desktop app: pick or drop a program, see what it is and how AnyOS
//! would run it on this computer, then run it and watch its output.
//!
//! Release builds look for a newer AnyOS on start (skip with
//! `--no-update-check` or `ANYOS_NO_UPDATE_CHECK=1`) and offer to update and
//! restart; "Check for updates" in the header asks any time.

// No console window behind the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod analysis;
mod runner;
mod updater;

use std::path::{Path, PathBuf};

use analysis::{Action, Analysis, Tone};
use anyos_pe::report::HostPlatform;
use eframe::egui::{self, Color32, RichText};
use runner::{Event, Job};
use updater::Updater;

/// Output lines kept in the log; older ones are dropped.
const MAX_LOG_LINES: usize = 10_000;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AnyOS")
            .with_inner_size([760.0, 680.0])
            .with_min_inner_size([480.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let no_update_check = args.len() != {
        args.retain(|a| a != "--no-update-check");
        args.len()
    };
    let auto_check = !no_update_check && anyos_update::auto_check_enabled();
    if let Ok(layout) = anyos_update::Layout::current() {
        anyos_update::cleanup_leftovers(&layout);
    }
    let start = args.into_iter().next().map(PathBuf::from);
    eframe::run_native(
        "AnyOS",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.all_styles_mut(|s| {
                s.spacing.item_spacing = egui::vec2(8.0, 8.0);
                s.spacing.button_padding = egui::vec2(12.0, 6.0);
            });
            let mut app = AnyosApp::new();
            if auto_check {
                let ctx = cc.egui_ctx.clone();
                app.updater.check(false, move || ctx.request_repaint());
            }
            if let Some(p) = start {
                app.open(&p);
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stream {
    Out,
    Err,
    Info,
}

struct AnyosApp {
    host: HostPlatform,
    cli: Option<PathBuf>,
    analysis: Option<Result<Analysis, String>>,
    args: String,
    trace: bool,
    job: Option<Job>,
    log: Vec<(Stream, String)>,
    updater: Updater,
}

impl AnyosApp {
    fn new() -> Self {
        AnyosApp {
            host: HostPlatform::current(),
            cli: runner::find_cli(),
            analysis: None,
            args: String::new(),
            trace: false,
            job: None,
            log: Vec::new(),
            updater: Updater::new(),
        }
    }

    fn open(&mut self, path: &Path) {
        if self.job.is_some() {
            return;
        }
        self.analysis = Some(analysis::analyse(path, self.host));
        self.log.clear();
        self.args.clear();
    }

    fn pick_file(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Choose a program to run")
            .pick_file()
        {
            self.open(&p);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(Ok(a)) = &self.analysis else { return };
        let args = analysis::split_args(&self.args);
        let cmd = match (&a.action, &self.cli) {
            (Action::Launch, _) => runner::launch_command(&a.picked, &a.program, &args),
            (Action::Run, Some(cli)) => runner::run_command(cli, &a.program, &args, self.trace),
            (Action::Install, Some(cli)) => runner::install_command(cli, &a.program),
            _ => return,
        };
        self.log.clear();
        self.log
            .push((Stream::Info, format!("$ {}", describe(&cmd))));
        let ctx = ctx.clone();
        match Job::start(cmd, move || ctx.request_repaint()) {
            Ok(job) => self.job = Some(job),
            Err(e) => self
                .log
                .push((Stream::Err, format!("could not start: {e}"))),
        }
    }

    fn drain(&mut self) {
        let Some(job) = &self.job else { return };
        for ev in job.poll() {
            match ev {
                Event::Stdout(l) => self.log.push((Stream::Out, l)),
                Event::Stderr(l) => self.log.push((Stream::Err, l)),
                Event::Exited(code) => {
                    let msg = match code {
                        Some(0) => "Finished successfully.".to_string(),
                        Some(c) => format!("Exited with code {c}."),
                        None => "Stopped.".to_string(),
                    };
                    self.log.push((Stream::Info, msg));
                    self.job = None;
                    break;
                }
            }
        }
        if self.log.len() > MAX_LOG_LINES {
            self.log.drain(..self.log.len() - MAX_LOG_LINES);
        }
    }
}

impl eframe::App for AnyosApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain();
        if let Some(installed) = self.updater.poll() {
            match updater::relaunch(&installed) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(e) => {
                    self.updater.state = updater::State::Failed(format!(
                        "Updated to AnyOS {}, but could not restart it ({e}). Open AnyOS again to use the new version.",
                        installed.version
                    ))
                }
            }
        }

        let dropped: Option<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped {
            self.open(&p);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("AnyOS").strong());
                ui.label(RichText::new(format!("on {}", analysis::host_label(self.host))).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = self.job.is_some();
                    if ui
                        .add_enabled(!busy, egui::Button::new("Choose a program…"))
                        .clicked()
                    {
                        self.pick_file();
                    }
                    let checking = self.updater.busy();
                    if ui
                        .add_enabled(!checking && !busy, egui::Button::new("Check for updates"))
                        .on_hover_text(format!("This is AnyOS {}", anyos_update::current_version()))
                        .clicked()
                    {
                        let c = ctx.clone();
                        self.updater.check(true, move || c.request_repaint());
                    }
                });
            });
            self.update_bar(ui);
            ui.add_space(4.0);
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if hovering {
                drop_zone(ui, "Drop to open", true);
                return;
            }
            match &self.analysis {
                None => {
                    if drop_zone(ui, "Drop a program here, or click to choose one", false).clicked()
                    {
                        self.pick_file();
                    }
                }
                Some(Err(e)) => {
                    let e = e.clone();
                    ui.add_space(12.0);
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    ui.add_space(8.0);
                    if ui.button("Choose another program…").clicked() {
                        self.pick_file();
                    }
                }
                Some(Ok(_)) => self.program_view(ui),
            }
        });
    }
}

impl AnyosApp {
    /// The line under the header about updates, when there is something to say.
    fn update_bar(&mut self, ui: &mut egui::Ui) {
        use updater::State;
        let busy = self.job.is_some();
        let ctx = ui.ctx().clone();
        match &self.updater.state {
            State::Idle | State::Checking { manual: false } => {}
            State::Checking { manual: true } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking for updates…");
                });
            }
            State::UpToDate => {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "AnyOS {} is the latest version.",
                        anyos_update::current_version()
                    ));
                    if ui.small_button("OK").clicked() {
                        self.updater.dismiss();
                    }
                });
            }
            State::Available(u) => {
                let text = format!(
                    "AnyOS {} is available (this is {}).",
                    u.version,
                    anyos_update::current_version()
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).strong());
                    if ui
                        .add_enabled(!busy, egui::Button::new("Update and restart"))
                        .on_disabled_hover_text("Stop the running program first.")
                        .clicked()
                    {
                        self.updater.install(move || ctx.request_repaint());
                    }
                    if ui.button("Later").clicked() {
                        self.updater.dismiss();
                    }
                });
            }
            State::Downloading {
                version,
                done,
                total,
            } => {
                let label = format!("Downloading AnyOS {version}…");
                let bar = match total {
                    Some(t) if *t > 0 => egui::ProgressBar::new(*done as f32 / *t as f32)
                        .text(format!("{label} {}", human_size(*done))),
                    _ => egui::ProgressBar::new(0.0).animate(true).text(label),
                };
                ui.add(bar.desired_width(ui.available_width()));
            }
            State::Failed(e) => {
                let e = format!("Update failed: {e}");
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                    if ui.small_button("OK").clicked() {
                        self.updater.dismiss();
                    }
                });
            }
        }
    }

    fn program_view(&mut self, ui: &mut egui::Ui) {
        let Some(Ok(a)) = &self.analysis else { return };
        let a = a.clone();
        let ctx = ui.ctx().clone();

        ui.horizontal(|ui| {
            ui.label(RichText::new(&a.name).size(20.0).strong());
            badge(ui, &a.badge, a.tone);
        });
        ui.label(RichText::new(format!("{} · {}", a.picked.display(), human_size(a.size))).weak());
        ui.add_space(4.0);

        egui::Frame::group(ui.style())
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("facts")
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for (k, v) in &a.facts {
                            ui.label(RichText::new(*k).weak());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                ui.separator();
                ui.label(&a.plan);
                if !a.still_needed.is_empty() {
                    ui.label(
                        RichText::new(format!("Still being built: {}", a.still_needed.join(", ")))
                            .weak(),
                    );
                }
                for n in &a.notes {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {n}"));
                }
            });

        ui.add_space(4.0);
        let busy = self.job.is_some();
        let needs_cli = matches!(a.action, Action::Run | Action::Install);
        if let Action::Blocked(reason) = &a.action {
            ui.label(RichText::new(reason).strong());
        } else if needs_cli && self.cli.is_none() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "The anyos command-line tool was not found next to this app or on PATH. \
                 Put it beside the app, or set ANYOS_CLI to its path.",
            );
        } else {
            if matches!(a.action, Action::Run | Action::Launch) {
                ui.horizontal(|ui| {
                    ui.label("Arguments");
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut self.args)
                            .hint_text("optional")
                            .desired_width(f32::INFINITY),
                    );
                });
            }
            ui.horizontal(|ui| {
                let label = match a.action {
                    Action::Install => "Install",
                    _ => "Run",
                };
                let run = egui::Button::new(RichText::new(format!("▶  {label}")).size(16.0))
                    .min_size(egui::vec2(120.0, 32.0));
                if ui.add_enabled(!busy, run).clicked() {
                    self.start(&ctx);
                }
                if busy {
                    if ui.button("■  Stop").clicked() {
                        if let Some(j) = &self.job {
                            j.stop();
                        }
                    }
                    ui.spinner();
                }
                if a.action == Action::Run {
                    ui.add_enabled(
                        !busy,
                        egui::Checkbox::new(&mut self.trace, "Log Windows API calls"),
                    );
                }
            });
        }

        if !self.log.is_empty() {
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(6.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let err = ui.visuals().error_fg_color;
                            let weak = ui.visuals().weak_text_color();
                            for (s, line) in &self.log {
                                let t = RichText::new(line).monospace();
                                ui.label(match s {
                                    Stream::Out => t,
                                    Stream::Err => t.color(err),
                                    Stream::Info => t.color(weak).italics(),
                                });
                            }
                        });
                });
        }
    }
}

fn drop_zone(ui: &mut egui::Ui, text: &str, active: bool) -> egui::Response {
    let size = ui.available_size();
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let v = ui.visuals();
    let stroke_color = if active || resp.hovered() {
        v.selection.stroke.color
    } else {
        v.widgets.noninteractive.bg_stroke.color
    };
    ui.painter().rect(
        rect.shrink(8.0),
        12.0,
        if active {
            v.selection.bg_fill.gamma_multiply(0.25)
        } else {
            v.faint_bg_color
        },
        egui::Stroke::new(2.0, stroke_color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center() - egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(18.0),
        v.text_color(),
    );
    ui.painter().text(
        rect.center() + egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        "Windows .exe, Linux and macOS programs, .app bundles and installers",
        egui::FontId::proportional(13.0),
        v.weak_text_color(),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn badge(ui: &mut egui::Ui, text: &str, tone: Tone) {
    let color = match tone {
        Tone::Good => Color32::from_rgb(46, 160, 67),
        Tone::Partial => Color32::from_rgb(210, 153, 34),
        Tone::Bad => Color32::from_rgb(207, 34, 46),
    };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).strong());
        });
}

fn describe(cmd: &std::process::Command) -> String {
    let mut s = Path::new(cmd.get_program())
        .file_name()
        .unwrap_or(cmd.get_program())
        .to_string_lossy()
        .into_owned();
    for a in cmd.get_args() {
        let a = a.to_string_lossy();
        s.push(' ');
        if a.contains(' ') {
            s.push_str(&format!("\"{a}\""));
        } else {
            s.push_str(&a);
        }
    }
    s
}

fn human_size(n: u64) -> String {
    match n {
        0..1024 => format!("{n} bytes"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}
