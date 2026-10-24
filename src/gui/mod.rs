use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};

use crate::{
    AudioTrackSettings, CropMargins, CustomPreset, EncodeRange, EncodeRequest, EncodeSettings,
    SizeDerivation, SizeMeasurement, SubtitleSelection,
    engine::ffmpeg::FfmpegEngine,
    model::{
        AudioEncoder, Container, EncoderSpeed, EngineCapabilities, MediaInfo, Preset, VideoEncoder,
    },
    persistence::Session,
    queue::{Command, Coordinator, Event, JobId, JobSnapshot, JobState},
    resolve_subtitles, size_derivation,
};

mod crop_editor;
mod handbrake;
mod range_brush;
pub mod theme;
mod widgets;

use widgets::Icon;

const VIDEO_EXTENSIONS: [&str; 10] = [
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "mts", "m2ts", "mpg",
];
const RESOLUTION_LIMITS: [(&str, u32, u32); 5] = [
    ("2160p (4K)", 3840, 2160),
    ("1440p", 2560, 1440),
    ("1080p", 1920, 1080),
    ("720p", 1280, 720),
    ("480p", 854, 480),
];
const AUDIO_BITRATES: [u16; 9] = [64, 96, 128, 160, 192, 256, 320, 384, 512];
const FRAME_RATE_CHOICES: [u32; 6] = [24, 25, 30, 48, 50, 60];
/// Fallback pool for the language picker when no file is open to read tags from.
const COMMON_LANGUAGES: [&str; 16] = [
    "eng", "fra", "spa", "deu", "ita", "jpn", "kor", "zho", "rus", "por", "hin", "ara", "tur",
    "pol", "nld", "swe",
];
const ESTIMATE_DEBOUNCE: Duration = Duration::from_millis(1500);
const PRIMARY_BUTTON_SIZE: egui::Vec2 = egui::vec2(150.0, 34.0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Video,
    Audio,
    Subtitles,
    Picture,
}

enum Status {
    Info(String),
    Error(String),
}

/// State of the pre-encode size estimate for the current settings.
enum SizeEstimateState {
    Sampling { id: u64, fraction: f32 },
    Measured,
    Failed(String),
}

/// What the estimate readout in the preset row renders.
struct EstimateChip {
    label: Option<RichText>,
    tooltip: Option<String>,
    spinner: bool,
}

/// A job waiting on the "replace existing file?" confirmation.
struct PendingAdd {
    request: EncodeRequest,
    start: bool,
    replaces: Option<JobId>,
}

enum JobAction {
    Remove(JobId),
    Retry(JobId),
    Reveal(PathBuf),
}

pub struct App {
    coordinator: Coordinator,
    backend: String,
    media: Option<MediaInfo>,
    probing: bool,
    probe_error: Option<String>,
    settings: EncodeSettings,
    preset: Preset,
    selected_custom_preset: Option<String>,
    custom_presets: Vec<CustomPreset>,
    output: String,
    output_dir: Option<PathBuf>,
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
    size_estimate: Option<SizeEstimateState>,
    size_measurement: Option<SizeMeasurement>,
    size_measurement_request: Option<EncodeRequest>,
    estimate_due: Option<Instant>,
    next_estimate_id: u64,
    selected_audio_tracks: BTreeSet<u32>,
    selected_subtitle_tracks: BTreeSet<u32>,
    available_language_pick: Option<String>,
    selected_language_pick: Option<String>,
    pending_add: Option<PendingAdd>,
    status: Option<Status>,
    jobs: Vec<JobSnapshot>,
    capabilities: Option<EngineCapabilities>,
    capability_error: Option<String>,
    tools_missing: bool,
    tools_path: Option<String>,
    checking_tools: bool,
    detecting_crop: bool,
    show_save_preset: bool,
    preset_name_input: String,
    preset_description_input: String,
    more_preset: Option<String>,
    more_presets_open: bool,
    preset_search: String,
    preset_search_focus: bool,
    tab: Tab,
    confirm_close: bool,
    allow_close: bool,
    window_title: String,
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
        let mut status = None;
        let custom_presets = crate::persistence::load_presets().unwrap_or_else(|error| {
            status = Some(Status::Error(format!(
                "Could not load custom presets: {error}"
            )));
            Vec::new()
        });
        let session = crate::persistence::load_session().unwrap_or_else(|error| {
            status = Some(Status::Error(format!(
                "Could not restore last session: {error}"
            )));
            None
        });
        let session = session.unwrap_or_else(|| Session {
            settings: Preset::Balanced.settings(),
            preset: Preset::Balanced,
            custom_preset: None,
            more_preset: None,
            output_dir: None,
        });
        let mut settings = session.settings;
        // Crop belongs to a particular source, so it never carries over between launches.
        settings.crop = CropMargins::ZERO;
        let selected_custom_preset = session
            .custom_preset
            .filter(|name| custom_presets.iter().any(|preset| &preset.name == name));
        Self {
            coordinator,
            backend: "Detecting backend…".into(),
            media: None,
            probing,
            probe_error: None,
            settings,
            preset: session.preset,
            selected_custom_preset,
            custom_presets,
            output: String::new(),
            output_dir: session.output_dir,
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
            size_estimate: None,
            size_measurement: None,
            size_measurement_request: None,
            estimate_due: None,
            next_estimate_id: 1,
            selected_audio_tracks: BTreeSet::new(),
            selected_subtitle_tracks: BTreeSet::new(),
            available_language_pick: None,
            selected_language_pick: None,
            pending_add: None,
            status,
            jobs: Vec::new(),
            capabilities: None,
            capability_error: None,
            tools_missing: false,
            tools_path: None,
            checking_tools: false,
            detecting_crop: false,
            show_save_preset: false,
            preset_name_input: String::new(),
            preset_description_input: String::new(),
            more_preset: session
                .more_preset
                .filter(|name| handbrake::find(name).is_some()),
            more_presets_open: false,
            preset_search: String::new(),
            preset_search_focus: false,
            tab: Tab::Video,
            confirm_close: false,
            allow_close: false,
            window_title: String::new(),
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
                    self.size_estimate = None;
                    self.size_measurement = None;
                    self.size_measurement_request = None;
                }
                Event::ProbeFinished(result) => {
                    self.probing = false;
                    match result {
                        Ok(media) => {
                            self.selected_audio_tracks.clear();
                            if let Some(first_audio) = media.audio_streams().next() {
                                self.selected_audio_tracks.insert(first_audio.index);
                            }
                            self.selected_subtitle_tracks.clear();
                            self.available_language_pick = None;
                            self.selected_language_pick = None;
                            self.trim_start_secs = 0.0;
                            self.trim_end_secs = media.duration_secs;
                            self.preview_at_secs = 0.0;
                            self.preview_fast_seek = false;
                            self.preview_due = Some(Instant::now());
                            self.settings.crop = CropMargins::ZERO;
                            self.detecting_crop = false;
                            self.output = self
                                .suggested_output(&media.path, None)
                                .display()
                                .to_string();
                            self.media = Some(media);
                            self.estimate_due = Some(Instant::now() + ESTIMATE_DEBOUNCE);
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
                Event::CropDetected(result) => {
                    self.detecting_crop = false;
                    match result {
                        Ok(crop) if crop.is_zero() => {
                            self.status = Some(Status::Info("No black bars found".into()));
                        }
                        Ok(crop) => {
                            self.settings.crop = crop;
                            self.status = Some(Status::Info(format!(
                                "Cropped black bars: {} top, {} bottom, {} left, {} right",
                                crop.top, crop.bottom, crop.left, crop.right
                            )));
                        }
                        Err(error) => {
                            self.status = Some(Status::Error(format!(
                                "Could not detect black bars: {error}"
                            )));
                        }
                    }
                }
                Event::Capabilities(result) => match result {
                    Ok(capabilities) => {
                        self.capabilities = Some(capabilities);
                        self.capability_error = None;
                        self.tools_missing = false;
                        self.tools_path = None;
                        self.checking_tools = false;
                    }
                    Err(error) => {
                        self.capability_error = Some(error);
                        self.checking_tools = false;
                    }
                },
                Event::ToolsMissing { program } => {
                    self.tools_missing = true;
                    self.checking_tools = false;
                    self.tools_path = Some(program);
                }
                Event::SizeEstimateProgress { id, fraction } => {
                    if let Some(SizeEstimateState::Sampling {
                        id: active,
                        fraction: shown,
                    }) = &mut self.size_estimate
                        && *active == id
                    {
                        *shown = fraction;
                    }
                }
                Event::SizeEstimateFinished { id, result } => {
                    if matches!(
                        &self.size_estimate,
                        Some(SizeEstimateState::Sampling { id: active, .. }) if *active == id
                    ) {
                        match result {
                            Ok(measurement) => {
                                self.size_measurement = Some(measurement);
                                self.size_estimate = Some(SizeEstimateState::Measured);
                            }
                            Err(message) => {
                                self.size_estimate = Some(SizeEstimateState::Failed(message));
                            }
                        }
                    }
                }
                Event::BatchProgress { current, total } => {
                    self.status = Some(Status::Info(format!("Reading {current} of {total}…")));
                }
                Event::BatchFinished { added, failed } => {
                    self.status = Some(if failed == 0 {
                        Status::Info(format!("Queued {added} {}", plural(added, "file")))
                    } else {
                        Status::Error(format!(
                            "Queued {added} {}; {failed} could not be read",
                            plural(added, "file")
                        ))
                    });
                }
                Event::Notice(message) => self.status = Some(Status::Error(message)),
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

    fn session(&self) -> Session {
        let mut settings = self.settings.clone();
        settings.crop = CropMargins::ZERO;
        Session {
            settings,
            preset: self.preset,
            custom_preset: self.selected_custom_preset.clone(),
            more_preset: self.more_preset.clone(),
            output_dir: self.output_dir.clone(),
        }
    }

    fn encoder_available(&self) -> bool {
        self.capabilities.as_ref().is_some_and(|capabilities| {
            capabilities
                .video_encoders
                .contains(&self.settings.video_encoder)
        })
    }

    /// Why the batch buttons are disabled, if they are.
    fn batch_blocker(&self) -> Option<&'static str> {
        if self.tools_missing {
            Some("Install FFmpeg first — see the setup screen.")
        } else if self.capabilities.is_none() {
            Some("Still detecting which encoders this machine supports…")
        } else if !self.encoder_available() {
            Some("The selected video encoder is not available on this machine.")
        } else {
            None
        }
    }

    /// Why the current source cannot be queued, if it cannot.
    fn queue_blocker(&self) -> Option<&'static str> {
        if self.media.is_none() {
            Some("Open a video first.")
        } else if let Some(reason) = self.batch_blocker() {
            Some(reason)
        } else if self.output.trim().is_empty() {
            Some("Choose where to save the encode.")
        } else if self.trim_end_secs - self.trim_start_secs < 0.1 {
            Some("The selected range is too short.")
        } else {
            None
        }
    }

    fn open_source(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Video", &VIDEO_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.coordinator.send(Command::Probe(path));
        }
    }

    fn add_batch_files(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("Video", &VIDEO_EXTENSIONS)
            .pick_files()
        {
            self.queue_files(paths);
        }
    }

    fn add_batch_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            self.queue_folder(path);
        }
    }

    fn queue_files(&mut self, paths: Vec<PathBuf>) {
        self.status = Some(Status::Info(format!(
            "Reading {} {}…",
            paths.len(),
            plural(paths.len(), "file")
        )));
        self.coordinator.send(Command::BatchFiles {
            paths,
            settings: self.settings_for_batch(),
        });
    }

    fn queue_folder(&mut self, path: PathBuf) {
        self.status = Some(Status::Info(format!("Scanning {}…", path.display())));
        self.coordinator.send(Command::BatchFolder {
            path,
            settings: self.settings_for_batch(),
        });
    }

    /// Batch files are different sources, so the current source's crop does not apply.
    fn settings_for_batch(&self) -> EncodeSettings {
        let mut settings = self.settings.clone();
        settings.crop = CropMargins::ZERO;
        settings
    }

    /// A single dropped file opens for editing; several files or folders are queued.
    fn handle_dropped(&mut self, paths: Vec<PathBuf>) {
        if self.tools_missing {
            self.status = Some(Status::Info(
                "Install FFmpeg first — the setup screen shows how.".into(),
            ));
            return;
        }
        if let [path] = paths.as_slice()
            && path.is_file()
        {
            self.coordinator.send(Command::Probe(path.clone()));
            return;
        }
        if let Some(reason) = self.batch_blocker() {
            self.status = Some(Status::Error(format!("Cannot queue files yet. {reason}")));
            return;
        }
        let (folders, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|path| path.is_dir());
        for folder in folders {
            self.queue_folder(folder);
        }
        if !files.is_empty() {
            self.queue_files(files);
        }
    }

    fn build_request(&self) -> Option<EncodeRequest> {
        let media = self.media.as_ref()?;
        Some(EncodeRequest {
            input: media.path.clone(),
            output: PathBuf::from(self.output.trim()),
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
            subtitle_tracks: resolve_subtitles(
                &media.streams,
                self.settings.subtitle_selection,
                &self.settings.subtitle_languages,
                &self
                    .selected_subtitle_tracks
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
            ),
            overwrite: false,
            settings: self.settings.clone(),
        })
    }

    fn add_current(&mut self, start: bool) {
        let Some(request) = self.build_request() else {
            return;
        };
        self.submit(request, start, None);
    }

    /// Validates a request and either queues it or asks before replacing an existing file.
    fn submit(&mut self, request: EncodeRequest, start: bool, replaces: Option<JobId>) {
        if let Err(error) = request.validate() {
            self.status = Some(Status::Error(capitalize(&error.to_string())));
            return;
        }
        if self.jobs.iter().any(|job| {
            Some(job.id) != replaces
                && matches!(job.state, JobState::Queued | JobState::Running)
                && job.request.output == request.output
        }) {
            self.status = Some(Status::Error(format!(
                "Another queued job already saves to {}",
                request.output.display()
            )));
            return;
        }
        if request.output.exists() {
            self.pending_add = Some(PendingAdd {
                request,
                start,
                replaces,
            });
        } else {
            self.enqueue(request, start, replaces);
        }
    }

    fn enqueue(&mut self, request: EncodeRequest, start: bool, replaces: Option<JobId>) {
        if let Some(id) = replaces {
            self.coordinator.send(Command::Remove(id));
        }
        let name = file_name(&request.output);
        let output = request.output.clone();
        self.coordinator.send(Command::Add(request));
        if start {
            self.coordinator.send(Command::Start);
            self.status = Some(Status::Info(format!("Encoding {name}")));
        } else {
            self.status = Some(Status::Info(format!("Added {name} to the queue")));
        }
        // Move the destination along so queueing another clip from this source does not collide.
        if replaces.is_none()
            && let Some(media) = &self.media
            && Path::new(self.output.trim()) == output
        {
            self.output = self
                .suggested_output(&media.path, Some(&output))
                .display()
                .to_string();
        }
    }

    fn retry(&mut self, id: JobId) {
        if let Some(job) = self.jobs.iter().find(|job| job.id == id) {
            let mut request = job.request.clone();
            request.overwrite = false;
            self.submit(request, false, Some(id));
        }
    }

    /// `<stem>-encoded.<ext>` in the remembered output folder (or beside the source),
    /// numbered so it never collides with an existing file or a queued job.
    fn suggested_output(&self, input: &Path, also_taken: Option<&Path>) -> PathBuf {
        let folder = self
            .output_dir
            .as_deref()
            .filter(|folder| folder.is_dir())
            .or_else(|| input.parent())
            .unwrap_or(Path::new(""));
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let extension = self.settings.container.extension();
        let taken = |candidate: &Path| {
            candidate == input
                || candidate.exists()
                || also_taken == Some(candidate)
                || self.jobs.iter().any(|job| {
                    matches!(job.state, JobState::Queued | JobState::Running)
                        && job.request.output == candidate
                })
        };
        let mut candidate = folder.join(format!("{stem}-encoded.{extension}"));
        for number in 2..1000 {
            if !taken(&candidate) {
                break;
            }
            candidate = folder.join(format!("{stem}-encoded-{number}.{extension}"));
        }
        candidate
    }

    fn sync_output_extension(&mut self) {
        if !self.output.trim().is_empty() {
            self.output = PathBuf::from(self.output.trim())
                .with_extension(self.settings.container.extension())
                .display()
                .to_string();
        }
    }

    /// Switches presets while keeping the crop, which describes the source rather than the preset.
    fn apply_settings(&mut self, settings: EncodeSettings) {
        let crop = self.settings.crop;
        self.settings = settings;
        self.settings.crop = crop;
        self.reconcile_formats();
        self.sync_output_extension();
    }

    fn base_settings(&self) -> EncodeSettings {
        if let Some(name) = &self.selected_custom_preset
            && let Some(preset) = self
                .custom_presets
                .iter()
                .find(|preset| &preset.name == name)
        {
            return preset.settings.clone();
        }
        if let Some(name) = &self.more_preset
            && let Some(preset) = handbrake::find(name)
        {
            return preset.settings.clone();
        }
        self.preset.settings()
    }

    fn preset_modified(&self) -> bool {
        let mut base = self.base_settings();
        base.crop = self.settings.crop;
        base != self.settings
    }

    fn preset_label(&self) -> String {
        self.selected_custom_preset
            .as_deref()
            .or(self.more_preset.as_deref())
            .map_or_else(|| self.preset.name().to_owned(), str::to_owned)
    }

    /// Keeps the container and audio codec compatible after the video encoder changes.
    fn reconcile_formats(&mut self) {
        if !self
            .settings
            .container
            .supports_video(self.settings.video_encoder)
        {
            self.settings.container = Container::Matroska;
            self.sync_output_extension();
        }
        if !self
            .settings
            .container
            .supports_audio(self.settings.audio_encoder)
        {
            self.settings.audio_encoder = AudioEncoder::Opus;
        }
        let range = self.settings.video_encoder.quality_range();
        self.settings.quality = self.settings.quality.clamp(*range.start(), *range.end());
    }

    fn detect_crop(&mut self) {
        let Some(media) = &self.media else {
            return;
        };
        let Some((width, height)) = media
            .video_stream()
            .and_then(|video| Some((video.width?, video.height?)))
        else {
            return;
        };
        self.detecting_crop = true;
        self.status = Some(Status::Info("Looking for black bars…".into()));
        self.coordinator.send(Command::DetectCrop {
            path: media.path.clone(),
            duration_secs: media.duration_secs,
            width,
            height,
        });
    }

    fn schedule_preview(&mut self, fast_seek: bool) {
        self.preview_fast_seek = fast_seek;
        let delay = if fast_seek { 35 } else { 80 };
        self.preview_due = Some(Instant::now() + Duration::from_millis(delay));
    }

    fn seek_preview(&mut self, at_secs: f64) {
        let Some(duration) = self.media.as_ref().map(|media| media.duration_secs) else {
            return;
        };
        self.preview_at_secs = at_secs.clamp(0.0, (duration - 0.1).max(0.0));
        self.schedule_preview(false);
    }

    fn set_start_at_playhead(&mut self) {
        self.trim_start_secs = self.preview_at_secs;
        if self.trim_end_secs - self.trim_start_secs < 0.1 {
            self.trim_end_secs = self
                .media
                .as_ref()
                .map_or(self.trim_end_secs, |media| media.duration_secs);
        }
    }

    fn set_end_at_playhead(&mut self) {
        self.trim_end_secs = self.preview_at_secs;
        if self.trim_end_secs - self.trim_start_secs < 0.1 {
            self.trim_start_secs = 0.0;
        }
    }

    /// Estimates run on their own: when a video loads, and again whenever the
    /// current request invalidates the last measurement — no button needed.
    fn dispatch_estimate_if_due(&mut self) {
        if self.estimate_due.is_none_or(|due| due > Instant::now())
            || self.media.is_none()
            || matches!(self.size_estimate, Some(SizeEstimateState::Sampling { .. }))
        {
            return;
        }
        let encoding = self.jobs.iter().any(|job| job.state == JobState::Running);
        let range_too_short = self.trim_end_secs - self.trim_start_secs < 0.1;
        if encoding || range_too_short || self.batch_blocker().is_some() {
            return; // Stay due: fire as soon as the block clears.
        }
        self.estimate_due = None;
        if let Some(request) = self.build_request() {
            self.begin_size_estimate(request);
        }
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
                display_aspect: display_aspect(media),
                fast_seek: self.preview_fast_seek,
            });
        }
    }

    fn modal_open(&self) -> bool {
        self.pending_add.is_some() || self.show_save_preset || self.confirm_close
    }

    fn handle_input(&mut self, context: &egui::Context) {
        let dropped: Vec<PathBuf> = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.handle_dropped(dropped);
        }

        if context.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && self.jobs.iter().any(|job| job.state == JobState::Running)
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }

        if self.modal_open() {
            return;
        }
        let open = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
        if context.input_mut(|input| input.consume_shortcut(&open)) {
            self.open_source();
        }
        if self.media.is_none() || context.egui_wants_keyboard_input() {
            return;
        }
        let (step, set_start, set_end) = context.input(|input| {
            let amount = if input.modifiers.shift { 10.0 } else { 1.0 };
            let step = if input.key_pressed(egui::Key::ArrowLeft) {
                -amount
            } else if input.key_pressed(egui::Key::ArrowRight) {
                amount
            } else {
                0.0
            };
            (
                step,
                input.key_pressed(egui::Key::I),
                input.key_pressed(egui::Key::O) && !input.modifiers.command,
            )
        });
        if step != 0.0 {
            self.seek_preview(self.preview_at_secs + step);
        }
        if set_start {
            self.set_start_at_playhead();
        }
        if set_end {
            self.set_end_at_playhead();
        }
    }

    fn update_title(&mut self, context: &egui::Context) {
        let title = if let Some(job) = self.jobs.iter().find(|job| job.state == JobState::Running) {
            format!(
                "{:.0}% · {} — Carbon Disc",
                job.progress * 100.0,
                file_name(&job.request.input)
            )
        } else if let Some(media) = &self.media {
            format!("{} — Carbon Disc", file_name(&media.path))
        } else {
            "Carbon Disc".into()
        };
        if title != self.window_title {
            context.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = root.ctx().clone();
        context.request_repaint_after(Duration::from_millis(50));
        self.poll(&context);
        self.handle_input(&context);
        self.dispatch_preview_if_due();
        self.dispatch_estimate_if_due();
        self.update_title(&context);

        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(root, |ui| self.toolbar_ui(ui));
        egui::Panel::right("queue")
            .resizable(true)
            .default_size(340.0)
            .min_size(260.0)
            .frame(panel_frame())
            .show(root, |ui| self.queue_ui(ui));
        egui::Panel::bottom("actions")
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(18, 12))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(root, |ui| self.action_bar_ui(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::WINDOW)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(root, |ui| {
                let viewport_height = ui.available_height();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.tools_missing {
                            self.setup_gate_ui(ui);
                            return;
                        }
                        if self.media.is_some() {
                            self.source_ui(ui, viewport_height);
                        } else {
                            self.empty_state_ui(ui);
                        }
                        ui.add_space(20.0);
                        self.settings_ui(ui);
                    });
            });

        self.overwrite_modal(&context);
        self.preset_modal(&context);
        self.close_modal(&context);
        paint_drop_overlay(&context);
    }

    fn on_exit(&mut self) {
        // Nothing useful can be shown to the user at this point, so a failed save is ignored.
        let _ = crate::persistence::save_session(&self.session());
    }
}

fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::same(14))
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
}

/// Logo geometry in a 120×120 box whose origin sits at the disc center minus
/// 60, so the caliper reaches the corners. Shared by the toolbar painter and
/// the window-icon rasterizer so they cannot drift apart. The caliper sits in
/// front of the rotor, like on a real brake.
const LOGO_BOX: f32 = 120.0;
const LOGO_CENTER: f32 = 50.0;
const LOGO_DISC_RADIUS: f32 = 46.0;
const LOGO_BORE_RADIUS: f32 = 15.0;
const LOGO_STUD_RING: f32 = 30.0;
const LOGO_STUD_HOLE: f32 = 5.5;
/// Only the studs clear of the caliper are drawn; the two it spans are hidden,
/// as a real caliper hides the bolts behind it.
const LOGO_STUD_HOLE_ANGLES: [f32; 3] = [126.0, 342.0, 54.0];
const LOGO_CALIPER_RING: f32 = 45.0;
const LOGO_CALIPER_WIDTH: f32 = 30.0;
const LOGO_CALIPER_START: f32 = 193.0;
const LOGO_CALIPER_SWEEP: f32 = 82.0;
const LOGO_CALIPER_GAP: f32 = 2.5;
const LOGO_CALIPER_COLOR: egui::Color32 = egui::Color32::from_rgb(167, 139, 250);

/// Paints the brake-disc logo: solid rotor, center bore, the visible studs,
/// and a lighter caliper gripping the front of the rotor across the top-left,
/// separated by a thin gap punched in the panel color behind it.
fn paint_logo(painter: &egui::Painter, rect: egui::Rect) {
    let scale = rect.width() / LOGO_BOX;
    let center = rect.center();
    let at = |degrees: f32, radius: f32| {
        egui::pos2(
            center.x + radius * scale * degrees.to_radians().cos(),
            center.y + radius * scale * degrees.to_radians().sin(),
        )
    };
    let caliper_path = |steps: usize| {
        (0..=steps)
            .map(|step| {
                at(
                    LOGO_CALIPER_START + LOGO_CALIPER_SWEEP * step as f32 / steps as f32,
                    LOGO_CALIPER_RING,
                )
            })
            .collect::<Vec<egui::Pos2>>()
    };
    painter.circle_filled(center, LOGO_DISC_RADIUS * scale, theme::VIOLET);
    painter.circle_filled(center, LOGO_BORE_RADIUS * scale, theme::PANEL);
    for degrees in LOGO_STUD_HOLE_ANGLES {
        painter.circle_filled(
            at(degrees, LOGO_STUD_RING),
            LOGO_STUD_HOLE * scale,
            theme::PANEL,
        );
    }
    let gap = (LOGO_CALIPER_WIDTH + 2.0 * LOGO_CALIPER_GAP) * scale;
    painter.add(egui::Shape::line(
        caliper_path(16),
        egui::Stroke::new(gap, theme::PANEL),
    ));
    painter.add(egui::Shape::line(
        caliper_path(16),
        egui::Stroke::new(LOGO_CALIPER_WIDTH * scale, LOGO_CALIPER_COLOR),
    ));
}

/// Rasterizes the logo into RGBA pixels for the window icon, so the taskbar
/// shows the brake disc without shipping an image alongside the binary.
#[must_use]
pub fn logo_rgba(size: u32) -> Vec<u8> {
    let scale = f64::from(size) / f64::from(LOGO_BOX);
    let cap_center = |degrees: f32| {
        let radians = f64::from(degrees.to_radians());
        (
            f64::from(LOGO_CENTER) + f64::from(LOGO_CALIPER_RING) * radians.cos(),
            f64::from(LOGO_CENTER) + f64::from(LOGO_CALIPER_RING) * radians.sin(),
        )
    };
    let start_cap = cap_center(LOGO_CALIPER_START);
    let end_cap = cap_center(LOGO_CALIPER_START + LOGO_CALIPER_SWEEP);
    let caliper_half = f64::from(LOGO_CALIPER_WIDTH) / 2.0;
    let gap_half = caliper_half + f64::from(LOGO_CALIPER_GAP);
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let vx = f64::from(x) / scale - 10.0;
            let vy = f64::from(y) / scale - 10.0;
            let distance = (vx - f64::from(LOGO_CENTER)).hypot(vy - f64::from(LOGO_CENTER));
            let angle = ((vy - f64::from(LOGO_CENTER))
                .atan2(vx - f64::from(LOGO_CENTER))
                .to_degrees()
                + 360.0)
                % 360.0;
            let mut solid =
                distance <= f64::from(LOGO_DISC_RADIUS) && distance > f64::from(LOGO_BORE_RADIUS);
            for degrees in LOGO_STUD_HOLE_ANGLES {
                let radians = f64::from(degrees.to_radians());
                let stud_x = f64::from(LOGO_CENTER) + f64::from(LOGO_STUD_RING) * radians.cos();
                let stud_y = f64::from(LOGO_CENTER) + f64::from(LOGO_STUD_RING) * radians.sin();
                solid &= (vx - stud_x).hypot(vy - stud_y) > f64::from(LOGO_STUD_HOLE);
            }
            let in_sweep = angle >= f64::from(LOGO_CALIPER_START)
                && angle <= f64::from(LOGO_CALIPER_START + LOGO_CALIPER_SWEEP);
            let on_ring = (distance - f64::from(LOGO_CALIPER_RING)).abs();
            let caliper = (in_sweep && on_ring <= caliper_half)
                || (vx - start_cap.0).hypot(vy - start_cap.1) <= caliper_half
                || (vx - end_cap.0).hypot(vy - end_cap.1) <= caliper_half;
            // A thin transparent band around the caliper separates it from the
            // rotor, like the gap between pads and disc.
            let gap = ((in_sweep && on_ring <= gap_half)
                || (vx - start_cap.0).hypot(vy - start_cap.1) <= gap_half
                || (vx - end_cap.0).hypot(vy - end_cap.1) <= gap_half)
                && !caliper;
            let [r, g, b, _] = if caliper {
                LOGO_CALIPER_COLOR.to_array()
            } else {
                theme::VIOLET.to_array()
            };
            let alpha = if gap {
                0
            } else if solid || caliper {
                255
            } else {
                0
            };
            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    rgba
}

impl App {
    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (logo, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            paint_logo(ui.painter(), logo);
            ui.label(
                RichText::new("CARBON DISC")
                    .strong()
                    .color(theme::VIOLET)
                    .size(17.0),
            );
            ui.separator();
            if ui
                .button("Open video…")
                .on_hover_text("Open one video to trim, crop and encode (Ctrl+O)")
                .clicked()
            {
                self.open_source();
            }
            let batch_blocker = self.batch_blocker();
            let hint = "Queue every video using the current preset and settings";
            if ui
                .add_enabled(batch_blocker.is_none(), egui::Button::new("Queue files…"))
                .on_hover_text(hint)
                .on_disabled_hover_text(batch_blocker.unwrap_or_default())
                .clicked()
            {
                self.add_batch_files();
            }
            if ui
                .add_enabled(batch_blocker.is_none(), egui::Button::new("Queue folder…"))
                .on_hover_text(format!("{hint}, including subfolders"))
                .on_disabled_hover_text(batch_blocker.unwrap_or_default())
                .clicked()
            {
                self.add_batch_folder();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(&self.backend).small().color(theme::MUTED));
            });
        });
    }

    /// Full-window setup screen shown when FFmpeg cannot be found, replacing
    /// the cryptic encoder-detection error the settings used to surface.
    fn setup_gate_ui(&mut self, ui: &mut egui::Ui) {
        let command = install_command();
        let mut copied = false;
        let mut recheck = false;
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            let (logo, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
            paint_logo(ui.painter(), logo);
        });
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("Carbon Disc needs FFmpeg")
                    .strong()
                    .size(24.0)
                    .color(Color32::WHITE),
            );
        });
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(460.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "FFmpeg does the actual encoding. Install it with your package manager, \
                         then check again — or grab a prebuilt download, which already includes it.",
                    )
                    .color(theme::MUTED),
                )
                .wrap()
                .halign(egui::Align::Center),
            );
        });
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .corner_radius(8)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(command).monospace().color(Color32::WHITE));
                        if ui.button("Copy").clicked() {
                            copied = true;
                        }
                    });
                });
        });
        if cfg!(target_os = "linux") {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("…or your distribution's package manager.")
                        .small()
                        .color(theme::MUTED),
                );
            });
        }
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            if self.checking_tools {
                ui.spinner();
                ui.label(RichText::new("Looking for FFmpeg…").color(theme::MUTED));
            } else if ui.button("Check again").clicked() {
                recheck = true;
            }
        });
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            if let Some(path) = &self.tools_path {
                ui.label(
                    RichText::new(format!("Looked for: {path}"))
                        .small()
                        .color(theme::MUTED),
                );
            }
        });
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(
                    "If it was just installed and still is not found, restart Carbon Disc.",
                )
                .small()
                .color(theme::MUTED),
            );
        });
        if copied {
            ui.ctx().copy_text(command.to_owned());
            self.status = Some(Status::Info(
                "Install command copied to the clipboard".into(),
            ));
        }
        if recheck {
            self.checking_tools = true;
            self.coordinator.send(Command::RecheckTools);
        }
    }

    fn empty_state_ui(&mut self, ui: &mut egui::Ui) {
        let height = 260.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click(),
        );
        let hovered = response.hovered();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 10.0, theme::PANEL);
        painter.rect_stroke(
            rect.shrink(1.0),
            10.0,
            egui::Stroke::new(
                1.5,
                if hovered {
                    theme::VIOLET
                } else {
                    theme::BORDER
                },
            ),
            egui::StrokeKind::Inside,
        );
        let center = rect.center();
        if self.probing {
            egui::Spinner::new()
                .size(28.0)
                .color(theme::VIOLET)
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(
                        center - egui::vec2(0.0, 18.0),
                        egui::vec2(28.0, 28.0),
                    ),
                );
            painter.text(
                center + egui::vec2(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                "Reading video…",
                egui::FontId::proportional(16.0),
                theme::MUTED,
            );
            return;
        }
        let (title, subtitle, batch_hint) = if file_drop_supported() {
            (
                "Drop a video here",
                "or click to browse",
                "Drop several files or a folder to queue them all with the settings below.",
            )
        } else {
            (
                "Open a video",
                "Click here or press Ctrl+O",
                "Use Queue files… or Queue folder… to encode many videos with the settings below.",
            )
        };
        painter.text(
            center - egui::vec2(0.0, 34.0),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(22.0),
            Color32::WHITE,
        );
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            subtitle,
            egui::FontId::proportional(15.0),
            theme::MUTED,
        );
        painter.text(
            center + egui::vec2(0.0, 46.0),
            egui::Align2::CENTER_CENTER,
            batch_hint,
            egui::FontId::proportional(13.0),
            theme::MUTED,
        );
        if let Some(error) = &self.probe_error {
            painter.text(
                center + egui::vec2(0.0, 82.0),
                egui::Align2::CENTER_CENTER,
                format!("Could not read that file: {error}"),
                egui::FontId::proportional(13.0),
                theme::RED,
            );
        }
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.open_source();
        }
    }

    fn source_ui(&mut self, ui: &mut egui::Ui, viewport_height: f32) {
        let Some(media) = &self.media else {
            return;
        };
        let source_duration = media.duration_secs;
        let video = media.video_stream().cloned();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(file_name(&media.path))
                    .strong()
                    .size(18.0)
                    .color(Color32::WHITE),
            );
            if self.probing {
                ui.spinner();
            }
        });
        let mut facts = vec![format_duration(media.duration_secs)];
        if let Some(video) = &video {
            facts.push(format!(
                "{}×{} {}",
                video.width.unwrap_or(0),
                video.height.unwrap_or(0),
                video.codec.to_uppercase()
            ));
            if let Some(fps) = video.frame_rate_display() {
                facts.push(format!("{fps} fps"));
            }
        }
        let audio_count = media.audio_streams().count();
        facts.push(format!(
            "{audio_count} audio {}",
            plural(audio_count, "track")
        ));
        facts.push(
            media
                .format_name
                .split(',')
                .next()
                .unwrap_or("unknown")
                .to_uppercase(),
        );
        facts.push(bytes(media.size_bytes));
        ui.label(RichText::new(facts.join("  ·  ")).color(theme::MUTED));
        ui.add(
            egui::Label::new(
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
            )
            .truncate(),
        );
        if let Some(error) = &self.probe_error {
            ui.colored_label(theme::RED, format!("Could not open the new file: {error}"));
        }
        ui.add_space(10.0);

        if source_duration <= 0.1 {
            ui.label(
                RichText::new(
                    "This source has no usable duration, so it cannot be previewed or trimmed.",
                )
                .color(theme::MUTED),
            );
            return;
        }

        // Preview: a fixed-aspect box so the layout does not jump while frames load.
        let aspect = video
            .as_ref()
            .and_then(|video| Some(video.width? as f32 / video.height?.max(1) as f32))
            .filter(|aspect| aspect.is_finite() && *aspect > 0.1)
            .unwrap_or(16.0 / 9.0);
        let max_height = (viewport_height * 0.4).clamp(160.0, 520.0);
        let height = (ui.available_width() / aspect).min(max_height);
        let width = height * aspect;
        let source_size = video
            .as_ref()
            .and_then(|video| Some((video.width?, video.height?)));
        let cropping = self.tab == Tab::Picture;

        // Tools sit in a strip above the picture, lined up with its edges, so they never cover it.
        let (strip, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
            egui::Sense::hover(),
        );
        let mut tools = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_center_size(
                    strip.center(),
                    egui::vec2(width.max(360.0).min(strip.width()), strip.height()),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        if let Some((source_width, source_height)) = source_size {
            let toggle =
                widgets::icon_toggle(&mut tools, Icon::Crop, cropping).on_hover_text(if cropping {
                    "Done cropping"
                } else {
                    "Crop: drag the edges of the picture"
                });
            if toggle.clicked() {
                self.tab = if cropping { Tab::Video } else { Tab::Picture };
            }
            if cropping {
                let label = if self.detecting_crop {
                    "Detecting…"
                } else {
                    "Detect black bars"
                };
                if tools
                    .add_enabled(!self.detecting_crop, egui::Button::new(label))
                    .on_hover_text("Find letterbox or pillarbox bars and crop them")
                    .clicked()
                {
                    self.detect_crop();
                }
                if tools
                    .add_enabled(!self.settings.crop.is_zero(), egui::Button::new("Reset"))
                    .on_hover_text("Remove the crop")
                    .clicked()
                {
                    self.settings.crop = CropMargins::ZERO;
                }
                let crop = self.settings.crop;
                tools.label(
                    RichText::new(format!(
                        "{} × {}",
                        source_width.saturating_sub(crop.left + crop.right),
                        source_height.saturating_sub(crop.top + crop.bottom)
                    ))
                    .monospace()
                    .color(theme::MUTED),
                );
            }
        }
        tools.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} / {}",
                    format_timecode(self.preview_at_secs, true),
                    format_timecode(source_duration, false)
                ))
                .monospace()
                .color(theme::MUTED),
            );
            if self.preview_loading && self.preview_texture.is_some() {
                ui.spinner();
            }
        });
        ui.add_space(6.0);

        let (row, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        let frame_rect = egui::Rect::from_center_size(row.center(), egui::vec2(width, height));
        ui.painter().rect_filled(frame_rect, 6.0, Color32::BLACK);
        if let Some(texture) = &self.preview_texture {
            egui::Image::new(texture)
                .corner_radius(6)
                .paint_at(ui, frame_rect);
        }
        if let Some((source_width, source_height)) = source_size
            && crop_editor::show(
                ui,
                frame_rect,
                source_width,
                source_height,
                &mut self.settings.crop,
                cropping,
            )
        {
            // The tool strip above was drawn with the old size; refresh it right away.
            ui.ctx().request_repaint();
        }
        if self.preview_loading && self.preview_texture.is_none() {
            egui::Spinner::new()
                .size(28.0)
                .color(Color32::WHITE)
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(frame_rect.center(), egui::vec2(28.0, 28.0)),
                );
        } else if let Some(error) = &self.preview_error {
            ui.painter().text(
                frame_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Preview unavailable: {error}"),
                egui::FontId::proportional(13.0),
                theme::RED,
            );
        }

        ui.add_space(4.0);
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

        let mut seek_to = None;
        let mut set_start = false;
        let mut set_end = false;
        let mut reset = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Start").color(theme::MUTED));
            if ui
                .add(timecode_drag(
                    &mut self.trim_start_secs,
                    0.0..=(self.trim_end_secs - 0.1).max(0.0),
                ))
                .on_hover_text("Drag or type a time like 1:30.5")
                .changed()
            {
                seek_to = Some(self.trim_start_secs);
            }
            set_start = widgets::icon_button(ui, Icon::MarkIn, true)
                .on_hover_text("Start the clip at the current frame (I)")
                .clicked();
            ui.add_space(14.0);
            ui.label(RichText::new("End").color(theme::MUTED));
            if ui
                .add(timecode_drag(
                    &mut self.trim_end_secs,
                    (self.trim_start_secs + 0.1)..=source_duration,
                ))
                .on_hover_text("Drag or type a time like 1:30.5")
                .changed()
            {
                seek_to = Some(self.trim_end_secs);
            }
            set_end = widgets::icon_button(ui, Icon::MarkOut, true)
                .on_hover_text("End the clip at the current frame (O)")
                .clicked();
            ui.add_space(14.0);
            let trimmed = self.trim_start_secs > 0.0 || self.trim_end_secs < source_duration - 0.05;
            ui.label(RichText::new("Length").color(theme::MUTED));
            ui.label(
                RichText::new(format_timecode(
                    self.trim_end_secs - self.trim_start_secs,
                    false,
                ))
                .color(if trimmed {
                    theme::VIOLET_LIGHT
                } else {
                    Color32::WHITE
                }),
            );
            reset = widgets::icon_button(ui, Icon::FullRange, trimmed)
                .on_hover_text("Use the whole video")
                .clicked();
        });
        if set_start {
            self.set_start_at_playhead();
        }
        if set_end {
            self.set_end_at_playhead();
        }
        if reset {
            self.trim_start_secs = 0.0;
            self.trim_end_secs = source_duration;
        }
        if let Some(at_secs) = seek_to {
            self.seek_preview(at_secs);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        self.preset_bar_ui(ui);
        ui.add_space(12.0);
        let audio_label = match &self.media {
            Some(media) if media.audio_streams().count() > 0 => format!(
                "Audio  {}/{}",
                self.selected_audio_tracks.len(),
                media.audio_streams().count()
            ),
            _ => "Audio".into(),
        };
        let subtitle_label = if self.settings.subtitle_selection == SubtitleSelection::Manual {
            format!("Subtitles  {}", self.selected_subtitle_tracks.len())
        } else {
            "Subtitles".to_owned()
        };
        let tabs = [
            (Tab::Video, "Video".to_owned()),
            (Tab::Audio, audio_label),
            (Tab::Subtitles, subtitle_label),
            (Tab::Picture, "Size & crop".to_owned()),
        ];
        egui::Frame::new()
            .fill(theme::PANEL)
            .corner_radius(8)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .inner_margin(egui::Margin {
                left: 16,
                right: 16,
                top: 0,
                bottom: 18,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                widgets::tab_bar(ui, &mut self.tab, &tabs);
                ui.add_space(14.0);
                match self.tab {
                    Tab::Video => self.video_ui(ui),
                    Tab::Audio => self.audio_ui(ui),
                    Tab::Subtitles => self.subtitle_ui(ui),
                    Tab::Picture => self.picture_ui(ui),
                }
            });
    }

    fn preset_bar_ui(&mut self, ui: &mut egui::Ui) {
        let chip = self.estimate_chip();
        let modified = self.preset_modified();
        let mut chosen: Option<(Preset, Option<String>, EncodeSettings)> = None;
        let mut revert = false;
        let mut delete = false;
        let mut import = false;
        let mut export = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Preset").strong().size(15.0));
            let label = if modified {
                format!("{}  (modified)", self.preset_label())
            } else {
                self.preset_label()
            };
            egui::ComboBox::from_id_salt("preset")
                .selected_text(label)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for preset in Preset::ALL {
                        let selected = self.selected_custom_preset.is_none()
                            && self.more_preset.is_none()
                            && self.preset == preset;
                        if ui
                            .selectable_label(selected, preset.name())
                            .on_hover_text(preset.description())
                            .clicked()
                        {
                            chosen = Some((preset, None, preset.settings()));
                        }
                    }
                    if !self.custom_presets.is_empty() {
                        ui.separator();
                        ui.label(RichText::new("Your presets").small().color(theme::MUTED));
                        for preset in &self.custom_presets {
                            let selected =
                                self.selected_custom_preset.as_deref() == Some(&preset.name);
                            let response = ui.selectable_label(selected, &preset.name);
                            let response = if preset.description.is_empty() {
                                response
                            } else {
                                response.on_hover_text(&preset.description)
                            };
                            if response.clicked() {
                                chosen = Some((
                                    self.preset,
                                    Some(preset.name.clone()),
                                    preset.settings.clone(),
                                ));
                            }
                        }
                    }
                    ui.separator();
                    if ui
                        .selectable_label(false, "More…")
                        .on_hover_text("HandBrake's official presets, searchable")
                        .clicked()
                    {
                        self.more_presets_open = true;
                        self.preset_search_focus = true;
                        ui.close();
                    }
                });
            if modified {
                revert = ui
                    .button("Revert")
                    .on_hover_text("Discard your changes and go back to the preset's settings")
                    .clicked();
            }
            if ui
                .button("Save as preset…")
                .on_hover_text("Save the current settings so you can pick them next time")
                .clicked()
            {
                self.preset_name_input = self.selected_custom_preset.clone().unwrap_or_default();
                self.preset_description_input = self
                    .custom_presets
                    .iter()
                    .find(|preset| Some(&preset.name) == self.selected_custom_preset.as_ref())
                    .map_or_else(String::new, |preset| preset.description.clone());
                self.show_save_preset = true;
            }
            ui.menu_button("☰", |ui| {
                if ui
                    .add_enabled(
                        self.selected_custom_preset.is_some(),
                        egui::Button::new("Delete this preset"),
                    )
                    .on_disabled_hover_text("Built-in presets cannot be deleted")
                    .clicked()
                {
                    delete = true;
                }
                ui.separator();
                if ui.button("Import presets…").clicked() {
                    import = true;
                }
                if ui
                    .add_enabled(
                        !self.custom_presets.is_empty(),
                        egui::Button::new("Export your presets…"),
                    )
                    .clicked()
                {
                    export = true;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if chip.spinner {
                    ui.spinner();
                }
                if let Some(label) = &chip.label {
                    let response = ui.label(label.clone());
                    if let Some(tooltip) = &chip.tooltip {
                        response.on_hover_text(tooltip);
                    }
                } else {
                    ui.label(RichText::new("Estimated size…").color(theme::MUTED));
                }
            });
        });
        let description = if let Some(name) = &self.selected_custom_preset {
            self.custom_presets
                .iter()
                .find(|preset| &preset.name == name)
                .map(|preset| preset.description.clone())
                .unwrap_or_default()
        } else if let Some(preset) = self.more_preset.as_deref().and_then(handbrake::find) {
            format!("HandBrake preset · {}", preset.category)
        } else {
            self.preset.description().to_owned()
        };
        let summary = settings_summary(&self.settings);
        ui.label(
            RichText::new(if description.is_empty() {
                summary
            } else {
                format!("{description}  ·  {summary}")
            })
            .color(theme::MUTED),
        );

        if self.more_presets_open {
            let mut open = self.more_presets_open;
            egui::Window::new("More presets")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .default_width(430.0)
                .show(ui.ctx(), |ui| {
                    let search = egui::TextEdit::singleline(&mut self.preset_search)
                        .hint_text("Search presets…")
                        .desired_width(f32::INFINITY);
                    let response = ui.add(search);
                    if self.preset_search_focus {
                        response.request_focus();
                        self.preset_search_focus = false;
                    }
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical()
                        .id_salt("more_presets")
                        .auto_shrink([false, false])
                        .max_height(340.0)
                        .show(ui, |ui| {
                            let query = self.preset_search.to_lowercase();
                            let mut shown = 0;
                            let mut last_category = "";
                            for preset in &handbrake::MORE_PRESETS {
                                if !query.is_empty()
                                    && !preset.name.to_lowercase().contains(&query)
                                    && !preset.category.to_lowercase().contains(&query)
                                {
                                    continue;
                                }
                                shown += 1;
                                if preset.category != last_category {
                                    ui.add_space(4.0);
                                    ui.label(
                                        RichText::new(preset.category)
                                            .small()
                                            .strong()
                                            .color(theme::MUTED),
                                    );
                                    last_category = preset.category;
                                }
                                let selected = self.more_preset.as_deref() == Some(preset.name);
                                if ui
                                    .selectable_label(
                                        selected,
                                        RichText::new(preset.name).color(Color32::WHITE),
                                    )
                                    .clicked()
                                {
                                    let settings = preset.settings.clone();
                                    self.selected_custom_preset = None;
                                    self.more_preset = Some(preset.name.to_owned());
                                    self.apply_settings(settings);
                                    self.more_presets_open = false;
                                    self.preset_search.clear();
                                }
                                hint(ui, &settings_summary(&preset.settings));
                            }
                            if shown == 0 {
                                ui.label(
                                    RichText::new("No presets match your search.")
                                        .color(theme::MUTED),
                                );
                            }
                        });
                });
            self.more_presets_open = open;
        }

        if let Some((preset, custom, settings)) = chosen {
            self.preset = preset;
            self.selected_custom_preset = custom;
            self.more_preset = None;
            self.apply_settings(settings);
        }
        if revert {
            self.apply_settings(self.base_settings());
        }
        if delete && let Some(name) = self.selected_custom_preset.take() {
            self.custom_presets.retain(|preset| preset.name != name);
            self.save_custom_presets();
            self.status = Some(Status::Info(format!("Deleted preset “{name}”")));
        }
        if import
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Carbon Disc presets", &["json"])
                .pick_file()
        {
            match crate::persistence::import_presets(&path) {
                Ok(imported) => {
                    let count = imported.len();
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
                    self.status = Some(Status::Info(format!(
                        "Imported {count} {}",
                        plural(count, "preset")
                    )));
                }
                Err(error) => self.status = Some(Status::Error(format!("Import failed: {error}"))),
            }
        }
        if export
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Carbon Disc presets", &["json"])
                .set_file_name("carbon-disc-presets.json")
                .save_file()
        {
            self.status = Some(
                match crate::persistence::export_presets(&path, &self.custom_presets) {
                    Ok(()) => Status::Info(format!("Exported presets to {}", path.display())),
                    Err(error) => Status::Error(format!("Export failed: {error}")),
                },
            );
        }
    }

    fn video_ui(&mut self, ui: &mut egui::Ui) {
        let mut formats_changed = false;
        field_row(ui, "Encoder", |ui| {
            if let Some(capabilities) = &self.capabilities {
                let before = self.settings.video_encoder;
                egui::ComboBox::from_id_salt("video_encoder")
                    .selected_text(encoder_label(self.settings.video_encoder))
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for encoder in &capabilities.video_encoders {
                            ui.selectable_value(
                                &mut self.settings.video_encoder,
                                *encoder,
                                encoder_label(*encoder),
                            );
                        }
                    });
                formats_changed |= before != self.settings.video_encoder;
                if !capabilities
                    .video_encoders
                    .contains(&self.settings.video_encoder)
                {
                    ui.colored_label(
                        theme::RED,
                        "Not available on this machine. Pick another encoder.",
                    );
                } else if self.settings.video_encoder.is_hardware() {
                    hint(
                        ui,
                        "⚡ Hardware encoder: much faster, slightly larger files",
                    );
                }
            } else if let Some(error) = &self.capability_error {
                ui.colored_label(theme::RED, format!("Encoder detection failed: {error}"));
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Detecting encoders…").color(theme::MUTED));
                });
            }
        });
        field_row(ui, "Quality", |ui| {
            let range = self.settings.video_encoder.quality_range();
            ui.spacing_mut().slider_width = 280.0;
            ui.add(
                egui::Slider::new(&mut self.settings.quality, range)
                    .clamping(egui::SliderClamping::Always),
            );
            hint(
                ui,
                if self.settings.video_encoder.higher_quality_is_better() {
                    "Higher = better picture, bigger file"
                } else {
                    "Lower = better picture, bigger file. Around 18–24 suits most videos."
                },
            );
        });
        field_row(ui, "Speed", |ui| {
            let options: Vec<_> = EncoderSpeed::ALL
                .iter()
                .map(|speed| (*speed, speed.display_name()))
                .collect();
            widgets::segmented(ui, "speed", &mut self.settings.speed, &options, |_| None);
            hint(ui, "Slower encodes make smaller files at the same quality");
        });
        field_row(ui, "Frame rate", |ui| {
            let selected = self
                .settings
                .frame_rate
                .map_or_else(|| "Same as source".to_owned(), |fps| format!("{fps} fps"));
            egui::ComboBox::from_id_salt("frame_rate")
                .selected_text(selected)
                .width(260.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.frame_rate, None, "Same as source");
                    for fps in FRAME_RATE_CHOICES {
                        ui.selectable_value(
                            &mut self.settings.frame_rate,
                            Some(fps),
                            format!("{fps} fps"),
                        );
                    }
                });
            let source_rate = self
                .media
                .as_ref()
                .and_then(|media| media.video_stream())
                .and_then(|video| video.frame_rate_display());
            let hint_text = match (&self.settings.frame_rate, source_rate) {
                (Some(_), Some(source)) => {
                    format!("Re-times frames to hit the target; the source runs at {source} fps")
                }
                (Some(_), None) => {
                    "Re-times frames to hit the target; lower rates make smaller files".to_owned()
                }
                (None, Some(source)) => format!("Encodes at the source's {source} fps"),
                (None, None) => "Keeps the source's frame rate".to_owned(),
            };
            hint(ui, &hint_text);
        });
        field_row(ui, "Format", |ui| {
            let options: Vec<_> = Container::ALL
                .iter()
                .map(|container| (*container, container.display_name()))
                .collect();
            let encoder = self.settings.video_encoder;
            formats_changed |= widgets::segmented(
                ui,
                "container",
                &mut self.settings.container,
                &options,
                |container| {
                    (!container.supports_video(encoder))
                        .then(|| format!("{} needs VP9 or AV1 video", container.display_name()))
                },
            );
        });
        if formats_changed {
            self.reconcile_formats();
            self.sync_output_extension();
        }
    }

    fn audio_ui(&mut self, ui: &mut egui::Ui) {
        let tracks: Vec<_> = self
            .media
            .as_ref()
            .map(|media| media.audio_streams().cloned().collect())
            .unwrap_or_default();
        field_row(ui, "Tracks", |ui| {
            if self.media.is_none() {
                ui.label(
                    RichText::new(
                        "Open a video to choose its audio tracks. Queued files keep their first audio track.",
                    )
                    .color(theme::MUTED),
                );
                return;
            }
            if tracks.is_empty() {
                ui.label(RichText::new("This video has no audio.").color(theme::MUTED));
                return;
            }
            for (number, track) in tracks.iter().enumerate() {
                let mut selected = self.selected_audio_tracks.contains(&track.index);
                let mut label = format!(
                    "{}.  {}",
                    number + 1,
                    language_name(track.language.as_deref())
                );
                if let Some(title) = &track.title {
                    label.push_str(&format!(" — {title}"));
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut selected, RichText::new(label).color(Color32::WHITE));
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            track.codec.to_uppercase(),
                            channel_layout(track.channels)
                        ))
                        .color(theme::MUTED),
                    );
                });
                if selected {
                    self.selected_audio_tracks.insert(track.index);
                } else {
                    self.selected_audio_tracks.remove(&track.index);
                }
            }
            if tracks.len() > 1 {
                ui.horizontal(|ui| {
                    if ui.button("Select all").clicked() {
                        self.selected_audio_tracks
                            .extend(tracks.iter().map(|track| track.index));
                    }
                    if ui.button("Select none").clicked() {
                        self.selected_audio_tracks.clear();
                    }
                });
            }
            if self.selected_audio_tracks.is_empty() {
                ui.colored_label(theme::RED, "No tracks selected: the output will be silent.");
            }
        });
        field_row(ui, "Codec", |ui| {
            let options: Vec<_> = AudioEncoder::ALL
                .iter()
                .map(|encoder| (*encoder, encoder.display_name()))
                .collect();
            let container = self.settings.container;
            widgets::segmented(
                ui,
                "audio_codec",
                &mut self.settings.audio_encoder,
                &options,
                |encoder| {
                    (!container.supports_audio(encoder)).then(|| {
                        format!(
                            "{} cannot hold {} audio",
                            container.display_name(),
                            encoder.display_name()
                        )
                    })
                },
            );
            hint(
                ui,
                match self.settings.audio_encoder {
                    AudioEncoder::Aac => "AAC plays everywhere",
                    AudioEncoder::Opus => "Opus sounds better at the same bitrate",
                    AudioEncoder::Copy => "Keeps the original audio untouched",
                },
            );
        });
        field_row(ui, "Bitrate", |ui| {
            ui.add_enabled_ui(self.settings.audio_encoder != AudioEncoder::Copy, |ui| {
                egui::ComboBox::from_id_salt("audio_bitrate")
                    .selected_text(format!("{} kb/s", self.settings.audio_bitrate_kbps))
                    .show_ui(ui, |ui| {
                        for bitrate in AUDIO_BITRATES {
                            ui.selectable_value(
                                &mut self.settings.audio_bitrate_kbps,
                                bitrate,
                                format!("{bitrate} kb/s"),
                            );
                        }
                    });
            });
        });
    }

    fn subtitle_ui(&mut self, ui: &mut egui::Ui) {
        field_row(ui, "Selection", |ui| {
            egui::ComboBox::from_id_salt("subtitle_selection")
                .selected_text(self.settings.subtitle_selection.display_name())
                .width(260.0)
                .show_ui(ui, |ui| {
                    for selection in SubtitleSelection::ALL {
                        ui.selectable_value(
                            &mut self.settings.subtitle_selection,
                            selection,
                            selection.display_name(),
                        );
                    }
                });
            hint(
                ui,
                match self.settings.subtitle_selection {
                    SubtitleSelection::None => "The output will have no subtitle tracks",
                    SubtitleSelection::Manual => {
                        "Pick tracks below. Queued files pick the same stream numbers when they have them."
                    }
                    SubtitleSelection::FirstMatching => {
                        "Adds the first track in any of the selected languages"
                    }
                    SubtitleSelection::AllMatching => "Adds every track in the selected languages",
                },
            );
        });
        if matches!(
            self.settings.subtitle_selection,
            SubtitleSelection::FirstMatching | SubtitleSelection::AllMatching
        ) {
            field_row(ui, "Languages", |ui| {
                let mut pool: Vec<String> = self
                    .media
                    .as_ref()
                    .map(|media| {
                        let mut codes: Vec<String> = media
                            .subtitle_streams()
                            .filter_map(|stream| stream.language.clone())
                            .collect();
                        codes.sort();
                        codes.dedup();
                        codes
                    })
                    .unwrap_or_default();
                if pool.is_empty() {
                    pool = COMMON_LANGUAGES
                        .iter()
                        .map(|code| (*code).to_owned())
                        .collect();
                }
                let available: Vec<String> = pool
                    .into_iter()
                    .filter(|code| !self.settings.subtitle_languages.contains(code))
                    .collect();
                // HandBrake-style columns: the two lists side by side with the
                // Add/Remove buttons between them.
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(220.0, 130.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(RichText::new("Available").color(theme::MUTED));
                            sunken_frame(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("available_languages")
                                    .auto_shrink([false, false])
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                    )
                                    .max_height(106.0)
                                    .show(ui, |ui| {
                                        if available.is_empty() {
                                            ui.label(
                                                RichText::new("All added").color(theme::MUTED),
                                            );
                                        }
                                        for code in &available {
                                            if ui
                                                .selectable_label(
                                                    self.available_language_pick.as_deref()
                                                        == Some(code),
                                                    language_name(Some(code)),
                                                )
                                                .clicked()
                                            {
                                                self.available_language_pick = Some(code.clone());
                                            }
                                        }
                                    });
                            });
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(74.0, 130.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            ui.add_space(ui.spacing().interact_size.y + 6.0);
                            ui.add_enabled_ui(self.available_language_pick.is_some(), |ui| {
                                if ui.button("Add").clicked()
                                    && let Some(code) = self.available_language_pick.take()
                                {
                                    self.settings.subtitle_languages.push(code);
                                    self.selected_language_pick = None;
                                }
                            });
                            ui.add_space(6.0);
                            ui.add_enabled_ui(self.selected_language_pick.is_some(), |ui| {
                                if ui.button("Remove").clicked()
                                    && let Some(code) = self.selected_language_pick.take()
                                {
                                    self.settings
                                        .subtitle_languages
                                        .retain(|kept| kept != &code);
                                    self.available_language_pick = None;
                                }
                            });
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(220.0, 130.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(RichText::new("Selected").color(theme::MUTED));
                            sunken_frame(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("selected_languages")
                                    .auto_shrink([false, false])
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                                    )
                                    .max_height(106.0)
                                    .show(ui, |ui| {
                                        if self.settings.subtitle_languages.is_empty() {
                                            ui.label(RichText::new("None").color(theme::MUTED));
                                        }
                                        for code in &self.settings.subtitle_languages {
                                            if ui
                                                .selectable_label(
                                                    self.selected_language_pick.as_deref()
                                                        == Some(code),
                                                    language_name(Some(code)),
                                                )
                                                .clicked()
                                            {
                                                self.selected_language_pick = Some(code.clone());
                                            }
                                        }
                                    });
                            });
                        },
                    );
                });
                hint(
                    ui,
                    "Queued files resolve their own tracks from these languages.",
                );
            });
        }
        if self.settings.subtitle_selection == SubtitleSelection::Manual {
            let tracks: Vec<_> = self
                .media
                .as_ref()
                .map(|media| media.subtitle_streams().cloned().collect())
                .unwrap_or_default();
            field_row(ui, "Tracks", |ui| {
                if self.media.is_none() {
                    ui.label(
                        RichText::new(
                            "Open a video to pick its subtitle tracks. Queued files pick the same stream numbers when they have them.",
                        )
                        .color(theme::MUTED),
                    );
                    return;
                }
                if tracks.is_empty() {
                    ui.label(
                        RichText::new("This video has no subtitle tracks.").color(theme::MUTED),
                    );
                    return;
                }
                let burned_index = self
                    .settings
                    .burn_subtitle
                    .then(|| self.selected_subtitle_tracks.iter().next().copied())
                    .flatten();
                for (number, track) in tracks.iter().enumerate() {
                    let mut selected = self.selected_subtitle_tracks.contains(&track.index);
                    let mut label = format!(
                        "{}.  {}",
                        number + 1,
                        language_name(track.language.as_deref())
                    );
                    if let Some(title) = &track.title {
                        label.push_str(&format!(" — {title}"));
                    }
                    if burned_index == Some(track.index) {
                        label.push_str("  ·  burned in");
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut selected, RichText::new(label).color(Color32::WHITE));
                        ui.label(RichText::new(track.codec.to_uppercase()).color(theme::MUTED));
                    });
                    if selected {
                        self.selected_subtitle_tracks.insert(track.index);
                    } else {
                        self.selected_subtitle_tracks.remove(&track.index);
                    }
                }
                if self.selected_subtitle_tracks.is_empty() {
                    ui.label(
                        RichText::new("No tracks selected: the output will have no subtitles.")
                            .color(theme::MUTED),
                    );
                }
            });
        }
        field_row(ui, "Burn-in", |ui| {
            ui.add_enabled_ui(
                self.settings.subtitle_selection != SubtitleSelection::None,
                |ui| {
                    ui.checkbox(
                        &mut self.settings.burn_subtitle,
                        "Burn the first selected track into the picture",
                    );
                },
            );
            hint(
                ui,
                "Renders subtitles onto the video itself: always visible and needs no player support. Picture tracks (PGS, VobSub) can only burn in — MP4 and WebM cannot hold them as tracks.",
            );
        });
    }

    fn picture_ui(&mut self, ui: &mut egui::Ui) {
        let source = self.media.as_ref().and_then(|media| {
            let video = media.video_stream()?;
            Some((video.width?, video.height?))
        });
        field_row(ui, "Resolution", |ui| self.resolution_ui(ui, source));
        field_row(ui, "Crop", |ui| {
            if let Some((width, height)) = source {
                self.crop_ui(ui, width, height);
            } else {
                ui.label(RichText::new("Open a video to crop it.").color(theme::MUTED));
            }
        });

        if let Some((source_width, source_height)) = source {
            let crop = self.settings.crop;
            let cropped_width = source_width.saturating_sub(crop.left + crop.right).max(2);
            let cropped_height = source_height.saturating_sub(crop.top + crop.bottom).max(2);
            let (output_width, output_height) = scaled_dimensions(
                cropped_width,
                cropped_height,
                self.settings.max_width,
                self.settings.max_height,
            );
            field_row(ui, "Output", |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{output_width} × {output_height}"))
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(format!(
                            "from {source_width} × {source_height}{}",
                            if crop.is_zero() {
                                String::new()
                            } else {
                                format!(", cropped to {cropped_width} × {cropped_height}")
                            }
                        ))
                        .color(theme::MUTED),
                    );
                });
            });
        }
    }

    fn resolution_ui(&mut self, ui: &mut egui::Ui, source: Option<(u32, u32)>) {
        let current = (self.settings.max_width, self.settings.max_height);
        let matched = RESOLUTION_LIMITS
            .iter()
            .find(|(_, width, height)| current == (Some(*width), Some(*height)));
        let mut custom = current != (None, None) && matched.is_none();
        let selected_text = match (current, matched) {
            ((None, None), _) => "Same as source".to_owned(),
            (_, Some((name, _, _))) => format!("Up to {name}"),
            _ => "Custom".to_owned(),
        };
        egui::ComboBox::from_id_salt("resolution")
            .selected_text(selected_text)
            .width(200.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(current == (None, None), "Same as source")
                    .clicked()
                {
                    self.settings.max_width = None;
                    self.settings.max_height = None;
                    custom = false;
                }
                for (name, width, height) in RESOLUTION_LIMITS {
                    let larger_than_source = source.is_some_and(|(source_width, source_height)| {
                        width >= source_width && height >= source_height
                    });
                    let response = ui
                        .add_enabled(
                            !larger_than_source,
                            egui::Button::selectable(
                                current == (Some(width), Some(height)),
                                format!("Up to {name}"),
                            ),
                        )
                        .on_disabled_hover_text("The source is already this size or smaller");
                    if response.clicked() {
                        self.settings.max_width = Some(width);
                        self.settings.max_height = Some(height);
                        custom = false;
                    }
                }
                if ui.selectable_label(custom, "Custom…").clicked() && !custom {
                    let (width, height) = source.unwrap_or((1920, 1080));
                    self.settings.max_width = Some(width);
                    self.settings.max_height = Some(height);
                    custom = true;
                }
            });
        if custom {
            let mut width = self.settings.max_width.unwrap_or(1920);
            let mut height = self.settings.max_height.unwrap_or(1080);
            ui.horizontal(|ui| {
                ui.label("Fit within");
                ui.add(
                    egui::DragValue::new(&mut width)
                        .range(16..=7680)
                        .speed(2)
                        .suffix(" px"),
                );
                ui.label("×");
                ui.add(
                    egui::DragValue::new(&mut height)
                        .range(16..=4320)
                        .speed(2)
                        .suffix(" px"),
                );
            });
            self.settings.max_width = Some(width.max(16) & !1);
            self.settings.max_height = Some(height.max(16) & !1);
        }
        ui.label(
            RichText::new("Videos are only ever scaled down, keeping their shape.")
                .small()
                .color(theme::MUTED),
        );
    }

    fn crop_ui(&mut self, ui: &mut egui::Ui, source_width: u32, source_height: u32) {
        hint(
            ui,
            "Drag the edges or corners on the preview, or use Detect black bars there. Fine-tune in pixels below.",
        );
        let crop = &mut self.settings.crop;
        let limits = [
            source_height.saturating_sub(crop.bottom + 16),
            source_height.saturating_sub(crop.top + 16),
            source_width.saturating_sub(crop.right + 16),
            source_width.saturating_sub(crop.left + 16),
        ];
        ui.horizontal(|ui| {
            for ((label, value), max) in [
                ("Top", &mut crop.top),
                ("Bottom", &mut crop.bottom),
                ("Left", &mut crop.left),
                ("Right", &mut crop.right),
            ]
            .into_iter()
            .zip(limits)
            {
                ui.label(RichText::new(label).color(theme::MUTED));
                ui.add(pixel_drag(value, max));
                ui.add_space(6.0);
            }
        });
        crop.top &= !1;
        crop.bottom &= !1;
        crop.left &= !1;
        crop.right &= !1;
    }

    /// A row under the settings tabs showing the projected output size. A measurement
    /// survives quality, range, and audio edits — those derive instantly — while any
    /// other change to the encode invalidates it.
    /// Computes the estimate chip's state, clearing a measurement that no
    /// longer applies to the current request.
    fn estimate_chip(&mut self) -> EstimateChip {
        let current = self.build_request();
        if let Some(previous) = &self.size_measurement_request
            && current
                .as_ref()
                .is_none_or(|request| size_derivation(previous, request) == SizeDerivation::Invalid)
        {
            self.size_estimate = None;
            self.size_measurement = None;
            self.size_measurement_request = None;
            self.estimate_due = Some(Instant::now() + ESTIMATE_DEBOUNCE);
        }
        let label = match &self.size_estimate {
            Some(SizeEstimateState::Sampling { fraction, .. }) => Some((
                RichText::new(format!(
                    "Estimated size: sampling… {:.0}%",
                    fraction * 100.0
                ))
                .color(theme::MUTED),
                None,
            )),
            Some(SizeEstimateState::Measured) => {
                let measurement = self
                    .size_measurement
                    .as_ref()
                    .expect("a measured estimate always has its measurement");
                current.as_ref().map(|request| {
                    let total = measurement.estimate_for(request).total_bytes();
                    match current
                        .as_ref()
                        .and_then(|request| {
                            self.size_measurement_request
                                .as_ref()
                                .map(|previous| size_derivation(previous, request))
                        })
                    {
                        Some(SizeDerivation::Same) => (
                            RichText::new(format!("Estimated size ≈ {}", bytes(total)))
                                .strong()
                                .color(Color32::WHITE),
                            Some(
                                "Measured with short sample encodes at these settings; the real size still varies with the content."
                                    .to_owned(),
                            ),
                        ),
                        _ => (
                            RichText::new(format!("Estimated size ≈ {}*", bytes(total)))
                                .strong()
                                .color(Color32::WHITE),
                            Some(format!(
                                "Derived from the measurement at Q{}; press Estimate to re-measure.",
                                measurement.quality
                            )),
                        ),
                    }
                })
            }
            Some(SizeEstimateState::Failed(message)) => Some((
                RichText::new("Estimate unavailable")
                    .small()
                    .color(theme::RED),
                Some(message.clone()),
            )),
            None => None,
        };
        let (label, tooltip) = match label {
            Some((label, tooltip)) => (Some(label), tooltip),
            None => (None, None),
        };
        EstimateChip {
            label,
            tooltip,
            spinner: matches!(self.size_estimate, Some(SizeEstimateState::Sampling { .. })),
        }
    }

    fn begin_size_estimate(&mut self, request: EncodeRequest) {
        let id = self.next_estimate_id;
        self.next_estimate_id += 1;
        self.size_estimate = Some(SizeEstimateState::Sampling { id, fraction: 0.0 });
        self.size_measurement = None;
        self.size_measurement_request = Some(request.clone());
        self.coordinator.send(Command::EstimateSize { id, request });
    }

    fn action_bar_ui(&mut self, ui: &mut egui::Ui) {
        if self.media.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save as").strong());
                let browse_width = 90.0;
                ui.add_sized(
                    [ui.available_width() - browse_width, 30.0],
                    egui::TextEdit::singleline(&mut self.output).hint_text("Choose an output file"),
                );
                if ui.button("Browse…").clicked() {
                    let output = PathBuf::from(self.output.trim());
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter(
                            self.settings.container.display_name(),
                            &[self.settings.container.extension()],
                        )
                        .set_file_name(output.file_name().unwrap_or_default().to_string_lossy());
                    if let Some(folder) = output.parent().filter(|folder| folder.is_dir()) {
                        dialog = dialog.set_directory(folder);
                    }
                    if let Some(path) = dialog.save_file() {
                        self.output_dir = path.parent().map(Path::to_path_buf);
                        self.output = path
                            .with_extension(self.settings.container.extension())
                            .display()
                            .to_string();
                    }
                }
            });
            if !self.output.trim().is_empty() && Path::new(self.output.trim()).exists() {
                ui.label(
                    RichText::new("A file with this name already exists. You'll be asked before it is replaced.")
                        .small()
                        .color(theme::MUTED),
                );
            }
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            let blocker = self.queue_blocker();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let start_hint = if self.jobs.iter().any(|job| job.state == JobState::Queued) {
                    "Add this video to the queue and start encoding (queued jobs run first)"
                } else {
                    "Encode this video now"
                };
                if ui
                    .add_enabled(
                        blocker.is_none(),
                        egui::Button::new(
                            RichText::new("▶  Start encode")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(theme::VIOLET)
                        .min_size(PRIMARY_BUTTON_SIZE),
                    )
                    .on_hover_text(start_hint)
                    .on_disabled_hover_text(blocker.unwrap_or_default())
                    .clicked()
                {
                    self.add_current(true);
                }
                if ui
                    .add_enabled(
                        blocker.is_none(),
                        egui::Button::new("Add to queue").min_size(egui::vec2(0.0, 34.0)),
                    )
                    .on_hover_text("Queue this video with these settings and keep editing")
                    .on_disabled_hover_text(blocker.unwrap_or_default())
                    .clicked()
                {
                    self.add_current(false);
                }
                ui.with_layout(
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| match &self.status {
                        Some(Status::Error(message)) => {
                            ui.add(
                                egui::Label::new(RichText::new(message).color(theme::RED))
                                    .truncate(),
                            )
                            .on_hover_text(message);
                        }
                        Some(Status::Info(message)) => {
                            ui.add(
                                egui::Label::new(RichText::new(message).color(theme::MUTED))
                                    .truncate(),
                            );
                        }
                        None => {}
                    },
                );
            });
        });
    }

    fn preset_modal(&mut self, context: &egui::Context) {
        if !self.show_save_preset {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::new("save_preset")).show(context, |ui| {
            ui.set_width(340.0);
            ui.heading("Save preset");
            ui.add_space(6.0);
            ui.label("Name");
            let name = ui.add(
                egui::TextEdit::singleline(&mut self.preset_name_input)
                    .desired_width(f32::INFINITY),
            );
            if ui.memory(|memory| memory.focused().is_none()) {
                name.request_focus();
            }
            ui.label("Description (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut self.preset_description_input)
                    .desired_width(f32::INFINITY),
            );
            let name = self.preset_name_input.trim();
            if self.custom_presets.iter().any(|preset| preset.name == name) {
                ui.label(
                    RichText::new("A preset with this name will be updated.")
                        .small()
                        .color(theme::MUTED),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_save = !name.is_empty();
                if ui
                    .add_enabled(can_save, egui::Button::new("Save").fill(theme::VIOLET))
                    .clicked()
                    || (can_save && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || response.should_close() && !save {
            self.show_save_preset = false;
        } else if save {
            let name = self.preset_name_input.trim().to_owned();
            let mut settings = self.settings.clone();
            settings.crop = CropMargins::ZERO;
            let preset = CustomPreset {
                name: name.clone(),
                description: self.preset_description_input.trim().to_owned(),
                settings,
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
            self.status = Some(Status::Info(format!("Saved preset “{name}”")));
            self.selected_custom_preset = Some(name);
            self.show_save_preset = false;
            self.save_custom_presets();
        }
    }

    fn save_custom_presets(&mut self) {
        if let Err(error) = crate::persistence::save_presets(&self.custom_presets) {
            self.status = Some(Status::Error(format!("Could not save presets: {error}")));
        }
    }

    fn overwrite_modal(&mut self, context: &egui::Context) {
        let Some(path) = self
            .pending_add
            .as_ref()
            .map(|pending| pending.request.output.clone())
        else {
            return;
        };
        let mut replace = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::new("replace_output")).show(context, |ui| {
            ui.set_width(420.0);
            ui.heading("Replace existing file?");
            ui.add_space(6.0);
            ui.label(format!("{} already exists in", file_name(&path)));
            ui.monospace(path.parent().unwrap_or(Path::new("")).display().to_string());
            ui.label(
                RichText::new(
                    "The existing file is kept until the new encode finishes successfully.",
                )
                .color(theme::MUTED),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("Replace").fill(theme::RED.gamma_multiply(0.8)))
                    .clicked()
                {
                    replace = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || response.should_close() && !replace {
            self.pending_add = None;
        } else if replace && let Some(pending) = self.pending_add.take() {
            let mut request = pending.request;
            request.overwrite = true;
            self.enqueue(request, pending.start, pending.replaces);
        }
    }

    fn close_modal(&mut self, context: &egui::Context) {
        if !self.confirm_close {
            return;
        }
        let mut quit = false;
        let mut keep = false;
        let response = egui::Modal::new(egui::Id::new("confirm_close")).show(context, |ui| {
            ui.set_width(380.0);
            ui.heading("An encode is still running");
            ui.add_space(6.0);
            ui.label("Quitting stops it. Unfinished jobs stay in the queue for next time.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Keep encoding").clicked() {
                    keep = true;
                }
                if ui
                    .add(egui::Button::new("Stop and quit").fill(theme::RED.gamma_multiply(0.8)))
                    .clicked()
                {
                    quit = true;
                }
            });
        });
        if quit {
            self.confirm_close = false;
            self.allow_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if keep || response.should_close() {
            self.confirm_close = false;
        }
    }

    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.jobs.iter().any(|job| job.state == JobState::Running);
        let waiting = self
            .jobs
            .iter()
            .filter(|job| job.state == JobState::Queued)
            .count();
        let finished = self
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    JobState::Complete | JobState::Failed | JobState::Cancelled
                )
            })
            .count();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Queue")
                    .strong()
                    .size(16.0)
                    .color(Color32::WHITE),
            );
            let mut summary = Vec::new();
            if running {
                summary.push("1 encoding".to_owned());
            }
            if waiting > 0 {
                summary.push(format!("{waiting} waiting"));
            }
            if finished > 0 {
                summary.push(format!("{finished} finished"));
            }
            ui.label(RichText::new(summary.join(" · ")).color(theme::MUTED));
        });
        ui.horizontal(|ui| {
            if running {
                if ui
                    .button(RichText::new("■  Stop").color(theme::RED))
                    .on_hover_text("Cancel the current encode. Waiting jobs stay queued.")
                    .clicked()
                {
                    self.coordinator.send(Command::Cancel);
                }
            } else if ui
                .add_enabled(
                    waiting > 0,
                    egui::Button::new(RichText::new("▶  Start queue").color(if waiting > 0 {
                        theme::GREEN
                    } else {
                        theme::MUTED
                    })),
                )
                .on_disabled_hover_text("Nothing is waiting in the queue")
                .clicked()
            {
                self.coordinator.send(Command::Start);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(finished > 0, egui::Button::new("Clear finished"))
                    .clicked()
                {
                    self.coordinator.send(Command::ClearFinished);
                }
            });
        });
        ui.separator();
        if self.jobs.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Nothing queued").color(theme::MUTED).size(15.0));
                ui.label(
                    RichText::new(if file_drop_supported() {
                        "Use “Add to queue” to line up several encodes, or drop multiple files onto the window."
                    } else {
                        "Use “Add to queue” to line up several encodes, or Queue files… to add many at once."
                    })
                    .small()
                    .color(theme::MUTED),
                );
            });
            return;
        }
        let mut actions = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for job in &self.jobs {
                    job_card(ui, job, &mut actions);
                    ui.add_space(6.0);
                }
            });
        for action in actions {
            match action {
                JobAction::Remove(id) => self.coordinator.send(Command::Remove(id)),
                JobAction::Retry(id) => self.retry(id),
                JobAction::Reveal(path) => {
                    if let Err(error) = reveal(&path) {
                        self.status =
                            Some(Status::Error(format!("Could not open the folder: {error}")));
                    }
                }
            }
        }
    }
}

fn job_card(ui: &mut egui::Ui, job: &JobSnapshot, actions: &mut Vec<JobAction>) {
    let border = match job.state {
        JobState::Running => theme::VIOLET.gamma_multiply(0.7),
        _ => theme::BORDER,
    };
    egui::Frame::new()
        .fill(theme::WINDOW)
        .corner_radius(6)
        .stroke(egui::Stroke::new(1.0, border))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if job.state != JobState::Running
                        && ui
                            .add(egui::Button::new("🗙").small().frame_when_inactive(false))
                            .on_hover_text("Remove from the queue")
                            .clicked()
                    {
                        actions.push(JobAction::Remove(job.id));
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(file_name(&job.request.input))
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .truncate(),
                        )
                        .on_hover_text(job.request.input.display().to_string());
                    });
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new(format!("» {}", file_name(&job.request.output)))
                        .small()
                        .color(theme::MUTED),
                )
                .truncate(),
            )
            .on_hover_text(job.request.output.display().to_string());
            let mut details = settings_summary(&job.request.settings);
            if job.request.range != EncodeRange::ALL {
                details = format!(
                    "Clip {}–{}  ·  {details}",
                    format_timecode(job.request.range.start_secs, false),
                    job.request
                        .range
                        .end_secs
                        .map_or_else(|| "end".into(), |end| format_timecode(end, false)),
                );
            }
            ui.add(egui::Label::new(RichText::new(details).small().color(theme::MUTED)).truncate());
            ui.add_space(2.0);
            match job.state {
                JobState::Running => {
                    ui.add(
                        egui::ProgressBar::new(job.progress)
                            .desired_height(14.0)
                            .fill(theme::VIOLET)
                            .text(format!("{:.0}%", job.progress * 100.0)),
                    );
                    let mut detail = vec!["Encoding".to_owned()];
                    if let Some(speed) = job.speed.filter(|speed| *speed > 0.0) {
                        detail.push(format!("{speed:.2}× realtime"));
                        let remaining = f64::from(1.0 - job.progress)
                            * job.request.output_duration_secs()
                            / f64::from(speed);
                        detail.push(format!("{} left", format_eta(remaining)));
                    }
                    ui.label(
                        RichText::new(detail.join(" · "))
                            .small()
                            .color(theme::VIOLET),
                    );
                }
                JobState::Queued => {
                    ui.label(RichText::new("Waiting").small().color(theme::MUTED));
                }
                JobState::Complete => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✔ Done").color(theme::GREEN));
                        if ui.button("Show in folder").clicked() {
                            actions.push(JobAction::Reveal(job.request.output.clone()));
                        }
                    });
                }
                JobState::Failed | JobState::Cancelled => {
                    ui.horizontal(|ui| {
                        let label = if job.state == JobState::Failed {
                            "Failed"
                        } else {
                            "Cancelled"
                        };
                        ui.label(RichText::new(label).color(theme::RED));
                        if ui
                            .button("Retry")
                            .on_hover_text("Put this job back in the queue")
                            .clicked()
                        {
                            actions.push(JobAction::Retry(job.id));
                        }
                    });
                    if let Some(error) = &job.error {
                        ui.label(RichText::new(error).small().color(theme::RED));
                    }
                }
            }
        });
}

/// A settings row: a fixed-width label aligned with the first control on the right.
fn field_row(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(110.0, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(110.0);
                ui.label(RichText::new(label).color(theme::MUTED));
            },
        );
        ui.vertical(add_contents);
    });
    ui.add_space(10.0);
}

/// A recessed panel behind scrollable lists, so they read as scrollable.
fn sunken_frame(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::SUNKEN)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::same(3))
        .show(ui, add_contents);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(theme::MUTED));
}

fn pixel_drag(value: &mut u32, max: u32) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .range(0..=max)
        .speed(2)
        .suffix(" px")
}

fn timecode_drag(value: &mut f64, range: std::ops::RangeInclusive<f64>) -> egui::DragValue<'_> {
    egui::DragValue::new(value)
        .range(range)
        .speed(0.1)
        .custom_formatter(|seconds, _| format_timecode(seconds, true))
        .custom_parser(parse_timecode)
}

fn paint_drop_overlay(context: &egui::Context) {
    let hovering = context.input(|input| !input.raw.hovered_files.is_empty());
    if !hovering {
        return;
    }
    let rect = context.content_rect();
    let painter = context.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop_overlay"),
    ));
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(190));
    painter.rect_stroke(
        rect.shrink(16.0),
        12.0,
        egui::Stroke::new(2.0, theme::VIOLET),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center() - egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        "Drop to open",
        egui::FontId::proportional(26.0),
        Color32::WHITE,
    );
    painter.text(
        rect.center() + egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        "One video opens for editing · several files or a folder are queued",
        egui::FontId::proportional(14.0),
        theme::MUTED,
    );
}

/// winit only delivers dropped files on X11, Windows and macOS; native Wayland windows never see them.
fn file_drop_supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
        || std::env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty())
}

/// The display aspect of the loaded video, for sizing preview frames.
fn display_aspect(media: &MediaInfo) -> f64 {
    media
        .video_stream()
        .and_then(|video| Some(f64::from(video.width?) / f64::from(video.height?.max(1))))
        .filter(|aspect| aspect.is_finite() && *aspect > 0.1)
        .unwrap_or(16.0 / 9.0)
}

fn install_command() -> &'static str {
    if cfg!(target_os = "windows") {
        "winget install --id Gyan.FFmpeg -e --source winget"
    } else if cfg!(target_os = "macos") {
        "brew install ffmpeg"
    } else {
        "sudo apt install ffmpeg"
    }
}

fn encoder_label(encoder: VideoEncoder) -> String {
    if encoder.is_hardware() {
        format!("⚡ {}", encoder.display_name())
    } else {
        encoder.display_name().to_owned()
    }
}

fn settings_summary(settings: &EncodeSettings) -> String {
    let mut parts = vec![
        settings.video_encoder.display_name().to_owned(),
        format!("Q{}", settings.quality),
        settings.speed.display_name().to_owned(),
    ];
    if let (Some(width), Some(height)) = (settings.max_width, settings.max_height) {
        parts.push(
            RESOLUTION_LIMITS
                .iter()
                .find(|(_, w, h)| (*w, *h) == (width, height))
                .map_or_else(
                    || format!("≤{width}×{height}"),
                    |(name, _, _)| format!("≤{name}"),
                ),
        );
    }
    if let Some(fps) = settings.frame_rate {
        parts.push(format!("{fps} fps"));
    }
    match settings.subtitle_selection {
        SubtitleSelection::None => {}
        SubtitleSelection::Manual => parts.push("subs: manual".to_owned()),
        SubtitleSelection::FirstMatching => parts.push("subs: first match".to_owned()),
        SubtitleSelection::AllMatching => parts.push("subs: all matches".to_owned()),
    }
    if settings.burn_subtitle && settings.subtitle_selection != SubtitleSelection::None {
        parts.push("burned-in".to_owned());
    }
    parts.push(match settings.audio_encoder {
        AudioEncoder::Copy => "audio passthrough".to_owned(),
        encoder => format!(
            "{} {} kb/s",
            encoder.display_name(),
            settings.audio_bitrate_kbps
        ),
    });
    parts.push(settings.container.display_name().to_owned());
    parts.join(" · ")
}

fn language_name(code: Option<&str>) -> String {
    match code.unwrap_or("und") {
        "eng" | "en" => "English".into(),
        "jpn" | "ja" => "Japanese".into(),
        "spa" | "es" => "Spanish".into(),
        "fre" | "fra" | "fr" => "French".into(),
        "ger" | "deu" | "de" => "German".into(),
        "ita" | "it" => "Italian".into(),
        "por" | "pt" => "Portuguese".into(),
        "rus" | "ru" => "Russian".into(),
        "chi" | "zho" | "zh" => "Chinese".into(),
        "kor" | "ko" => "Korean".into(),
        "und" | "" => "Unknown language".into(),
        other => other.to_uppercase(),
    }
}

fn channel_layout(channels: Option<u32>) -> String {
    match channels {
        Some(1) => "Mono".into(),
        Some(2) => "Stereo".into(),
        Some(6) => "5.1".into(),
        Some(8) => "7.1".into(),
        Some(count) => format!("{count} ch"),
        None => "? ch".into(),
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn format_duration(seconds: f64) -> String {
    format_timecode(seconds, false)
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

/// Accepts `90`, `90.5`, `1:30`, `1:30.5` and `0:01:30.500`.
fn parse_timecode(text: &str) -> Option<f64> {
    let mut seconds = 0.0;
    let parts: Vec<_> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for (index, part) in parts.iter().enumerate() {
        let value: f64 = part.trim().parse().ok()?;
        let is_last = index == parts.len() - 1;
        if !value.is_finite() || value < 0.0 || (!is_last && value.fract() != 0.0) {
            return None;
        }
        seconds = seconds * 60.0 + value;
    }
    Some(seconds)
}

fn format_eta(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    if total >= 3600 {
        format!("{}h {:02}m", total / 3600, total / 60 % 60)
    } else if total >= 60 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{total}s")
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

/// Opens the system file manager at the encoded file.
fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(Path::new(".")));
        command
    };
    let mut child = command.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_raster_matches_the_geometry() {
        // At size 120 the logo box maps 1:1, offset by the -10 origin.
        let rgba = logo_rgba(120);
        let pixel = |vx: i32, vy: i32| {
            let x = (vx + 10) as usize;
            let y = (vy + 10) as usize;
            let at = (y * 120 + x) * 4;
            ([rgba[at], rgba[at + 1], rgba[at + 2]], rgba[at + 3])
        };
        assert_eq!(pixel(50, 50).1, 0, "the center bore is transparent");
        assert_eq!(
            pixel(90, 50),
            ([139, 92, 246], 255),
            "the rotor is solid violet"
        );
        assert_eq!(
            pixel(18, 18),
            ([167, 139, 250], 255),
            "the caliper rides in front of the rotor"
        );
        assert_eq!(pixel(29, 29).1, 0, "a gap separates caliper and rotor");
        assert_eq!(pixel(5, 5).1, 0, "outside the logo is transparent");
        let hole_x = (50.0 + 30.0 * 126.0f32.to_radians().cos()) as i32;
        let hole_y = (50.0 + 30.0 * 126.0f32.to_radians().sin()) as i32;
        assert_eq!(
            pixel(hole_x, hole_y).1,
            0,
            "visible stud holes are punched out"
        );
        let hidden_x = (50.0 + 30.0 * 270.0f32.to_radians().cos()) as i32;
        let hidden_y = (50.0 + 30.0 * 270.0f32.to_radians().sin()) as i32;
        assert_eq!(
            pixel(hidden_x, hidden_y).1,
            255,
            "the caliper hides the studs behind it"
        );
    }

    #[test]
    fn timecodes_parse_in_common_forms() {
        assert_eq!(parse_timecode("90"), Some(90.0));
        assert_eq!(parse_timecode("1:30.5"), Some(90.5));
        assert_eq!(parse_timecode("01:01:30.250"), Some(3690.25));
        assert_eq!(parse_timecode(&format_timecode(75.125, true)), Some(75.125));
        assert_eq!(parse_timecode("1.5:00"), None);
        assert_eq!(parse_timecode("abc"), None);
        assert_eq!(parse_timecode("1:2:3:4"), None);
    }

    #[test]
    fn eta_is_compact() {
        assert_eq!(format_eta(42.0), "42s");
        assert_eq!(format_eta(192.0), "3m 12s");
        assert_eq!(format_eta(3720.0), "1h 02m");
    }
}
