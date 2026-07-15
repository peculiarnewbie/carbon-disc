use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};

use crate::{
    EncodeRange, EncodeRequest, EncodeSettings,
    engine::ffmpeg::FfmpegEngine,
    model::{Container, MediaInfo, Preset},
    queue::{Command, Coordinator, Event, JobSnapshot, JobState},
};

mod range_brush;
pub mod theme;

pub struct App {
    coordinator: Coordinator,
    backend: String,
    media: Option<MediaInfo>,
    probing: bool,
    probe_error: Option<String>,
    settings: EncodeSettings,
    preset: Preset,
    output: String,
    trim_start_secs: f64,
    trim_end_secs: f64,
    preview_at_secs: f64,
    preview_texture: Option<egui::TextureHandle>,
    preview_loading: bool,
    preview_error: Option<String>,
    preview_due: Option<Instant>,
    jobs: Vec<JobSnapshot>,
    selected_job: Option<u64>,
}

impl App {
    #[must_use]
    pub fn new(context: &eframe::CreationContext<'_>, initial_path: Option<PathBuf>) -> Self {
        theme::apply(&context.egui_ctx);
        let coordinator = Coordinator::spawn(Arc::new(FfmpegEngine::default()));
        let probing = initial_path.is_some();
        if let Some(path) = initial_path {
            coordinator.send(Command::Probe(path));
        }
        Self {
            coordinator,
            backend: "Detecting backend…".into(),
            media: None,
            probing,
            probe_error: None,
            settings: Preset::Balanced.settings(),
            preset: Preset::Balanced,
            output: String::new(),
            trim_start_secs: 0.0,
            trim_end_secs: 0.0,
            preview_at_secs: 0.0,
            preview_texture: None,
            preview_loading: false,
            preview_error: None,
            preview_due: None,
            jobs: Vec::new(),
            selected_job: None,
        }
    }

    fn poll(&mut self, context: &egui::Context) {
        while let Some(event) = self.coordinator.try_recv() {
            match event {
                Event::Backend(name) => self.backend = name,
                Event::ProbeStarted => {
                    self.probing = true;
                    self.probe_error = None;
                    self.preview_texture = None;
                    self.preview_error = None;
                    self.preview_due = None;
                }
                Event::ProbeFinished(result) => {
                    self.probing = false;
                    match result {
                        Ok(media) => {
                            self.trim_start_secs = 0.0;
                            self.trim_end_secs = media.duration_secs;
                            self.preview_at_secs = 0.0;
                            self.preview_due = Some(Instant::now());
                            self.output = suggested_output(&media.path, self.settings.container)
                                .display()
                                .to_string();
                            self.media = Some(media);
                        }
                        Err(error) => self.probe_error = Some(error),
                    }
                }
                Event::PreviewStarted(at_secs) => {
                    if (at_secs - self.preview_at_secs).abs() < 0.051 {
                        self.preview_loading = true;
                    }
                }
                Event::PreviewFinished { at_secs, result } => {
                    self.preview_loading = false;
                    if (at_secs - self.preview_at_secs).abs() < 0.051 {
                        match result {
                            Ok(frame) => {
                                let image = egui::ColorImage::from_rgba_unmultiplied(
                                    [frame.width as usize, frame.height as usize],
                                    &frame.rgba,
                                );
                                self.preview_texture = Some(context.load_texture(
                                    "source_preview",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                                self.preview_error = None;
                            }
                            Err(error) => self.preview_error = Some(error),
                        }
                    }
                }
                Event::Snapshot(snapshot) => self.merge_snapshot(snapshot),
            }
            context.request_repaint();
        }
    }

    fn merge_snapshot(&mut self, snapshot: Vec<JobSnapshot>) {
        // Progress events contain only the active job; full coordinator snapshots replace the list.
        if snapshot.len() == 1
            && snapshot[0].state == JobState::Running
            && let Some(job) = self.jobs.iter_mut().find(|job| job.id == snapshot[0].id)
        {
            *job = snapshot[0].clone();
            return;
        }
        self.jobs = snapshot;
    }

    fn open_source(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Video", &["mp4", "mkv", "webm", "mov", "avi", "m4v", "ts"])
            .pick_file()
        {
            self.coordinator.send(Command::Probe(path));
        }
    }

    fn add_to_queue(&mut self) {
        let Some(media) = &self.media else {
            return;
        };
        let request = EncodeRequest {
            input: media.path.clone(),
            output: PathBuf::from(&self.output),
            source_duration_secs: media.duration_secs,
            range: if self.trim_start_secs <= 0.0
                && self.trim_end_secs >= media.duration_secs - 0.05
            {
                EncodeRange::ALL
            } else {
                EncodeRange::new(self.trim_start_secs, self.trim_end_secs)
            },
            settings: self.settings.clone(),
        };
        if request.validate().is_ok() {
            self.coordinator.send(Command::Add(request));
        }
    }

    fn schedule_preview(&mut self) {
        self.preview_due = Some(Instant::now() + Duration::from_millis(180));
    }

    fn dispatch_preview_if_due(&mut self) {
        if self.preview_loading || self.preview_due.is_none_or(|due| due > Instant::now()) {
            return;
        }
        self.preview_due = None;
        if let Some(media) = &self.media {
            self.preview_loading = true;
            self.coordinator.send(Command::Preview {
                path: media.path.clone(),
                at_secs: self.preview_at_secs,
            });
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        context.request_repaint_after(std::time::Duration::from_millis(50));
        self.poll(&context);
        self.dispatch_preview_if_due();
        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("◉  CARBON DISC")
                            .strong()
                            .color(theme::VIOLET)
                            .size(17.0),
                    );
                    ui.separator();
                    if ui.button("＋ Open source").clicked() {
                        self.open_source();
                    }
                    let can_queue = self.media.is_some()
                        && !self.output.is_empty()
                        && self.trim_end_secs - self.trim_start_secs >= 0.1;
                    if ui
                        .add_enabled(can_queue, egui::Button::new("Add to queue"))
                        .clicked()
                    {
                        self.add_to_queue();
                    }
                    if ui
                        .add_enabled(
                            self.jobs.iter().any(|job| job.state == JobState::Queued),
                            egui::Button::new(RichText::new("▶ Start").color(theme::GREEN)),
                        )
                        .clicked()
                    {
                        self.coordinator.send(Command::Start);
                    }
                    if ui
                        .add_enabled(
                            self.jobs.iter().any(|job| job.state == JobState::Running),
                            egui::Button::new(RichText::new("■ Stop").color(theme::RED)),
                        )
                        .clicked()
                    {
                        self.coordinator.send(Command::Cancel);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.backend).small().color(theme::MUTED));
                    });
                });
            });

        egui::Panel::bottom("queue")
            .resizable(true)
            .default_size(190.0)
            .min_size(120.0)
            .frame(panel_frame())
            .show(root, |ui| self.queue_ui(ui));
        egui::Panel::left("source")
            .resizable(true)
            .default_size(280.0)
            .min_size(220.0)
            .frame(panel_frame())
            .show(root, |ui| self.source_ui(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_ui(ui));
            });
        if self.probing {
            egui::Window::new("Reading source")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&context, |ui| {
                    ui.spinner();
                    ui.label("Asking FFprobe for tracks and metadata…");
                });
        }
    }
}

fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::same(14))
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
}

impl App {
    fn source_ui(&mut self, ui: &mut egui::Ui) {
        heading(ui, "SOURCE");
        if let Some(error) = &self.probe_error {
            ui.colored_label(theme::RED, error);
            ui.add_space(8.0);
        }
        let Some(media) = &self.media else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Drop in a video").size(18.0));
                ui.label(
                    RichText::new("Open a file to inspect its tracks and prepare an encode.")
                        .color(theme::MUTED),
                );
                ui.add_space(12.0);
                if ui.button("Choose video…").clicked() {
                    self.open_source();
                }
            });
            return;
        };
        ui.label(
            RichText::new(media.path.file_name().unwrap_or_default().to_string_lossy())
                .strong()
                .size(16.0),
        );
        ui.label(
            RichText::new(
                media
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .display()
                    .to_string(),
            )
            .small()
            .color(theme::MUTED),
        );
        ui.add_space(14.0);
        egui::Grid::new("source_meta")
            .num_columns(2)
            .spacing([16.0, 7.0])
            .show(ui, |ui| {
                row(ui, "Duration", format_duration(media.duration_secs));
                row(
                    ui,
                    "Container",
                    media
                        .format_name
                        .split(',')
                        .next()
                        .unwrap_or("unknown")
                        .to_uppercase(),
                );
                row(ui, "Size", bytes(media.size_bytes));
                if let Some(video) = media.video_stream() {
                    row(
                        ui,
                        "Video",
                        format!(
                            "{}  {}×{}",
                            video.codec.to_uppercase(),
                            video.width.unwrap_or(0),
                            video.height.unwrap_or(0)
                        ),
                    );
                }
                row(
                    ui,
                    "Audio",
                    format!("{} track(s)", media.audio_streams().count()),
                );
            });
        ui.add_space(18.0);
        heading(ui, "TRACKS");
        for stream in &media.streams {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("#{}", stream.index));
                    ui.label(RichText::new(format!("{:?}", stream.kind)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(stream.codec.to_uppercase()).color(theme::MUTED));
                    });
                });
            });
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| { ui.heading("Encode settings"); ui.label(RichText::new("A focused first pass; advanced controls can grow behind the same engine contract.").color(theme::MUTED)); });
        ui.separator();
        ui.add_space(8.0);
        heading(ui, "PRESET");
        ui.horizontal_wrapped(|ui| {
            for preset in Preset::ALL {
                let selected = self.preset == preset;
                if ui
                    .selectable_label(selected, preset.name())
                    .on_hover_text(preset.description())
                    .clicked()
                {
                    self.preset = preset;
                    self.settings = preset.settings();
                    if let Some(media) = &self.media {
                        self.output = suggested_output(&media.path, self.settings.container)
                            .display()
                            .to_string();
                    }
                }
            }
        });
        ui.add_space(18.0);
        egui::Grid::new("settings")
            .num_columns(2)
            .min_col_width(160.0)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                ui.label("Video encoder");
                ui.label(format!("{:?}", self.settings.video_encoder));
                ui.end_row();
                ui.label("Constant quality");
                ui.add(egui::Slider::new(&mut self.settings.quality, 0..=51).text("CRF"));
                ui.end_row();
                ui.label("Encoder speed");
                ui.label(format!("{:?}", self.settings.speed));
                ui.end_row();
                ui.label("Audio");
                ui.label(format!(
                    "{:?} · {} kb/s",
                    self.settings.audio_encoder, self.settings.audio_bitrate_kbps
                ));
                ui.end_row();
                ui.label("Container");
                ui.label(format!("{:?}", self.settings.container));
                ui.end_row();
                ui.label("Resolution limit");
                ui.label(match (self.settings.max_width, self.settings.max_height) {
                    (Some(w), Some(h)) => format!("{w}×{h}"),
                    _ => "Source resolution".into(),
                });
                ui.end_row();
            });
        ui.add_space(24.0);
        heading(ui, "RANGE");
        if let Some(source_duration) = self.media.as_ref().map(|media| media.duration_secs) {
            if source_duration > 0.1 {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Encode a section").strong());
                        ui.label(
                            RichText::new(format!(
                                "{} selected",
                                format_timecode(self.trim_end_secs - self.trim_start_secs, false)
                            ))
                            .color(theme::VIOLET),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.small_button("Reset to full video").clicked() {
                                    self.trim_start_secs = 0.0;
                                    self.trim_end_secs = source_duration;
                                    self.preview_at_secs = 0.0;
                                    self.schedule_preview();
                                }
                            },
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "Preview · {}",
                                format_timecode(self.preview_at_secs, true)
                            ))
                            .strong(),
                        );
                        if self.preview_loading {
                            ui.spinner();
                            ui.label(RichText::new("Loading frame…").color(theme::MUTED));
                        }
                    });
                    if let Some(texture) = &self.preview_texture {
                        ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Image::new(texture)
                                    .max_size(egui::vec2(ui.available_width(), 240.0))
                                    .corner_radius(6),
                            );
                        });
                    } else if !self.preview_loading {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 120.0),
                            egui::Layout::centered_and_justified(egui::Direction::TopDown),
                            |ui| {
                                ui.label(
                                    RichText::new("Scrub the timeline to preview a frame")
                                        .color(theme::MUTED),
                                );
                            },
                        );
                    }
                    if let Some(error) = &self.preview_error {
                        ui.colored_label(theme::RED, error);
                    }
                    let brush_response = range_brush::show(
                        ui,
                        &mut self.trim_start_secs,
                        &mut self.trim_end_secs,
                        &mut self.preview_at_secs,
                        source_duration,
                    );
                    if brush_response.changed() {
                        self.schedule_preview();
                    }
                    let mut precise_preview = None;
                    egui::Grid::new("encode_range")
                        .num_columns(4)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Start");
                            if ui.add(
                                egui::DragValue::new(&mut self.trim_start_secs)
                                    .range(0.0..=(self.trim_end_secs - 0.1).max(0.0))
                                    .speed(0.1)
                                    .suffix(" s"),
                            ).changed() {
                                precise_preview = Some(self.trim_start_secs);
                            }
                            ui.monospace(format_timecode(self.trim_start_secs, true));
                            ui.label(RichText::new("drag left handle").small().color(theme::MUTED));
                            ui.end_row();

                            ui.label("End");
                            if ui.add(
                                egui::DragValue::new(&mut self.trim_end_secs)
                                    .range((self.trim_start_secs + 0.1)..=source_duration)
                                    .speed(0.1)
                                    .suffix(" s"),
                            ).changed() {
                                precise_preview = Some(self.trim_end_secs);
                            }
                            ui.monospace(format_timecode(self.trim_end_secs, true));
                            ui.label(RichText::new("drag right handle").small().color(theme::MUTED));
                            ui.end_row();
                        });
                    if let Some(at_secs) = precise_preview {
                        self.preview_at_secs =
                            at_secs.min((source_duration - 0.1).max(0.0));
                        self.schedule_preview();
                    }
                    ui.label(
                        RichText::new(
                            "Start and end are preserved with the queued job, so you can queue multiple clips from one source.",
                        )
                        .small()
                        .color(theme::MUTED),
                    );
                });
            } else {
                ui.label(
                    RichText::new("This source has no usable duration metadata.")
                        .color(theme::MUTED),
                );
            }
        } else {
            ui.label(RichText::new("Open a source to choose a time range.").color(theme::MUTED));
        }
        ui.add_space(24.0);
        heading(ui, "DESTINATION");
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width() - 100.0, 30.0],
                egui::TextEdit::singleline(&mut self.output).hint_text("Choose an output file"),
            );
            if ui.button("Browse…").clicked() {
                let output = Path::new(&self.output);
                let dialog = rfd::FileDialog::new()
                    .set_file_name(output.file_name().unwrap_or_default().to_string_lossy());
                if let Some(path) = dialog.save_file() {
                    self.output = path.display().to_string();
                }
            }
        });
        ui.label(RichText::new("The output is overwritten only after the job starts; FFmpeg writes directly to this path.").small().color(theme::MUTED));
    }

    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            heading(ui, &format!("QUEUE · {}", self.jobs.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear finished").clicked() {
                    self.coordinator.send(Command::ClearFinished);
                }
                if let Some(id) = self.selected_job
                    && ui.small_button("Remove").clicked()
                {
                    self.coordinator.send(Command::Remove(id));
                }
            });
        });
        ui.separator();
        if self.jobs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Your encode queue is empty").color(theme::MUTED));
            });
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for job in &self.jobs {
                let selected = self.selected_job == Some(job.id);
                let response = ui.selectable_label(
                    selected,
                    format!(
                        "{}  →  {}",
                        job.request
                            .input
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        job.request
                            .output
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                    ),
                );
                if response.clicked() {
                    self.selected_job = Some(job.id);
                }
                if job.request.range != EncodeRange::ALL {
                    ui.label(
                        RichText::new(format!(
                            "Clip {} → {} · {}",
                            format_timecode(job.request.range.start_secs, false),
                            job.request
                                .range
                                .end_secs
                                .map_or_else(|| "end".into(), |end| format_timecode(end, false)),
                            format_timecode(job.request.output_duration_secs(), false),
                        ))
                        .small()
                        .color(theme::MUTED),
                    );
                }
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(job.progress)
                            .desired_width((ui.available_width() - 150.0).max(80.0))
                            .show_percentage(),
                    );
                    ui.label(status_text(job));
                });
                if let Some(error) = &job.error {
                    ui.colored_label(theme::RED, error);
                }
                ui.add_space(5.0);
            }
        });
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().small().color(theme::MUTED));
}
fn row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.label(RichText::new(key).color(theme::MUTED));
    ui.label(value);
    ui.end_row();
}
fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total / 60 % 60,
        total % 60
    )
}

fn format_timecode(seconds: f64, precise: bool) -> String {
    let seconds = seconds.max(0.0);
    let total = seconds as u64;
    if precise {
        let millis = ((seconds.fract() * 1000.0).round() as u64).min(999);
        format!(
            "{:02}:{:02}:{:02}.{millis:03}",
            total / 3600,
            total / 60 % 60,
            total % 60
        )
    } else {
        format!(
            "{:02}:{:02}:{:02}",
            total / 3600,
            total / 60 % 60,
            total % 60
        )
    }
}
fn bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut n = value as f64;
    let mut unit = 0;
    while n >= 1000.0 && unit < UNITS.len() - 1 {
        n /= 1000.0;
        unit += 1;
    }
    format!("{n:.1} {}", UNITS[unit])
}
fn suggested_output(input: &Path, container: Container) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}-encoded.{}", container.extension()))
}
fn status_text(job: &JobSnapshot) -> RichText {
    let text = match job.state {
        JobState::Queued => "Queued".into(),
        JobState::Running => job
            .speed
            .map_or_else(|| "Encoding".into(), |speed| format!("{speed:.2}×")),
        JobState::Complete => "Complete".into(),
        JobState::Failed => "Failed".into(),
        JobState::Cancelled => "Cancelled".into(),
    };
    let color = match job.state {
        JobState::Complete => theme::GREEN,
        JobState::Failed | JobState::Cancelled => theme::RED,
        JobState::Running => theme::VIOLET,
        JobState::Queued => Color32::GRAY,
    };
    RichText::new(text).color(color)
}
