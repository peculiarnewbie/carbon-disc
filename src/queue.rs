use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use crate::{
    AudioTrackSettings, EncodeRange, EncodeRequest, EncodeSettings, EngineCapabilities,
    MediaEngine, MediaInfo, PreviewFrame, engine::EngineEvent,
};

pub type JobId = u64;
type ActiveJob = (JobId, Arc<AtomicBool>, Receiver<(JobId, crate::Result<()>)>);

#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: JobId,
    pub request: EncodeRequest,
    pub state: JobState,
    pub progress: f32,
    pub speed: Option<f32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Debug)]
pub enum Command {
    Probe(std::path::PathBuf),
    Preview {
        id: u64,
        path: std::path::PathBuf,
        at_secs: f64,
        fast_seek: bool,
    },
    BatchFiles {
        paths: Vec<PathBuf>,
        settings: EncodeSettings,
    },
    BatchFolder {
        path: PathBuf,
        settings: EncodeSettings,
    },
    Add(EncodeRequest),
    Remove(JobId),
    Start,
    Cancel,
    ClearFinished,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    ProbeStarted,
    ProbeFinished(Result<MediaInfo, String>),
    PreviewStarted {
        id: u64,
        at_secs: f64,
    },
    PreviewFinished {
        id: u64,
        at_secs: f64,
        result: Result<PreviewFrame, String>,
    },
    Capabilities(Result<EngineCapabilities, String>),
    BatchProgress {
        current: usize,
        total: usize,
    },
    BatchFinished {
        added: usize,
        failed: usize,
    },
    Notice(String),
    Snapshot(Vec<JobSnapshot>),
    Backend(String),
}

pub struct Coordinator {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

impl Coordinator {
    #[must_use]
    pub fn spawn(engine: Arc<dyn MediaEngine>) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || run(engine, command_rx, event_tx));
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

fn run(engine: Arc<dyn MediaEngine>, commands: Receiver<Command>, events: Sender<Event>) {
    let _ = events.send(Event::Backend(engine.name().into()));
    let capability_engine = engine.clone();
    let capability_events = events.clone();
    thread::spawn(move || {
        let result = capability_engine
            .capabilities()
            .map_err(|error| error.to_string());
        let _ = capability_events.send(Event::Capabilities(result));
    });
    let mut jobs: VecDeque<JobSnapshot> = VecDeque::new();
    let mut next_id = 1;
    match crate::persistence::load_queue() {
        Ok(requests) => {
            for mut request in requests {
                request.overwrite = false;
                let (state, error) = restored_state(&request);
                jobs.push_back(JobSnapshot {
                    id: next_id,
                    request,
                    state,
                    progress: 0.0,
                    speed: None,
                    error,
                });
                next_id += 1;
            }
            if !jobs.is_empty() {
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
            }
        }
        Err(error) => {
            let _ = events.send(Event::Notice(format!("Could not restore queue: {error}")));
        }
    }
    let mut processing = false;
    let mut active: Option<ActiveJob> = None;
    let mut preview_cancel: Option<Arc<AtomicBool>> = None;
    loop {
        if let Some((id, cancelled, result_rx)) = &active {
            if let Ok((finished_id, result)) = result_rx.try_recv() {
                if let Some(job) = jobs.iter_mut().find(|job| job.id == finished_id) {
                    match result {
                        Ok(()) => {
                            job.state = JobState::Complete;
                            job.progress = 1.0;
                        }
                        Err(crate::Error::Cancelled) => job.state = JobState::Cancelled,
                        Err(error) => {
                            job.state = JobState::Failed;
                            job.error = Some(error.to_string());
                        }
                    }
                }
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
                active = None;
                persist_jobs(&jobs, &events);
                if !jobs.iter().any(|job| job.state == JobState::Queued) {
                    processing = false;
                }
            } else {
                let _ = (id, cancelled);
            }
        }
        let command = match commands.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if processing {
                    maybe_start(&engine, &mut jobs, &mut active, &events);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            Command::Probe(path) => {
                let _ = events.send(Event::ProbeStarted);
                let result = engine.probe(&path).map_err(|error| error.to_string());
                let _ = events.send(Event::ProbeFinished(result));
            }
            Command::Preview {
                id,
                path,
                at_secs,
                fast_seek,
            } => {
                if let Some(cancelled) = preview_cancel.take() {
                    cancelled.store(true, Ordering::Relaxed);
                }
                let cancelled = Arc::new(AtomicBool::new(false));
                preview_cancel = Some(cancelled.clone());
                let preview_engine = engine.clone();
                let preview_events = events.clone();
                let _ = events.send(Event::PreviewStarted { id, at_secs });
                thread::spawn(move || {
                    let result = preview_engine
                        .preview(&path, at_secs, 640, fast_seek, cancelled)
                        .map_err(|error| error.to_string());
                    let _ = preview_events.send(Event::PreviewFinished {
                        id,
                        at_secs,
                        result,
                    });
                });
            }
            Command::BatchFiles { paths, settings } => {
                import_paths(&engine, paths, &settings, &mut jobs, &mut next_id, &events);
                persist_jobs(&jobs, &events);
            }
            Command::BatchFolder { path, settings } => {
                let paths = discover_media(&path);
                import_paths(&engine, paths, &settings, &mut jobs, &mut next_id, &events);
                persist_jobs(&jobs, &events);
            }
            Command::Add(request) => {
                jobs.push_back(JobSnapshot {
                    id: next_id,
                    request,
                    state: JobState::Queued,
                    progress: 0.0,
                    speed: None,
                    error: None,
                });
                next_id += 1;
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
                persist_jobs(&jobs, &events);
            }
            Command::Remove(id) => {
                jobs.retain(|job| job.id != id || job.state == JobState::Running);
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
                persist_jobs(&jobs, &events);
            }
            Command::Start => {
                processing = true;
                maybe_start(&engine, &mut jobs, &mut active, &events);
            }
            Command::Cancel => {
                processing = false;
                if let Some((_, cancelled, _)) = &active {
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
            Command::ClearFinished => {
                jobs.retain(|job| matches!(job.state, JobState::Queued | JobState::Running));
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
                persist_jobs(&jobs, &events);
            }
            Command::Shutdown => {
                if let Some(cancelled) = &preview_cancel {
                    cancelled.store(true, Ordering::Relaxed);
                }
                if let Some((_, cancelled, _)) = &active {
                    cancelled.store(true, Ordering::Relaxed);
                }
                break;
            }
        }
        if processing {
            maybe_start(&engine, &mut jobs, &mut active, &events);
        }
    }
}

fn maybe_start(
    engine: &Arc<dyn MediaEngine>,
    jobs: &mut VecDeque<JobSnapshot>,
    active: &mut Option<ActiveJob>,
    events: &Sender<Event>,
) {
    if active.is_some() {
        return;
    }
    let Some(job) = jobs.iter_mut().find(|job| job.state == JobState::Queued) else {
        return;
    };
    job.state = JobState::Running;
    let id = job.id;
    let request = job.request.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_worker = cancelled.clone();
    let engine = engine.clone();
    let event_tx = events.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
    thread::spawn(move || {
        let mut emit = |event| {
            if let EngineEvent::Progress {
                fraction, speed, ..
            } = event
            {
                let _ = event_tx.send(Event::Snapshot(vec![JobSnapshot {
                    id,
                    request: request.clone(),
                    state: JobState::Running,
                    progress: fraction,
                    speed,
                    error: None,
                }]));
            }
        };
        let result = engine.encode(&request, cancel_worker, &mut emit);
        let _ = result_tx.send((id, result));
    });
    *active = Some((id, cancelled, result_rx));
}

fn import_paths(
    engine: &Arc<dyn MediaEngine>,
    paths: Vec<PathBuf>,
    settings: &EncodeSettings,
    jobs: &mut VecDeque<JobSnapshot>,
    next_id: &mut JobId,
    events: &Sender<Event>,
) {
    let total = paths.len();
    let mut added = 0;
    let mut failed = 0;
    for (index, path) in paths.into_iter().enumerate() {
        let _ = events.send(Event::BatchProgress {
            current: index + 1,
            total,
        });
        match engine.probe(&path) {
            Ok(media) => {
                let output = suggested_output(&path, settings.container.extension());
                let audio_tracks = media
                    .audio_streams()
                    .next()
                    .map(|track| {
                        vec![AudioTrackSettings {
                            source_index: track.index,
                            encoder: settings.audio_encoder,
                            bitrate_kbps: settings.audio_bitrate_kbps,
                        }]
                    })
                    .unwrap_or_default();
                let request = EncodeRequest {
                    input: path,
                    output,
                    source_duration_secs: media.duration_secs,
                    source_width: media.video_stream().and_then(|stream| stream.width),
                    source_height: media.video_stream().and_then(|stream| stream.height),
                    range: EncodeRange::ALL,
                    audio_tracks,
                    overwrite: false,
                    settings: settings.clone(),
                };
                let (state, error) = restored_state(&request);
                jobs.push_back(JobSnapshot {
                    id: *next_id,
                    request,
                    state,
                    progress: 0.0,
                    speed: None,
                    error,
                });
                *next_id += 1;
                added += 1;
            }
            Err(_) => failed += 1,
        }
    }
    let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
    let _ = events.send(Event::BatchFinished { added, failed });
}

fn discover_media(root: &Path) -> Vec<PathBuf> {
    const EXTENSIONS: [&str; 10] = [
        "mp4", "mkv", "webm", "mov", "avi", "m4v", "ts", "mts", "m2ts", "mpg",
    ];
    let mut media = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                pending.extend(
                    entries
                        .filter_map(std::result::Result::ok)
                        .map(|entry| entry.path()),
                );
            }
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                EXTENSIONS
                    .iter()
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
            })
        {
            media.push(path);
        }
    }
    media.sort();
    media
}

fn suggested_output(input: &Path, extension: &str) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    input.with_file_name(format!("{stem}-encoded.{extension}"))
}

fn restored_state(request: &EncodeRequest) -> (JobState, Option<String>) {
    if !request.input.exists() {
        (
            JobState::Failed,
            Some(format!("Source is missing: {}", request.input.display())),
        )
    } else if request.output.exists() {
        (
            JobState::Failed,
            Some(format!(
                "Destination already exists: {}. Remove and re-add this job to confirm replacement.",
                request.output.display()
            )),
        )
    } else {
        (JobState::Queued, None)
    }
}

fn persist_jobs(jobs: &VecDeque<JobSnapshot>, events: &Sender<Event>) {
    let requests: Vec<_> = jobs
        .iter()
        .filter(|job| matches!(job.state, JobState::Queued | JobState::Running))
        .map(|job| job.request.clone())
        .collect();
    if let Err(error) = crate::persistence::save_queue(&requests) {
        let _ = events.send(Event::Notice(format!("Could not save queue: {error}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_discovery_is_recursive_and_filtered() {
        let root =
            std::env::temp_dir().join(format!("carbon-disc-discovery-{}", std::process::id()));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("create discovery fixture");
        std::fs::write(root.join("one.MKV"), b"").expect("create video fixture");
        std::fs::write(nested.join("two.mp4"), b"").expect("create nested video fixture");
        std::fs::write(root.join("notes.txt"), b"").expect("create text fixture");
        let found = discover_media(&root);
        assert_eq!(found.len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
