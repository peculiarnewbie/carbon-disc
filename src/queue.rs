use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use crate::{EncodeRequest, MediaEngine, MediaInfo, PreviewFrame, engine::EngineEvent};

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
        path: std::path::PathBuf,
        at_secs: f64,
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
    PreviewStarted(f64),
    PreviewFinished {
        at_secs: f64,
        result: Result<PreviewFrame, String>,
    },
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
    let mut jobs: VecDeque<JobSnapshot> = VecDeque::new();
    let mut next_id = 1;
    let mut processing = false;
    let mut active: Option<ActiveJob> = None;
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
            Command::Preview { path, at_secs } => {
                let _ = events.send(Event::PreviewStarted(at_secs));
                let result = engine
                    .preview(&path, at_secs, 640)
                    .map_err(|error| error.to_string());
                let _ = events.send(Event::PreviewFinished { at_secs, result });
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
            }
            Command::Remove(id) => {
                jobs.retain(|job| job.id != id || job.state == JobState::Running);
                let _ = events.send(Event::Snapshot(jobs.iter().cloned().collect()));
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
            }
            Command::Shutdown => {
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
