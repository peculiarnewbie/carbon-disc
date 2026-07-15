use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub format_name: String,
    pub duration_secs: f64,
    pub size_bytes: u64,
    pub bitrate: Option<u64>,
    pub streams: Vec<MediaStream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl MediaInfo {
    #[must_use]
    pub fn video_stream(&self) -> Option<&MediaStream> {
        self.streams
            .iter()
            .find(|stream| stream.kind == StreamKind::Video)
    }

    pub fn audio_streams(&self) -> impl Iterator<Item = &MediaStream> {
        self.streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaStream {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub channels: Option<u32>,
    pub language: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
    Attachment,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncodeRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub source_duration_secs: f64,
    pub range: EncodeRange,
    pub settings: EncodeSettings,
}

impl EncodeRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.input == self.output {
            return Err(crate::Error::InvalidRequest(
                "input and output must be different files".into(),
            ));
        }
        if self.output.as_os_str().is_empty() {
            return Err(crate::Error::InvalidRequest("output path is empty".into()));
        }
        self.range.validate(self.source_duration_secs)?;
        Ok(())
    }

    #[must_use]
    pub fn output_duration_secs(&self) -> f64 {
        self.range.duration_secs(self.source_duration_secs)
    }
}

/// A half-open time range in the source. `end_secs: None` means the end of the source.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct EncodeRange {
    pub start_secs: f64,
    pub end_secs: Option<f64>,
}

impl Default for EncodeRange {
    fn default() -> Self {
        Self::ALL
    }
}

impl EncodeRange {
    pub const ALL: Self = Self {
        start_secs: 0.0,
        end_secs: None,
    };

    #[must_use]
    pub const fn new(start_secs: f64, end_secs: f64) -> Self {
        Self {
            start_secs,
            end_secs: Some(end_secs),
        }
    }

    pub fn validate(self, source_duration_secs: f64) -> crate::Result<()> {
        if !source_duration_secs.is_finite() || source_duration_secs <= 0.0 {
            return Err(crate::Error::InvalidRequest(
                "source duration is unavailable".into(),
            ));
        }
        if !self.start_secs.is_finite() || self.start_secs < 0.0 {
            return Err(crate::Error::InvalidRequest(
                "range start must be a positive time".into(),
            ));
        }
        let end = self.end_secs.unwrap_or(source_duration_secs);
        if !end.is_finite() || end > source_duration_secs + 0.05 {
            return Err(crate::Error::InvalidRequest(
                "range end is beyond the source duration".into(),
            ));
        }
        if end - self.start_secs < 0.1 {
            return Err(crate::Error::InvalidRequest(
                "encode range must be at least 0.1 seconds long".into(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn duration_secs(self, source_duration_secs: f64) -> f64 {
        self.end_secs.unwrap_or(source_duration_secs) - self.start_secs
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncodeSettings {
    pub container: Container,
    pub video_encoder: VideoEncoder,
    pub quality: u8,
    pub speed: EncoderSpeed,
    pub audio_encoder: AudioEncoder,
    pub audio_bitrate_kbps: u16,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        Preset::Balanced.settings()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Matroska,
    WebM,
}

impl Container {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
            Self::WebM => "webm",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoEncoder {
    H264,
    H265,
    Av1,
    Vp9,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioEncoder {
    Aac,
    Opus,
    Copy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EncoderSpeed {
    Fast,
    Balanced,
    Quality,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Preset {
    Fast1080p,
    Balanced,
    HighQuality,
    SmallAv1,
}

impl Preset {
    pub const ALL: [Self; 4] = [
        Self::Fast1080p,
        Self::Balanced,
        Self::HighQuality,
        Self::SmallAv1,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fast1080p => "Fast 1080p",
            Self::Balanced => "Balanced",
            Self::HighQuality => "High quality",
            Self::SmallAv1 => "Small AV1",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Fast1080p => "Quick H.264 encode, capped at 1080p",
            Self::Balanced => "H.264 with a practical quality/speed balance",
            Self::HighQuality => "Slower H.265 encode with higher visual quality",
            Self::SmallAv1 => "Slow AV1 encode for compact archival files",
        }
    }

    #[must_use]
    pub const fn settings(self) -> EncodeSettings {
        match self {
            Self::Fast1080p => EncodeSettings {
                container: Container::Mp4,
                video_encoder: VideoEncoder::H264,
                quality: 24,
                speed: EncoderSpeed::Fast,
                audio_encoder: AudioEncoder::Aac,
                audio_bitrate_kbps: 160,
                max_width: Some(1920),
                max_height: Some(1080),
            },
            Self::Balanced => EncodeSettings {
                container: Container::Mp4,
                video_encoder: VideoEncoder::H264,
                quality: 21,
                speed: EncoderSpeed::Balanced,
                audio_encoder: AudioEncoder::Aac,
                audio_bitrate_kbps: 192,
                max_width: None,
                max_height: None,
            },
            Self::HighQuality => EncodeSettings {
                container: Container::Matroska,
                video_encoder: VideoEncoder::H265,
                quality: 18,
                speed: EncoderSpeed::Quality,
                audio_encoder: AudioEncoder::Opus,
                audio_bitrate_kbps: 192,
                max_width: None,
                max_height: None,
            },
            Self::SmallAv1 => EncodeSettings {
                container: Container::Matroska,
                video_encoder: VideoEncoder::Av1,
                quality: 30,
                speed: EncoderSpeed::Quality,
                audio_encoder: AudioEncoder::Opus,
                audio_bitrate_kbps: 128,
                max_width: None,
                max_height: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_inverted_encode_range() {
        let request = EncodeRequest {
            input: "source.mkv".into(),
            output: "clip.mp4".into(),
            source_duration_secs: 60.0,
            range: EncodeRange::new(30.0, 20.0),
            settings: EncodeSettings::default(),
        };
        assert!(request.validate().is_err());
    }
}
