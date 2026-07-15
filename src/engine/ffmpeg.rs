use std::{
    collections::HashMap,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde::Deserialize;

use crate::{
    EncodeRequest, Error, MediaInfo, Result,
    model::{AudioEncoder, EncoderSpeed, MediaStream, StreamKind, VideoEncoder},
};

use super::{EngineEvent, MediaEngine};

#[derive(Debug, Clone)]
pub struct FfmpegEngine {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl Default for FfmpegEngine {
    fn default() -> Self {
        Self {
            ffmpeg: std::env::var_os("CARBON_DISC_FFMPEG")
                .map_or_else(|| "ffmpeg".into(), PathBuf::from),
            ffprobe: std::env::var_os("CARBON_DISC_FFPROBE")
                .map_or_else(|| "ffprobe".into(), PathBuf::from),
        }
    }
}

impl FfmpegEngine {
    #[must_use]
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    #[must_use]
    pub fn encode_args(request: &EncodeRequest) -> Vec<OsString> {
        let settings = &request.settings;
        let video_codec = match settings.video_encoder {
            VideoEncoder::H264 => "libx264",
            VideoEncoder::H265 => "libx265",
            VideoEncoder::Av1 => "libsvtav1",
            VideoEncoder::Vp9 => "libvpx-vp9",
        };
        let preset = match (settings.video_encoder, settings.speed) {
            (VideoEncoder::Av1, EncoderSpeed::Fast) => "10",
            (VideoEncoder::Av1, EncoderSpeed::Balanced) => "7",
            (VideoEncoder::Av1, EncoderSpeed::Quality) => "5",
            (_, EncoderSpeed::Fast) => "veryfast",
            (_, EncoderSpeed::Balanced) => "medium",
            (_, EncoderSpeed::Quality) => "slow",
        };
        let mut args = vec!["-hide_banner".into(), "-y".into()];
        if request.range.start_secs > 0.0 {
            args.extend([
                OsString::from("-ss"),
                format!("{:.3}", request.range.start_secs).into(),
            ]);
        }
        args.extend([OsString::from("-i"), request.input.as_os_str().into()]);
        if request.range.start_secs > 0.0 || request.range.end_secs.is_some() {
            args.extend([
                OsString::from("-t"),
                format!("{:.3}", request.output_duration_secs()).into(),
            ]);
        }
        args.extend([
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a?".into(),
            "-c:v".into(),
            video_codec.into(),
        ]);
        if settings.video_encoder == VideoEncoder::Vp9 {
            let cpu_used = match settings.speed {
                EncoderSpeed::Fast => "6",
                EncoderSpeed::Balanced => "4",
                EncoderSpeed::Quality => "2",
            };
            args.extend([
                OsString::from("-crf"),
                settings.quality.to_string().into(),
                OsString::from("-b:v"),
                OsString::from("0"),
                OsString::from("-deadline"),
                OsString::from("good"),
                OsString::from("-cpu-used"),
                cpu_used.into(),
            ]);
        } else {
            args.extend([
                OsString::from("-crf"),
                settings.quality.to_string().into(),
                OsString::from("-preset"),
                preset.into(),
            ]);
        }
        match settings.audio_encoder {
            AudioEncoder::Copy => args.extend([OsString::from("-c:a"), OsString::from("copy")]),
            encoder => {
                let codec = if encoder == AudioEncoder::Aac {
                    "aac"
                } else {
                    "libopus"
                };
                args.extend([
                    OsString::from("-c:a"),
                    codec.into(),
                    OsString::from("-b:a"),
                    format!("{}k", settings.audio_bitrate_kbps).into(),
                ]);
            }
        }
        if settings.max_width.is_some() || settings.max_height.is_some() {
            let width = settings
                .max_width
                .map_or_else(|| "iw".into(), |v| v.to_string());
            let height = settings
                .max_height
                .map_or_else(|| "ih".into(), |v| v.to_string());
            args.extend([OsString::from("-vf"), format!("scale='min({width},iw)':'min({height},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2").into()]);
        }
        if request.settings.container == crate::model::Container::Mp4 {
            args.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
        }
        args.extend([
            OsString::from("-progress"),
            OsString::from("pipe:1"),
            OsString::from("-nostats"),
            request.output.as_os_str().into(),
        ]);
        args
    }
}

impl MediaEngine for FfmpegEngine {
    fn name(&self) -> &'static str {
        "FFmpeg"
    }

    fn probe(&self, path: &Path) -> Result<MediaInfo> {
        let output = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .map_err(|source| Error::Launch {
                program: self.ffprobe.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(Error::Process {
                program: self.ffprobe.display().to_string(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        parse_probe(path, &output.stdout)
    }

    fn encode(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<()> {
        request.validate()?;
        let mut child = Command::new(&self.ffmpeg)
            .args(Self::encode_args(request))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|source| Error::Launch {
                program: self.ffmpeg.clone(),
                source,
            })?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Process {
            program: self.ffmpeg.display().to_string(),
            message: "progress pipe was unavailable".into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| Error::Process {
            program: self.ffmpeg.display().to_string(),
            message: "diagnostic pipe was unavailable".into(),
        })?;
        let stderr_thread = thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut text);
            text
        });
        let mut progress = HashMap::new();
        for line in BufReader::new(stdout).lines() {
            if cancelled.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_thread.join();
                return Err(Error::Cancelled);
            }
            let line = line?;
            if let Some((key, value)) = line.split_once('=') {
                progress.insert(key.to_owned(), value.to_owned());
            }
            if line == "progress=continue" || line == "progress=end" {
                emit(progress_event(&progress, request.output_duration_secs()));
                progress.clear();
            }
        }
        while child.try_wait()?.is_none() {
            if cancelled.load(Ordering::Relaxed) {
                let _ = child.kill();
            }
            thread::sleep(Duration::from_millis(50));
        }
        let status = child.wait()?;
        let stderr = stderr_thread
            .join()
            .unwrap_or_else(|_| "FFmpeg diagnostic reader failed".into());
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if !status.success() {
            return Err(Error::Process {
                program: self.ffmpeg.display().to_string(),
                message: tail(&stderr, 20),
            });
        }
        emit(EngineEvent::Progress {
            fraction: 1.0,
            frame: None,
            speed: None,
        });
        Ok(())
    }
}

fn progress_event(values: &HashMap<String, String>, duration_secs: f64) -> EngineEvent {
    let micros = values
        .get("out_time_us")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fraction = if duration_secs > 0.0 {
        (micros / 1_000_000.0 / duration_secs).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let frame = values.get("frame").and_then(|value| value.parse().ok());
    let speed = values
        .get("speed")
        .and_then(|value| value.trim_end_matches('x').parse().ok());
    EngineEvent::Progress {
        fraction,
        frame,
        speed,
    }
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<_> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}
#[derive(Deserialize)]
struct ProbeFormat {
    #[serde(default)]
    format_name: String,
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
}
#[derive(Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    tags: Option<HashMap<String, String>>,
}

fn parse_probe(path: &Path, bytes: &[u8]) -> Result<MediaInfo> {
    let probe: Probe = serde_json::from_slice(bytes)?;
    let format = probe.format;
    Ok(MediaInfo {
        path: path.to_owned(),
        format_name: format
            .as_ref()
            .map_or_else(String::new, |value| value.format_name.clone()),
        duration_secs: format
            .as_ref()
            .and_then(|value| value.duration.as_deref())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0),
        size_bytes: format
            .as_ref()
            .and_then(|value| value.size.as_deref())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        bitrate: format
            .as_ref()
            .and_then(|value| value.bit_rate.as_deref())
            .and_then(|value| value.parse().ok()),
        streams: probe
            .streams
            .into_iter()
            .map(|stream| MediaStream {
                index: stream.index,
                kind: match stream.codec_type.as_deref() {
                    Some("video") => StreamKind::Video,
                    Some("audio") => StreamKind::Audio,
                    Some("subtitle") => StreamKind::Subtitle,
                    Some("data") => StreamKind::Data,
                    Some("attachment") => StreamKind::Attachment,
                    _ => StreamKind::Unknown,
                },
                codec: stream.codec_name.unwrap_or_else(|| "unknown".into()),
                width: stream.width,
                height: stream.height,
                channels: stream.channels,
                language: tag(&stream.tags, "language"),
                title: tag(&stream.tags, "title"),
            })
            .collect(),
    })
}

fn tag(tags: &Option<HashMap<String, String>>, key: &str) -> Option<String> {
    tags.as_ref()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EncodeSettings, Preset};

    #[test]
    fn parses_probe_json() {
        let info = parse_probe(Path::new("movie.mkv"), br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"tags":{"language":"eng"}}],"format":{"format_name":"matroska","duration":"12.5","size":"1000","bit_rate":"640"}}"#).expect("valid fixture");
        assert_eq!(info.duration_secs, 12.5);
        assert_eq!(info.video_stream().and_then(|s| s.width), Some(1920));
        assert_eq!(
            info.audio_streams()
                .next()
                .and_then(|s| s.language.as_deref()),
            Some("eng")
        );
    }

    #[test]
    fn command_is_generated_only_in_adapter() {
        let request = EncodeRequest {
            input: "in file.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 10.0,
            range: crate::EncodeRange::ALL,
            settings: EncodeSettings::default(),
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(
            args.iter()
                .any(|arg| arg == std::ffi::OsStr::new("in file.mkv"))
        );
        assert!(args.windows(2).any(|pair| pair
            == [
                std::ffi::OsStr::new("-c:v"),
                std::ffi::OsStr::new("libx264"),
            ]));
        assert_eq!(request.settings, Preset::Balanced.settings());
    }

    #[test]
    fn trim_range_becomes_seek_and_duration_arguments() {
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 60.0,
            range: crate::EncodeRange::new(12.5, 42.75),
            settings: EncodeSettings::default(),
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(args.windows(2).any(|pair| pair == ["-ss", "12.500"]));
        assert!(args.windows(2).any(|pair| pair == ["-t", "30.250"]));
        assert_eq!(request.output_duration_secs(), 30.25);
    }

    #[test]
    fn progress_is_normalized() {
        let values = HashMap::from([
            ("out_time_us".into(), "5000000".into()),
            ("speed".into(), "2.5x".into()),
        ]);
        assert_eq!(
            progress_event(&values, 10.0),
            EngineEvent::Progress {
                fraction: 0.5,
                frame: None,
                speed: Some(2.5)
            }
        );
    }
}
