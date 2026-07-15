use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use crate::{EncodeRequest, EngineCapabilities, MediaInfo, PreviewFrame, Result};

pub mod ffmpeg;

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Progress {
        fraction: f32,
        frame: Option<u64>,
        speed: Option<f32>,
    },
    Log(String),
}

/// Backend boundary used by the coordinator. Implementations may call a local process,
/// a linked library, or a remote worker; callers only exchange domain values and events.
pub trait MediaEngine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Result<EngineCapabilities>;
    fn probe(&self, path: &Path) -> Result<MediaInfo>;
    fn preview(&self, path: &Path, at_secs: f64, max_width: u32) -> Result<PreviewFrame>;
    fn encode(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<()>;
}
