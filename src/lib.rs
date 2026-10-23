#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod gui;
pub mod model;
pub mod persistence;
pub mod queue;

pub use engine::{EngineEvent, MediaEngine};
pub use error::{Error, Result};
pub use model::{
    AudioTrackSettings, CropMargins, CustomPreset, EncodeRange, EncodeRequest, EncodeSettings,
    EngineCapabilities, MediaInfo, Preset, PreviewFrame, SizeDerivation, SizeEstimate,
    SizeMeasurement, size_derivation,
};
