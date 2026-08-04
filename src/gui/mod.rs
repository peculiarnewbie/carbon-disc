use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};

use crate::{
    AudioTrackSettings, CustomPreset, EncodeRange, EncodeRequest, EncodeSettings,
    engine::ffmpeg::FfmpegEngine,
    model::{AudioEncoder, Container, EngineCapabilities, MediaInfo, Preset},
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
    preview_fast_seek: bool,
    next_preview_id: u64,
    active_preview_id: Option<u64>,
    selected_audio_tracks: BTreeSet<u32>,
    pending_overwrite: Option<EncodeRequest>,
    request_error: Option<String>,
    jobs: Vec<JobSnapshot>,
    selected_job: Option<u64>,
    capabilities: Option<EngineCapabilities>,
    capability_error: Option<String>,
    custom_presets: Vec<CustomPreset>,
    selected_custom_preset: Option<String>,
    show_save_preset: bool,
    preset_name_input: String,
    preset_description_input: String,
    batch_status: Option<String>,
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
        let (custom_presets, request_error) = match crate::persistence::load_presets() {
            Ok(presets) => (presets, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Could not load custom presets: {error}")),
            ),
        };
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
            preview_fast_seek: false,
            next_preview_id: 1,
            active_preview_id: None,
            selected_audio_tracks: BTreeSet::new(),
            pending_overwrite: None,
            request_error,
            jobs: Vec::new(),
            selected_job: None,
            capabilities: None,
            capability_error: None,
            custom_presets,
            selected_custom_preset: None,
            show_save_preset: false,
            preset_name_input: String::new(),
            preset_description_input: String::new(),
            batch_status: None,
        }
    }

    fn poll(&mut self, context: &egui::Context) {
        while let Some(event) = self.coordinator.try_recv() {
            match event {
                Event::Backend(name) => self.backend = name,
                Event::ProbeStarted => {
                    self.probing = true;
                    self.probe_error = None;
                    self.next_preview_id += 1;
                    self.active_preview_id = None;
                    self.preview_loading = false;
                    self.preview_texture = None;
                    self.preview_error = None;
                    self.preview_due = None;
                }
                Event::ProbeFinished(result) => {
                    self.probing = false;
                    match result {
                        Ok(media) => {
                            self.selected_audio_tracks.clear();
                            if let Some(first_audio) = media.audio_streams().next() {
                                self.selected_audio_tracks.insert(first_audio.index);
                            }
                            self.trim_start_secs = 0.0;
                            self.trim_end_secs = media.duration_secs;
                            self.preview_at_secs = 0.0;
                            self.preview_fast_seek = false;
                            self.preview_due = Some(Instant::now());
                            self.output = suggested_output(&media.path, self.settings.container)
                                .display()
                                .to_string();
                            self.media = Some(media);
                        }
                        Err(error) => self.probe_error = Some(error),
                    }
                }
                Event::PreviewStarted { id, at_secs } => {
                    if self.active_preview_id == Some(id)
                        && (at_secs - self.preview_at_secs).abs() < 0.051
                    {
                        self.preview_loading = true;
                    }
                }
                Event::PreviewFinished {
                    id,
                    at_secs,
                    result,
                } => {
                    if self.active_preview_id == Some(id) {
                        self.preview_loading = false;
                        self.active_preview_id = None;
                    }
                    if self.active_preview_id.is_none()
                        && id + 1 == self.next_preview_id
                        && (at_secs - self.preview_at_secs).abs() < 0.051
                    {
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
                Event::Capabilities(result) => match result {
                    Ok(capabilities) => {
                        self.capabilities = Some(capabilities);
                        self.capability_error = None;
                    }
                    Err(error) => self.capability_error = Some(error),
                },
                Event::BatchProgress { current, total } => {
                    self.batch_status = Some(format!("Importing {current} of {total}…"));
                }
                Event::BatchFinished { added, failed } => {
                    self.batch_status = Some(if failed == 0 {
                        format!("Added {added} files to the queue")
                    } else {
                        format!("Added {added} files; skipped {failed}")
                    });
                }
                Event::Notice(message) => self.request_error = Some(message),
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

    fn add_batch_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Video",
                &[
                    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "mts", "m2ts", "mpg",
                ],
            )
            .pick_files()
        {
            self.batch_status = Some(format!("Preparing {} files…", paths.len()));
            self.coordinator.send(Command::BatchFiles {
                paths,
                settings: self.settings.clone(),
            });
        }
    }

    fn add_batch_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.batch_status = Some("Scanning folder…".into());
            self.coordinator.send(Command::BatchFolder {
                path,
                settings: self.settings.clone(),
            });
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
            source_width: media.video_stream().and_then(|stream| stream.width),
            source_height: media.video_stream().and_then(|stream| stream.height),
            range: if self.trim_start_secs <= 0.0
                && self.trim_end_secs >= media.duration_secs - 0.05
            {
                EncodeRange::ALL
            } else {
                EncodeRange::new(self.trim_start_secs, self.trim_end_secs)
            },
            audio_tracks: self
                .selected_audio_tracks
                .iter()
                .map(|&source_index| AudioTrackSettings {
                    source_index,
                    encoder: self.settings.audio_encoder,
                    bitrate_kbps: self.settings.audio_bitrate_kbps,
                })
                .collect(),
            overwrite: false,
            settings: self.settings.clone(),
        };
        if let Err(error) = request.validate() {
            self.request_error = Some(error.to_string());
        } else if request.output.exists() {
            self.pending_overwrite = Some(request);
        } else {
            self.request_error = None;
            self.coordinator.send(Command::Add(request));
        }
    }

    fn schedule_preview(&mut self, fast_seek: bool) {
        self.preview_fast_seek = fast_seek;
        let delay = if fast_seek { 35 } else { 80 };
        self.preview_due = Some(Instant::now() + Duration::from_millis(delay));
    }

    fn dispatch_preview_if_due(&mut self) {
        if self.preview_due.is_none_or(|due| due > Instant::now()) {
            return;
        }
        self.preview_due = None;
        if let Some(media) = &self.media {
            let id = self.next_preview_id;
            self.next_preview_id += 1;
            self.active_preview_id = Some(id);
            self.preview_loading = true;
            self.coordinator.send(Command::Preview {
                id,
                path: media.path.clone(),
                at_secs: self.preview_at_secs,
                fast_seek: self.preview_fast_seek,
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
                    let encoder_available =
                        self.capabilities.as_ref().is_some_and(|capabilities| {
                            capabilities
                                .video_encoders
                                .contains(&self.settings.video_encoder)
                        });
                    if ui
                        .add_enabled(encoder_available, egui::Button::new("Batch files…"))
                        .clicked()
                    {
                        self.add_batch_files();
                    }
                    if ui
                        .add_enabled(encoder_available, egui::Button::new("Folder…"))
                        .clicked()
                    {
                        self.add_batch_folder();
                    }
                    let can_queue = self.media.is_some()
                        && !self.output.is_empty()
                        && self.trim_end_secs - self.trim_start_secs >= 0.1
                        && encoder_available;
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
                if let Some(status) = &self.batch_status {
                    ui.label(RichText::new(status).small().color(theme::MUTED));
                }
            });

        egui::Panel::right("queue")
            .resizable(true)
            .default_size(320.0)
            .min_size(240.0)
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
        self.overwrite_modal(&context);
        self.preset_modal(&context);
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
                let selected = self.selected_custom_preset.is_none() && self.preset == preset;
                if ui
                    .selectable_label(selected, preset.name())
                    .on_hover_text(preset.description())
                    .clicked()
                {
                    self.preset = preset;
                    self.selected_custom_preset = None;
                    self.settings = preset.settings();
                    if let Some(media) = &self.media {
                        self.output = suggested_output(&media.path, self.settings.container)
                            .display()
                            .to_string();
                    }
                }
            }
        });
        self.custom_presets_ui(ui);
        ui.add_space(18.0);
        egui::Grid::new("settings")
            .num_columns(2)
            .min_col_width(160.0)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                ui.label("Video encoder");
                if let Some(capabilities) = &self.capabilities {
                    egui::ComboBox::from_id_salt("video_encoder")
                        .selected_text(self.settings.video_encoder.display_name())
                        .show_ui(ui, |ui| {
                            for encoder in &capabilities.video_encoders {
                                let label = if encoder.is_hardware() {
                                    format!("⚡ {}", encoder.display_name())
                                } else {
                                    encoder.display_name().to_owned()
                                };
                                ui.selectable_value(
                                    &mut self.settings.video_encoder,
                                    *encoder,
                                    label,
                                );
                            }
                        });
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Detecting available encoders…");
                    });
                }
                ui.end_row();
                ui.label("Quality value");
                ui.add(egui::Slider::new(&mut self.settings.quality, 0..=51));
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
        if let Some(error) = &self.capability_error {
            ui.colored_label(theme::RED, format!("Encoder detection failed: {error}"));
        } else if let Some(capabilities) = &self.capabilities
            && !capabilities
                .video_encoders
                .contains(&self.settings.video_encoder)
        {
            ui.colored_label(
                theme::RED,
                "This preset's video encoder is unavailable on this machine.",
            );
        }
        ui.add_space(24.0);
        self.audio_ui(ui);
        ui.add_space(24.0);
        self.picture_ui(ui);
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
                                    self.schedule_preview(false);
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
                    let crop_overlay = self.media.as_ref().and_then(|media| {
                        let video = media.video_stream()?;
                        Some((video.width?, video.height?, self.settings.crop))
                    });
                    if let Some(texture) = &self.preview_texture {
                        ui.vertical_centered(|ui| {
                            let response = ui.add(
                                egui::Image::new(texture)
                                    .max_size(egui::vec2(ui.available_width(), 240.0))
                                    .corner_radius(6),
                            );
                            if let Some((width, height, crop)) = crop_overlay {
                                paint_crop_overlay(ui, response.rect, width, height, crop);
                            }
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
                        self.schedule_preview(brush_response.dragged());
                    }
                    if brush_response.drag_stopped() {
                        self.schedule_preview(false);
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
                        self.schedule_preview(false);
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
        ui.label(RichText::new("Encodes are written to a temporary file beside the destination and committed only after FFmpeg succeeds.").small().color(theme::MUTED));
        if let Some(error) = &self.request_error {
            ui.colored_label(theme::RED, error);
        }
    }

    fn custom_presets_ui(&mut self, ui: &mut egui::Ui) {
        let mut selected_settings = None;
        let mut delete_selected = false;
        ui.horizontal_wrapped(|ui| {
            for preset in &self.custom_presets {
                if ui
                    .selectable_label(
                        self.selected_custom_preset.as_deref() == Some(&preset.name),
                        &preset.name,
                    )
                    .on_hover_text(&preset.description)
                    .clicked()
                {
                    selected_settings = Some((preset.name.clone(), preset.settings.clone()));
                }
            }
            if ui.small_button("＋ Save current…").clicked() {
                self.preset_name_input = self.selected_custom_preset.clone().unwrap_or_default();
                self.preset_description_input = self
                    .custom_presets
                    .iter()
                    .find(|preset| Some(&preset.name) == self.selected_custom_preset.as_ref())
                    .map_or_else(String::new, |preset| preset.description.clone());
                self.show_save_preset = true;
            }
            if ui
                .add_enabled(
                    self.selected_custom_preset.is_some(),
                    egui::Button::new("Delete"),
                )
                .clicked()
            {
                delete_selected = true;
            }
            if ui.small_button("Import…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Carbon Disc presets", &["json"])
                    .pick_file()
            {
                match crate::persistence::import_presets(&path) {
                    Ok(imported) => {
                        for preset in imported {
                            if let Some(existing) = self
                                .custom_presets
                                .iter_mut()
                                .find(|existing| existing.name == preset.name)
                            {
                                *existing = preset;
                            } else {
                                self.custom_presets.push(preset);
                            }
                        }
                        self.save_custom_presets();
                    }
                    Err(error) => self.request_error = Some(format!("Import failed: {error}")),
                }
            }
            if ui
                .add_enabled(
                    !self.custom_presets.is_empty(),
                    egui::Button::new("Export…"),
                )
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Carbon Disc presets", &["json"])
                    .set_file_name("carbon-disc-presets.json")
                    .save_file()
                && let Err(error) = crate::persistence::export_presets(&path, &self.custom_presets)
            {
                self.request_error = Some(format!("Export failed: {error}"));
            }
        });

        if let Some((name, settings)) = selected_settings {
            self.selected_custom_preset = Some(name);
            self.settings = settings;
            if let Some(media) = &self.media {
                self.output = suggested_output(&media.path, self.settings.container)
                    .display()
                    .to_string();
            }
        }
        if delete_selected && let Some(name) = self.selected_custom_preset.take() {
            self.custom_presets.retain(|preset| preset.name != name);
            self.save_custom_presets();
        }
    }

    fn preset_modal(&mut self, context: &egui::Context) {
        if !self.show_save_preset {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Save custom preset")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(context, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut self.preset_name_input);
                ui.label("Description");
                ui.text_edit_singleline(&mut self.preset_description_input);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add_enabled(
                            !self.preset_name_input.trim().is_empty(),
                            egui::Button::new("Save preset"),
                        )
                        .clicked()
                    {
                        save = true;
                    }
                });
            });
        if cancel {
            self.show_save_preset = false;
        } else if save {
            let name = self.preset_name_input.trim().to_owned();
            let preset = CustomPreset {
                name: name.clone(),
                description: self.preset_description_input.trim().to_owned(),
                settings: self.settings.clone(),
            };
            if let Some(existing) = self
                .custom_presets
                .iter_mut()
                .find(|existing| existing.name == name)
            {
                *existing = preset;
            } else {
                self.custom_presets.push(preset);
            }
            self.selected_custom_preset = Some(name);
            self.show_save_preset = false;
            self.save_custom_presets();
        }
    }

    fn save_custom_presets(&mut self) {
        match crate::persistence::save_presets(&self.custom_presets) {
            Ok(()) => self.request_error = None,
            Err(error) => self.request_error = Some(format!("Could not save presets: {error}")),
        }
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui) {
        heading(ui, "AUDIO TRACKS");
        let tracks: Vec<_> = self
            .media
            .as_ref()
            .map(|media| media.audio_streams().cloned().collect())
            .unwrap_or_default();
        if tracks.is_empty() {
            ui.label(RichText::new("This source has no audio tracks.").color(theme::MUTED));
            return;
        }

        ui.horizontal(|ui| {
            if ui.small_button("Select all").clicked() {
                self.selected_audio_tracks
                    .extend(tracks.iter().map(|track| track.index));
            }
            if ui.small_button("Select none").clicked() {
                self.selected_audio_tracks.clear();
            }
            ui.label(
                RichText::new(format!(
                    "{} of {} selected",
                    self.selected_audio_tracks.len(),
                    tracks.len()
                ))
                .color(theme::MUTED),
            );
        });
        for track in tracks {
            let mut selected = self.selected_audio_tracks.contains(&track.index);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut selected, "");
                    ui.label(RichText::new(format!("Track {}", track.index)).strong());
                    ui.label(track.language.as_deref().unwrap_or("und").to_uppercase());
                    if let Some(title) = &track.title {
                        ui.label(RichText::new(title).color(theme::MUTED));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} · {} ch",
                                track.codec.to_uppercase(),
                                track.channels.unwrap_or(0)
                            ))
                            .color(theme::MUTED),
                        );
                    });
                });
            });
            if selected {
                self.selected_audio_tracks.insert(track.index);
            } else {
                self.selected_audio_tracks.remove(&track.index);
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Selected tracks use");
            egui::ComboBox::from_id_salt("audio_encoder")
                .selected_text(format!("{:?}", self.settings.audio_encoder))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.audio_encoder, AudioEncoder::Aac, "AAC");
                    ui.selectable_value(
                        &mut self.settings.audio_encoder,
                        AudioEncoder::Opus,
                        "Opus",
                    );
                    ui.selectable_value(
                        &mut self.settings.audio_encoder,
                        AudioEncoder::Copy,
                        "Passthrough",
                    );
                });
            ui.add_enabled(
                self.settings.audio_encoder != AudioEncoder::Copy,
                egui::DragValue::new(&mut self.settings.audio_bitrate_kbps)
                    .range(32..=1024)
                    .speed(16)
                    .suffix(" kb/s"),
            );
        });
        if self.selected_audio_tracks.is_empty() {
            ui.label(RichText::new("The queued output will be video-only.").color(theme::MUTED));
        }
    }

    fn picture_ui(&mut self, ui: &mut egui::Ui) {
        heading(ui, "DIMENSIONS & CROP");
        let Some((source_width, source_height)) = self.media.as_ref().and_then(|media| {
            let video = media.video_stream()?;
            Some((video.width?, video.height?))
        }) else {
            ui.label(
                RichText::new("Open a source to adjust picture dimensions.").color(theme::MUTED),
            );
            return;
        };

        ui.group(|ui| {
            let mut limited = self.settings.max_width.is_some() || self.settings.max_height.is_some();
            if ui.checkbox(&mut limited, "Limit output resolution").changed() {
                if limited {
                    self.settings.max_width = Some(source_width);
                    self.settings.max_height = Some(source_height);
                } else {
                    self.settings.max_width = None;
                    self.settings.max_height = None;
                }
            }
            if limited {
                let mut width = self.settings.max_width.unwrap_or(source_width);
                let mut height = self.settings.max_height.unwrap_or(source_height);
                ui.horizontal(|ui| {
                    ui.label("Maximum");
                    ui.add(
                        egui::DragValue::new(&mut width)
                            .range(16..=source_width)
                            .speed(2)
                            .suffix(" px wide"),
                    );
                    ui.label("×");
                    ui.add(
                        egui::DragValue::new(&mut height)
                            .range(16..=source_height)
                            .speed(2)
                            .suffix(" px high"),
                    );
                });
                self.settings.max_width = Some(width.max(16) & !1);
                self.settings.max_height = Some(height.max(16) & !1);
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Crop margins").strong());
                if ui.small_button("Reset crop").clicked() {
                    self.settings.crop = crate::CropMargins::ZERO;
                }
            });
            let crop = &mut self.settings.crop;
            egui::Grid::new("crop_margins")
                .num_columns(4)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Top");
                    ui.add(
                        egui::DragValue::new(&mut crop.top)
                            .range(0..=source_height.saturating_sub(crop.bottom + 16))
                            .speed(2),
                    );
                    ui.label("Bottom");
                    ui.add(
                        egui::DragValue::new(&mut crop.bottom)
                            .range(0..=source_height.saturating_sub(crop.top + 16))
                            .speed(2),
                    );
                    ui.end_row();
                    ui.label("Left");
                    ui.add(
                        egui::DragValue::new(&mut crop.left)
                            .range(0..=source_width.saturating_sub(crop.right + 16))
                            .speed(2),
                    );
                    ui.label("Right");
                    ui.add(
                        egui::DragValue::new(&mut crop.right)
                            .range(0..=source_width.saturating_sub(crop.left + 16))
                            .speed(2),
                    );
                    ui.end_row();
                });
            crop.top &= !1;
            crop.bottom &= !1;
            crop.left &= !1;
            crop.right &= !1;

            let cropped_width = source_width.saturating_sub(crop.left + crop.right).max(2);
            let cropped_height = source_height.saturating_sub(crop.top + crop.bottom).max(2);
            let (output_width, output_height) = scaled_dimensions(
                cropped_width,
                cropped_height,
                self.settings.max_width,
                self.settings.max_height,
            );
            ui.label(
                RichText::new(format!(
                    "Source {source_width}×{source_height} · Cropped {cropped_width}×{cropped_height} · Output {output_width}×{output_height}"
                ))
                .small()
                .color(theme::MUTED),
            );
        });
    }

    fn overwrite_modal(&mut self, context: &egui::Context) {
        let Some(path) = self
            .pending_overwrite
            .as_ref()
            .map(|request| request.output.clone())
        else {
            return;
        };
        let mut replace = false;
        let mut cancel = false;
        egui::Window::new("Replace existing output?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(context, |ui| {
                ui.label("A file already exists at this destination:");
                ui.monospace(path.display().to_string());
                ui.label(
                    RichText::new(
                        "The existing file is kept until the new encode completes successfully.",
                    )
                    .color(theme::MUTED),
                );
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .button(RichText::new("Replace after encoding").color(theme::RED))
                        .clicked()
                    {
                        replace = true;
                    }
                });
            });
        if cancel {
            self.pending_overwrite = None;
        } else if replace && let Some(mut request) = self.pending_overwrite.take() {
            request.overwrite = true;
            self.request_error = None;
            self.coordinator.send(Command::Add(request));
        }
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

fn scaled_dimensions(
    width: u32,
    height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
) -> (u32, u32) {
    let width_scale = max_width.map_or(1.0, |limit| f64::from(limit) / f64::from(width));
    let height_scale = max_height.map_or(1.0, |limit| f64::from(limit) / f64::from(height));
    let scale = width_scale.min(height_scale).min(1.0);
    let output_width = ((f64::from(width) * scale).round() as u32).max(2) & !1;
    let output_height = ((f64::from(height) * scale).round() as u32).max(2) & !1;
    (output_width, output_height)
}

fn paint_crop_overlay(
    ui: &egui::Ui,
    rect: egui::Rect,
    source_width: u32,
    source_height: u32,
    crop: crate::CropMargins,
) {
    if crop.is_zero() || source_width == 0 || source_height == 0 {
        return;
    }
    let left = rect.left() + rect.width() * crop.left as f32 / source_width as f32;
    let right = rect.right() - rect.width() * crop.right as f32 / source_width as f32;
    let top = rect.top() + rect.height() * crop.top as f32 / source_height as f32;
    let bottom = rect.bottom() - rect.height() * crop.bottom as f32 / source_height as f32;
    let shade = Color32::from_black_alpha(180);
    let painter = ui.painter();
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), top)),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.left(), bottom), rect.max),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.left(), top), egui::pos2(left, bottom)),
        0.0,
        shade,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(right, top), egui::pos2(rect.right(), bottom)),
        0.0,
        shade,
    );
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
