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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomPreset {
    pub name: String,
    pub description: String,
    pub settings: EncodeSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineCapabilities {
    pub video_encoders: Vec<VideoEncoder>,
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
    pub source_width: Option<u32>,
    pub source_height: Option<u32>,
    pub range: EncodeRange,
    pub audio_tracks: Vec<AudioTrackSettings>,
    pub overwrite: bool,
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
        if let (Some(width), Some(height)) = (self.source_width, self.source_height)
            && (self
                .settings
                .crop
                .left
                .saturating_add(self.settings.crop.right)
                >= width
                || self
                    .settings
                    .crop
                    .top
                    .saturating_add(self.settings.crop.bottom)
                    >= height)
        {
            return Err(crate::Error::InvalidRequest(
                "crop margins remove the entire picture".into(),
            ));
        }
        if !self
            .settings
            .container
            .supports_video(self.settings.video_encoder)
        {
            return Err(crate::Error::InvalidRequest(format!(
                "{} cannot hold {} video",
                self.settings.container.display_name(),
                self.settings.video_encoder.display_name()
            )));
        }
        if self.settings.max_width == Some(0) || self.settings.max_height == Some(0) {
            return Err(crate::Error::InvalidRequest(
                "output dimensions must be greater than zero".into(),
            ));
        }
        let mut indices = std::collections::HashSet::new();
        for track in &self.audio_tracks {
            if !indices.insert(track.source_index) {
                return Err(crate::Error::InvalidRequest(format!(
                    "audio stream {} was selected more than once",
                    track.source_index
                )));
            }
            if !self.settings.container.supports_audio(track.encoder) {
                return Err(crate::Error::InvalidRequest(format!(
                    "{} cannot hold {} audio",
                    self.settings.container.display_name(),
                    track.encoder.display_name()
                )));
            }
            if track.encoder != AudioEncoder::Copy && track.bitrate_kbps == 0 {
                return Err(crate::Error::InvalidRequest(
                    "audio bitrate must be greater than zero".into(),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn output_duration_secs(&self) -> f64 {
        self.range.duration_secs(self.source_duration_secs)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioTrackSettings {
    pub source_index: u32,
    pub encoder: AudioEncoder,
    pub bitrate_kbps: u16,
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
    pub crop: CropMargins,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CropMargins {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl CropMargins {
    pub const ZERO: Self = Self {
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
    };

    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.top == 0 && self.bottom == 0 && self.left == 0 && self.right == 0
    }
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
    pub const ALL: [Self; 3] = [Self::Mp4, Self::Matroska, Self::WebM];

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Matroska => "MKV",
            Self::WebM => "WebM",
        }
    }

    /// WebM is restricted to royalty-free codecs; MP4 and Matroska accept everything we encode.
    #[must_use]
    pub const fn supports_video(self, encoder: VideoEncoder) -> bool {
        match self {
            Self::WebM => matches!(
                encoder,
                VideoEncoder::Vp9
                    | VideoEncoder::Av1
                    | VideoEncoder::NvencAv1
                    | VideoEncoder::QsvAv1
                    | VideoEncoder::AmfAv1
                    | VideoEncoder::VaapiAv1
            ),
            Self::Mp4 | Self::Matroska => true,
        }
    }

    #[must_use]
    pub const fn supports_audio(self, encoder: AudioEncoder) -> bool {
        !matches!((self, encoder), (Self::WebM, AudioEncoder::Aac))
    }

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
    NvencH264,
    NvencH265,
    NvencAv1,
    QsvH264,
    QsvH265,
    QsvAv1,
    AmfH264,
    AmfH265,
    AmfAv1,
    VaapiH264,
    VaapiH265,
    VaapiAv1,
    VideoToolboxH264,
    VideoToolboxH265,
}

impl VideoEncoder {
    pub const ALL: [Self; 18] = [
        Self::H264,
        Self::H265,
        Self::Av1,
        Self::Vp9,
        Self::NvencH264,
        Self::NvencH265,
        Self::NvencAv1,
        Self::QsvH264,
        Self::QsvH265,
        Self::QsvAv1,
        Self::AmfH264,
        Self::AmfH265,
        Self::AmfAv1,
        Self::VaapiH264,
        Self::VaapiH265,
        Self::VaapiAv1,
        Self::VideoToolboxH264,
        Self::VideoToolboxH265,
    ];

    #[must_use]
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::Av1 => "libsvtav1",
            Self::Vp9 => "libvpx-vp9",
            Self::NvencH264 => "h264_nvenc",
            Self::NvencH265 => "hevc_nvenc",
            Self::NvencAv1 => "av1_nvenc",
            Self::QsvH264 => "h264_qsv",
            Self::QsvH265 => "hevc_qsv",
            Self::QsvAv1 => "av1_qsv",
            Self::AmfH264 => "h264_amf",
            Self::AmfH265 => "hevc_amf",
            Self::AmfAv1 => "av1_amf",
            Self::VaapiH264 => "h264_vaapi",
            Self::VaapiH265 => "hevc_vaapi",
            Self::VaapiAv1 => "av1_vaapi",
            Self::VideoToolboxH264 => "h264_videotoolbox",
            Self::VideoToolboxH265 => "hevc_videotoolbox",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::H264 => "H.264 (x264)",
            Self::H265 => "H.265 (x265)",
            Self::Av1 => "AV1 (SVT-AV1)",
            Self::Vp9 => "VP9 (libvpx)",
            Self::NvencH264 => "H.264 (NVIDIA NVENC)",
            Self::NvencH265 => "H.265 (NVIDIA NVENC)",
            Self::NvencAv1 => "AV1 (NVIDIA NVENC)",
            Self::QsvH264 => "H.264 (Intel Quick Sync)",
            Self::QsvH265 => "H.265 (Intel Quick Sync)",
            Self::QsvAv1 => "AV1 (Intel Quick Sync)",
            Self::AmfH264 => "H.264 (AMD AMF)",
            Self::AmfH265 => "H.265 (AMD AMF)",
            Self::AmfAv1 => "AV1 (AMD AMF)",
            Self::VaapiH264 => "H.264 (VAAPI)",
            Self::VaapiH265 => "H.265 (VAAPI)",
            Self::VaapiAv1 => "AV1 (VAAPI)",
            Self::VideoToolboxH264 => "H.264 (VideoToolbox)",
            Self::VideoToolboxH265 => "H.265 (VideoToolbox)",
        }
    }

    /// The quality scale the encoder's rate control accepts.
    #[must_use]
    pub const fn quality_range(self) -> std::ops::RangeInclusive<u8> {
        match self {
            Self::Av1 | Self::Vp9 => 0..=63,
            Self::VideoToolboxH264 | Self::VideoToolboxH265 => 1..=100,
            _ => 0..=51,
        }
    }

    /// VideoToolbox uses a 1–100 scale where higher is better; everything else is a
    /// CRF/QP-style scale where lower is better.
    #[must_use]
    pub const fn higher_quality_is_better(self) -> bool {
        matches!(self, Self::VideoToolboxH264 | Self::VideoToolboxH265)
    }

    #[must_use]
    pub const fn is_hardware(self) -> bool {
        !matches!(self, Self::H264 | Self::H265 | Self::Av1 | Self::Vp9)
    }

    #[must_use]
    pub const fn is_vaapi(self) -> bool {
        matches!(self, Self::VaapiH264 | Self::VaapiH265 | Self::VaapiAv1)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioEncoder {
    Aac,
    Opus,
    Copy,
}

impl AudioEncoder {
    pub const ALL: [Self; 3] = [Self::Aac, Self::Opus, Self::Copy];

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::Copy => "Passthrough",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EncoderSpeed {
    Fast,
    Balanced,
    Quality,
}

impl EncoderSpeed {
    pub const ALL: [Self; 3] = [Self::Fast, Self::Balanced, Self::Quality];

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::Quality => "Slow",
        }
    }
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
                crop: CropMargins::ZERO,
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
                crop: CropMargins::ZERO,
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
                crop: CropMargins::ZERO,
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
                crop: CropMargins::ZERO,
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
            source_width: Some(1920),
            source_height: Some(1080),
            range: EncodeRange::new(30.0, 20.0),
            audio_tracks: Vec::new(),
            overwrite: false,
            settings: EncodeSettings::default(),
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn rejects_crop_that_removes_the_picture() {
        let mut request = EncodeRequest {
            input: "source.mkv".into(),
            output: "clip.mp4".into(),
            source_duration_secs: 60.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: EncodeRange::ALL,
            audio_tracks: Vec::new(),
            overwrite: false,
            settings: EncodeSettings::default(),
        };
        request.settings.crop.left = 1920;
        assert!(request.validate().is_err());
    }

    #[test]
    fn rejects_codecs_the_container_cannot_hold() {
        let mut request = EncodeRequest {
            input: "source.mkv".into(),
            output: "clip.webm".into(),
            source_duration_secs: 60.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: EncodeRange::ALL,
            audio_tracks: Vec::new(),
            overwrite: false,
            settings: EncodeSettings::default(),
        };
        request.settings.container = Container::WebM;
        assert!(request.validate().is_err());
        request.settings.video_encoder = VideoEncoder::Vp9;
        assert!(request.validate().is_ok());
        request.audio_tracks.push(AudioTrackSettings {
            source_index: 1,
            encoder: AudioEncoder::Aac,
            bitrate_kbps: 160,
        });
        assert!(request.validate().is_err());
    }
}
