use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use crate::{
    CropMargins, EncodeRequest, EngineCapabilities, MediaInfo, PreviewFrame, Result,
    SizeMeasurement,
};

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
    fn preview(
        &self,
        path: &Path,
        at_secs: f64,
        max_width: u32,
        display_aspect: f64,
        fast_seek: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreviewFrame>;
    /// Finds black bars by sampling frames across the source. Returns the largest crop
    /// that is safe for every sample, so bright scenes are never cut.
    fn detect_crop(
        &self,
        path: &Path,
        duration_secs: f64,
        width: u32,
        height: u32,
    ) -> Result<CropMargins>;
    /// Measures output size rates before encoding. Quality-based rate control
    /// makes video size content-dependent, so this encodes short samples spread
    /// across the requested range; estimates at other quality, range, or audio
    /// settings are then derived from the measurement without new encodes.
    fn estimate_size(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<SizeMeasurement>;
    fn encode(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<()>;
}
