//! K13 spike: a tiny egui summoner backed by the daemon.
//!
//! Run with:
//!   cargo run --features quick-ui --bin mnemo-quick
//!
//! This is intentionally not wired into the product. It measures the native
//! window's resident set and daemon round-trip so we can compare against the
//! Tauri window before committing to a second UI toolkit.

use eframe::egui;
use std::time::Instant;

fn rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
            return 0;
        };
        statm
            .split_whitespace()
            .nth(1)
            .and_then(|pages| pages.parse::<u64>().ok())
            .map(|pages| pages.saturating_mul(4096))
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

struct QuickApp {
    query: String,
    results: Vec<(String, f64)>,
    error: Option<String>,
    last_round_trip_ms: Option<f64>,
    focused: bool,
    started: Instant,
    hidden: bool,
    first_ui: bool,
    searched: bool,
}

impl Default for QuickApp {
    fn default() -> Self {
        Self {
            query: String::new(),
            results: Vec::new(),
            error: None,
            last_round_trip_ms: None,
            focused: false,
            started: Instant::now(),
            hidden: false,
            first_ui: false,
            searched: false,
        }
    }
}

impl QuickApp {
    fn search(&mut self) {
        let query = self.query.trim().to_string();
        if query.is_empty() {
            self.results.clear();
            return;
        }
        let started = Instant::now();
        let client = mnemo_lib::daemon::DaemonClient;
        let outcome = client.search_page(query, 12, 0, false);
        self.last_round_trip_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        match outcome {
            Ok(response) => {
                self.results = response
                    .results
                    .into_iter()
                    .map(|result| (result.path, result.score))
                    .collect();
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
                self.results.clear();
            }
        }
    }
}

impl eframe::App for QuickApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.first_ui {
            self.first_ui = true;
            eprintln!(
                "K13 quick first_ui_ms={:.1}",
                self.started.elapsed().as_secs_f64() * 1000.0
            );
        }
        if !self.searched && self.started.elapsed().as_secs_f64() >= 0.5 {
            self.searched = true;
            self.query = "test".to_string();
            self.search();
            eprintln!(
                "K13 quick daemon_round_trip_ms={:.2}",
                self.last_round_trip_ms.unwrap_or(0.0)
            );
        }
        if !self.focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.focused = true;
        }

        let should_hide = !self.hidden
            && (self.started.elapsed().as_secs_f64() >= 2.0
                || ctx.input(|input| input.key_pressed(egui::Key::Escape)));
        if should_hide {
            self.hidden = true;
            eprintln!("K13 quick visible rss_bytes={}", rss_bytes());
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        egui::Frame::new()
            .fill(egui::Color32::from_rgb(24, 24, 27))
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::from_rgb(230, 230, 235));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search files…")
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Heading),
                );
                if self.focused {
                    response.request_focus();
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.search();
                }

                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(240, 140, 120), error);
                }

                if let Some(ms) = self.last_round_trip_ms {
                    ui.label(format!("daemon round-trip: {ms:.2} ms"));
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (path, score) in &self.results {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("{score:.2}"));
                            ui.label(path);
                        });
                    }
                });
            });
    }
}

fn main() -> eframe::Result {
    eprintln!("K13 quick startup rss_bytes={}", rss_bytes());
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        eprintln!("K13 quick hidden rss_bytes={}", rss_bytes());
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 420.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native(
        "Mnemo Quick",
        options,
        Box::new(|_cc| Ok(Box::new(QuickApp::default()))),
    )
}
