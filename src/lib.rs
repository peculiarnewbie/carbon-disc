#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod gui;
pub mod model;
pub mod queue;

pub use engine::{EngineEvent, MediaEngine};
pub use error::{Error, Result};
pub use model::{EncodeRange, EncodeRequest, EncodeSettings, MediaInfo, Preset};
