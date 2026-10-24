use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

use crate::{
    CropMargins, EncodeRange, EncodeRequest, EngineCapabilities, Error, MediaInfo, PreviewFrame,
    Result, SizeMeasurement,
    model::{AudioEncoder, EncoderSpeed, MediaStream, StreamKind, VideoEncoder},
};

use super::{EngineEvent, MediaEngine};

#[derive(Debug, Clone)]
pub struct FfmpegEngine {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

/// `ffmpeg`/`ffprobe` sitting in `dir`, matching the platform's executable suffix.
fn tool_beside(dir: &Path, name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let candidate = dir.join(file);
    candidate.is_file().then_some(candidate)
}

/// Places a freshly installed FFmpeg can appear that `PATH` misses, because
/// the running app inherited its environment before the install happened.
fn well_known_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    if cfg!(target_os = "windows")
        && let Some(local) = std::env::var_os("LOCALAPPDATA")
    {
        dirs.push(PathBuf::from(local).join(r"Microsoft\WinGet\Links"));
    }
    dirs
}

/// Resolution order: an explicit environment override, FFmpeg shipped beside the
/// app (release bundles), a downloaded copy in the app-data tools folder,
/// well-known package-manager locations, and finally whatever `PATH` provides.
fn resolve_tool(env_var: &str, name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_var) {
        return PathBuf::from(path);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let mut dirs: Vec<PathBuf> = exe_dir.into_iter().collect();
    if let Ok(tools) = crate::persistence::tools_dir() {
        dirs.push(tools);
    }
    dirs.extend(well_known_dirs());
    for dir in &dirs {
        if let Some(bundled) = tool_beside(dir, name) {
            return bundled;
        }
    }
    PathBuf::from(name)
}

impl Default for FfmpegEngine {
    fn default() -> Self {
        Self {
            ffmpeg: resolve_tool("CARBON_DISC_FFMPEG", "ffmpeg"),
            ffprobe: resolve_tool("CARBON_DISC_FFPROBE", "ffprobe"),
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
        Self::encode_args_for_output(request, &request.output)
    }

    #[must_use]
    pub fn encode_args_for_output(request: &EncodeRequest, output: &Path) -> Vec<OsString> {
        let settings = &request.settings;
        let video_codec = settings.video_encoder.ffmpeg_name();
        let preset = match (settings.video_encoder, settings.speed) {
            (VideoEncoder::Av1, EncoderSpeed::Fast) => "10",
            (VideoEncoder::Av1, EncoderSpeed::Balanced) => "7",
            (VideoEncoder::Av1, EncoderSpeed::Quality) => "5",
            (_, EncoderSpeed::Fast) => "veryfast",
            (_, EncoderSpeed::Balanced) => "medium",
            (_, EncoderSpeed::Quality) => "slow",
        };
        let mut args = vec!["-hide_banner".into(), "-y".into()];
        if settings.video_encoder.is_vaapi() {
            args.extend([
                OsString::from("-vaapi_device"),
                OsString::from("/dev/dri/renderD128"),
            ]);
        }
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
        args.extend(["-map".into(), "0:v:0".into()]);
        for track in &request.audio_tracks {
            args.extend([
                OsString::from("-map"),
                format!("0:{}", track.source_index).into(),
            ]);
        }
        args.extend([OsString::from("-c:v"), video_codec.into()]);
        match settings.video_encoder {
            VideoEncoder::H264 | VideoEncoder::H265 | VideoEncoder::Av1 => {
                args.extend([
                    OsString::from("-crf"),
                    settings.quality.to_string().into(),
                    OsString::from("-preset"),
                    preset.into(),
                ]);
            }
            VideoEncoder::Vp9 => {
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
            }
            VideoEncoder::NvencH264 | VideoEncoder::NvencH265 | VideoEncoder::NvencAv1 => {
                let preset = match settings.speed {
                    EncoderSpeed::Fast => "p3",
                    EncoderSpeed::Balanced => "p5",
                    EncoderSpeed::Quality => "p7",
                };
                args.extend([
                    OsString::from("-rc"),
                    OsString::from("vbr"),
                    OsString::from("-cq"),
                    settings.quality.to_string().into(),
                    OsString::from("-b:v"),
                    OsString::from("0"),
                    OsString::from("-preset"),
                    preset.into(),
                ]);
            }
            VideoEncoder::QsvH264 | VideoEncoder::QsvH265 | VideoEncoder::QsvAv1 => {
                args.extend([
                    OsString::from("-global_quality"),
                    settings.quality.to_string().into(),
                    OsString::from("-preset"),
                    preset.into(),
                ]);
            }
            VideoEncoder::AmfH264 | VideoEncoder::AmfH265 | VideoEncoder::AmfAv1 => {
                let quality = match settings.speed {
                    EncoderSpeed::Fast => "speed",
                    EncoderSpeed::Balanced => "balanced",
                    EncoderSpeed::Quality => "quality",
                };
                args.extend([
                    OsString::from("-quality"),
                    quality.into(),
                    OsString::from("-qp_i"),
                    settings.quality.to_string().into(),
                    OsString::from("-qp_p"),
                    settings.quality.to_string().into(),
                ]);
            }
            VideoEncoder::VaapiH264 | VideoEncoder::VaapiH265 | VideoEncoder::VaapiAv1 => {
                args.extend([OsString::from("-qp"), settings.quality.to_string().into()]);
            }
            VideoEncoder::VideoToolboxH264 | VideoEncoder::VideoToolboxH265 => {
                args.extend([OsString::from("-q:v"), settings.quality.to_string().into()]);
            }
        }
        if let Some(frame_rate) = settings.frame_rate {
            args.extend([OsString::from("-r"), frame_rate.to_string().into()]);
        }
        for (output_index, track) in request.audio_tracks.iter().enumerate() {
            let codec_option = format!("-c:a:{output_index}");
            match track.encoder {
                AudioEncoder::Copy => {
                    args.extend([codec_option.into(), OsString::from("copy")]);
                }
                encoder => {
                    let codec = if encoder == AudioEncoder::Aac {
                        "aac"
                    } else {
                        "libopus"
                    };
                    args.extend([
                        codec_option.into(),
                        codec.into(),
                        format!("-b:a:{output_index}").into(),
                        format!("{}k", track.bitrate_kbps).into(),
                    ]);
                }
            }
        }
        let mut filters = vec!["scale='trunc(if(gt(sar,0),iw*sar,iw)/2)*2':'ih',setsar=1".into()];
        if !settings.crop.is_zero() {
            filters.push(format!(
                "crop=iw-{}:ih-{}:{}:{}",
                settings.crop.left + settings.crop.right,
                settings.crop.top + settings.crop.bottom,
                settings.crop.left,
                settings.crop.top
            ));
        }
        if settings.max_width.is_some() || settings.max_height.is_some() {
            let width = settings
                .max_width
                .map_or_else(|| "iw".into(), |v| v.to_string());
            let height = settings
                .max_height
                .map_or_else(|| "ih".into(), |v| v.to_string());
            filters.push(format!("scale='min({width},iw)':'min({height},ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"));
        }
        if settings.video_encoder.is_vaapi() {
            filters.push("format=nv12,hwupload".into());
        }
        if !filters.is_empty() {
            args.extend([OsString::from("-vf"), filters.join(",").into()]);
        }
        if request.settings.container == crate::model::Container::Mp4 {
            args.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
        }
        args.extend([
            OsString::from("-progress"),
            OsString::from("pipe:1"),
            OsString::from("-nostats"),
            output.as_os_str().into(),
        ]);
        args
    }

    #[must_use]
    pub fn crop_detect_args(path: &Path, at_secs: f64) -> Vec<OsString> {
        vec![
            "-hide_banner".into(),
            "-ss".into(),
            format!("{:.3}", at_secs.max(0.0)).into(),
            "-i".into(),
            path.as_os_str().into(),
            "-frames:v".into(),
            "8".into(),
            "-an".into(),
            "-sn".into(),
            "-dn".into(),
            "-vf".into(),
            "cropdetect=limit=24:round=2:reset=0".into(),
            "-f".into(),
            "null".into(),
            "-".into(),
        ]
    }

    #[must_use]
    pub fn preview_args(
        path: &Path,
        at_secs: f64,
        max_width: u32,
        display_aspect: f64,
        fast_seek: bool,
    ) -> Vec<OsString> {
        let mut args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-ss".into(),
            format!("{at_secs:.3}").into(),
        ];
        if fast_seek {
            args.push("-noaccurate_seek".into());
        }
        args.extend([
            OsString::from("-i"),
            path.as_os_str().into(),
            OsString::from("-frames:v"),
            OsString::from("1"),
            OsString::from("-an"),
            OsString::from("-sn"),
            OsString::from("-dn"),
            OsString::from("-vf"),
            format!(
                "scale={}:{},format=rgb24",
                max_width.max(16),
                preview_height(max_width.max(16), display_aspect)
            )
            .into(),
            OsString::from("-f"),
            OsString::from("image2pipe"),
            OsString::from("-vcodec"),
            OsString::from("ppm"),
            OsString::from("pipe:1"),
        ]);
        args
    }

    fn encoder_initializes(&self, encoder: VideoEncoder) -> bool {
        if encoder.is_vaapi() && !Path::new("/dev/dri/renderD128").exists() {
            return false;
        }
        let mut command = Command::new(&self.ffmpeg);
        command.args(["-hide_banner", "-loglevel", "error"]);
        if encoder.is_vaapi() {
            command.args(["-vaapi_device", "/dev/dri/renderD128"]);
        }
        command.args([
            "-f",
            "lavfi",
            "-i",
            "color=size=64x64:rate=1",
            "-frames:v",
            "1",
        ]);
        if encoder.is_vaapi() {
            command.args(["-vf", "format=nv12,hwupload"]);
        }
        let Ok(mut child) = command
            .args(["-an", "-c:v", encoder.ffmpeg_name(), "-f", "null", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    /// Encodes one sample segment to `output` and reports its size and duration.
    fn run_sample(
        &self,
        request: &EncodeRequest,
        output: &Path,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<(u64, f64)> {
        let mut child = Command::new(&self.ffmpeg)
            .args(Self::encode_args_for_output(request, output))
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
        // The requested sample length is only nominal: seeking lands on a keyframe, so
        // the encoded duration is read back from FFmpeg's own progress output.
        let mut out_secs = 0.0;
        for line in BufReader::new(stdout).lines() {
            if cancelled.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_thread.join();
                return Err(Error::Cancelled);
            }
            let line = line?;
            if let Some((key, value)) = line.split_once('=')
                && key == "out_time_us"
                && let Ok(micros) = value.parse::<f64>()
            {
                out_secs = micros / 1_000_000.0;
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
        let size = std::fs::metadata(output).map_or(0, |metadata| metadata.len());
        Ok((size, out_secs))
    }

    /// Sums the encoded video packet sizes in a sample output, so video bytes can
    /// be separated from passthrough audio muxed into the same file.
    fn video_packet_bytes(&self, path: &Path) -> Result<u64> {
        let output = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=size",
                "-of",
                "csv=p=0",
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
                message: tail(&String::from_utf8_lossy(&output.stderr), 5),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u64>().ok())
            .sum())
    }

    /// Measures how many quality steps halve the video size, by re-encoding one
    /// already-measured segment at a shifted quality. Same content, same encoder:
    /// the size ratio is this video's own quality→size curve. Anything unexpected
    /// falls back to the theoretical QP-scale default instead of failing.
    fn calibrate_quality_steps(
        &self,
        request: &EncodeRequest,
        segment: (f64, f64),
        segment_video_bytes: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<f64> {
        let range = request.settings.video_encoder.quality_range();
        let quality = i16::from(request.settings.quality);
        // Shift towards smaller files, which also encodes faster.
        let shift: i16 = if request.settings.video_encoder.higher_quality_is_better() {
            -20
        } else {
            6
        };
        let mut probe = quality + shift;
        if !range.contains(&(probe.clamp(i16::from(*range.start()), i16::from(*range.end())) as u8))
        {
            probe = quality - shift;
        }
        probe = probe.clamp(i16::from(*range.start()), i16::from(*range.end()));
        if probe == quality || segment_video_bytes == 0 {
            return Ok(DEFAULT_QUALITY_STEPS);
        }
        let mut sample = sample_request(request, segment.0, segment.1);
        sample.settings.quality = probe as u8;
        sample.audio_tracks.clear();
        let output = std::env::temp_dir().join(format!(
            "carbon-disc-estimate-{}-calibration.{}",
            std::process::id(),
            request.settings.container.extension(),
        ));
        let result = self.run_sample(&sample, &output, cancelled);
        let _ = std::fs::remove_file(&output);
        let (probe_bytes, _) = match result {
            Ok(measured) if measured.0 > 0 && measured.1 > 0.1 => measured,
            Ok(_) => return Ok(DEFAULT_QUALITY_STEPS),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => return Ok(DEFAULT_QUALITY_STEPS),
        };
        let ratio = segment_video_bytes as f64 / probe_bytes as f64;
        if ratio <= 1.02 {
            return Ok(DEFAULT_QUALITY_STEPS);
        }
        Ok(((quality - probe).unsigned_abs() as f64 / ratio.log2()).clamp(3.0, 24.0))
    }
}

impl MediaEngine for FfmpegEngine {
    fn name(&self) -> &'static str {
        "FFmpeg"
    }

    fn capabilities(&self) -> Result<EngineCapabilities> {
        let output = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    Error::ToolsMissing(self.ffmpeg.clone())
                } else {
                    Error::Launch {
                        program: self.ffmpeg.clone(),
                        source,
                    }
                }
            })?;
        if !output.status.success() {
            return Err(Error::Process {
                program: self.ffmpeg.display().to_string(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let names: HashSet<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let flags = fields.next()?;
                let name = fields.next()?;
                flags.starts_with('V').then_some(name.to_owned())
            })
            .collect();
        let video_encoders = VideoEncoder::ALL
            .into_iter()
            .filter(|encoder| names.contains(encoder.ffmpeg_name()))
            .filter(|encoder| !encoder.is_hardware() || self.encoder_initializes(*encoder))
            .collect();
        Ok(EngineCapabilities { video_encoders })
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

    fn preview(
        &self,
        path: &Path,
        at_secs: f64,
        max_width: u32,
        display_aspect: f64,
        fast_seek: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreviewFrame> {
        if !at_secs.is_finite() || at_secs < 0.0 {
            return Err(Error::InvalidRequest(
                "preview position must be a positive time".into(),
            ));
        }
        let mut child = Command::new(&self.ffmpeg)
            .args(Self::preview_args(
                path,
                at_secs,
                max_width,
                display_aspect,
                fast_seek,
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| Error::Launch {
                program: self.ffmpeg.clone(),
                source,
            })?;
        let mut stdout = child.stdout.take().ok_or_else(|| Error::Process {
            program: self.ffmpeg.display().to_string(),
            message: "preview output pipe was unavailable".into(),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| Error::Process {
            program: self.ffmpeg.display().to_string(),
            message: "preview diagnostic pipe was unavailable".into(),
        })?;
        let stdout_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let status = loop {
            if cancelled.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Error::Cancelled);
            }
            match child.try_wait()? {
                Some(status) => break status,
                None => thread::sleep(Duration::from_millis(8)),
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| Error::InvalidPreview("preview reader stopped unexpectedly".into()))??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| Error::InvalidPreview("preview reader stopped unexpectedly".into()))??;
        if !status.success() {
            return Err(Error::Process {
                program: self.ffmpeg.display().to_string(),
                message: String::from_utf8_lossy(&stderr).trim().to_owned(),
            });
        }
        parse_ppm(&stdout)
    }

    fn detect_crop(
        &self,
        path: &Path,
        duration_secs: f64,
        width: u32,
        height: u32,
    ) -> Result<CropMargins> {
        let mut combined: Option<CropMargins> = None;
        for fraction in [0.1, 0.3, 0.5, 0.7, 0.9] {
            let output = Command::new(&self.ffmpeg)
                .args(Self::crop_detect_args(path, duration_secs * fraction))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|source| Error::Launch {
                    program: self.ffmpeg.clone(),
                    source,
                })?;
            let log = String::from_utf8_lossy(&output.stderr);
            // Fully black frames report nonsense crops and are skipped.
            if let Some(sample) = parse_cropdetect(&log, width, height) {
                combined = Some(combined.map_or(sample, |crop| CropMargins {
                    top: crop.top.min(sample.top),
                    bottom: crop.bottom.min(sample.bottom),
                    left: crop.left.min(sample.left),
                    right: crop.right.min(sample.right),
                }));
            }
        }
        Ok(combined.unwrap_or(CropMargins::ZERO))
    }

    fn estimate_size(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<SizeMeasurement> {
        request.range.validate(request.source_duration_secs)?;
        let segments = sample_segments(request.range, request.source_duration_secs);
        let passthrough = request
            .audio_tracks
            .iter()
            .any(|track| track.encoder == AudioEncoder::Copy);
        // The sample encodes are independent, so they run in parallel: wall time
        // is roughly one sample instead of all of them added up.
        let jobs: Vec<_> = segments
            .iter()
            .enumerate()
            .map(|(index, (start, length))| {
                let sample = sample_request(request, *start, *length);
                let output = std::env::temp_dir().join(format!(
                    "carbon-disc-estimate-{}-{index}.{}",
                    std::process::id(),
                    request.settings.container.extension(),
                ));
                (index, sample, output)
            })
            .collect();
        let results: Vec<_> = thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .iter()
                .map(|(index, sample, output)| {
                    let cancelled = cancelled.clone();
                    scope.spawn(move || (*index, self.run_sample(sample, output, &cancelled)))
                })
                .collect();
            let joined: Vec<_> = handles.into_iter().map(|handle| handle.join()).collect();
            joined
        })
        .into_iter()
        .map(|joined| match joined {
            Ok(value) => value,
            Err(_) => (
                0,
                Err(Error::Process {
                    program: self.ffmpeg.display().to_string(),
                    message: "a sample encode thread stopped unexpectedly".into(),
                }),
            ),
        })
        .collect();
        let mut video_bytes = 0u64;
        let mut copy_bytes = 0u64;
        let mut seconds = 0.0;
        let mut calibration: Option<((f64, f64), u64)> = None;
        let steps = segments.len() + 1;
        for (position, (index, result)) in results.into_iter().enumerate() {
            let output = &jobs[position].2;
            // Video bytes must be separated from muxed passthrough audio before the
            // file is deleted, because only video follows the quality scale.
            let sample_video = match &result {
                Ok((sample_bytes, _)) if passthrough => {
                    self.video_packet_bytes(output).unwrap_or(*sample_bytes)
                }
                Ok((sample_bytes, _)) => *sample_bytes,
                Err(_) => 0,
            };
            let _ = std::fs::remove_file(output);
            let (sample_bytes, sample_secs) = result?;
            if sample_bytes > 0 && sample_secs > 0.1 {
                video_bytes += sample_video;
                copy_bytes += sample_bytes - sample_video;
                seconds += sample_secs;
                if index == segments.len() / 2 {
                    let (start, length) = segments[index];
                    calibration = Some(((start, length), sample_video));
                }
            }
            emit(EngineEvent::Progress {
                fraction: (position + 1) as f32 / steps as f32,
                frame: None,
                speed: None,
            });
        }
        if seconds < 0.1 {
            return Err(Error::Process {
                program: self.ffmpeg.display().to_string(),
                message: "the sample encodes produced no output".into(),
            });
        }
        let quality_halving_steps = match calibration {
            Some((segment, segment_video_bytes)) => {
                self.calibrate_quality_steps(request, segment, segment_video_bytes, &cancelled)?
            }
            None => DEFAULT_QUALITY_STEPS,
        };
        emit(EngineEvent::Progress {
            fraction: 1.0,
            frame: None,
            speed: None,
        });
        Ok(SizeMeasurement {
            video_bytes_per_second: video_bytes as f64 / seconds,
            copy_audio_bytes_per_second: copy_bytes as f64 / seconds,
            quality: request.settings.quality,
            quality_halving_steps,
        })
    }

    fn encode(
        &self,
        request: &EncodeRequest,
        cancelled: Arc<AtomicBool>,
        emit: &mut dyn FnMut(EngineEvent),
    ) -> Result<()> {
        request.validate()?;
        if request.output.exists() && !request.overwrite {
            return Err(Error::OutputExists(request.output.clone()));
        }
        let temporary = TemporaryOutput::new(&request.output)?;
        let mut child = Command::new(&self.ffmpeg)
            .args(Self::encode_args_for_output(request, temporary.path()))
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
        temporary.commit(&request.output, request.overwrite)?;
        emit(EngineEvent::Progress {
            fraction: 1.0,
            frame: None,
            speed: None,
        });
        Ok(())
    }
}

struct TemporaryOutput {
    path: PathBuf,
    committed: bool,
}

impl TemporaryOutput {
    fn new(destination: &Path) -> Result<Self> {
        let stem = destination
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("output");
        let extension = destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let name = if extension.is_empty() {
            format!(".{stem}.carbon-disc-{}.part", std::process::id())
        } else {
            format!(
                ".{stem}.carbon-disc-{}.part.{extension}",
                std::process::id()
            )
        };
        let path = destination.with_file_name(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(Self {
            path,
            committed: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(mut self, destination: &Path, overwrite: bool) -> Result<()> {
        if !overwrite {
            match std::fs::hard_link(&self.path, destination) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&self.path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(Error::OutputExists(destination.to_owned()));
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            #[cfg(windows)]
            if destination.exists() {
                std::fs::remove_file(destination)?;
            }
            std::fs::rename(&self.path, destination)?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for TemporaryOutput {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Sample encodes are short and few, and they run in parallel: estimation
/// trades accuracy for speed, aiming for a few seconds of wall time even on
/// slow software encoders.
const SAMPLE_SECS: f64 = 3.0;
const MAX_SAMPLES: usize = 3;
/// QP quantizers double every six steps, so quality scaling falls back to this
/// when the per-video calibration sample cannot run.
const DEFAULT_QUALITY_STEPS: f64 = 6.0;

/// Segments (start, seconds) to encode for a size estimate, spread across the
/// requested range. A range no longer than one sample is measured in full, which
/// makes the estimate exact.
fn sample_segments(range: EncodeRange, source_duration_secs: f64) -> Vec<(f64, f64)> {
    let duration = range.duration_secs(source_duration_secs);
    if duration <= SAMPLE_SECS {
        return vec![(range.start_secs, duration)];
    }
    let span = duration - SAMPLE_SECS;
    let count = ((duration / (SAMPLE_SECS * 10.0)) as usize).clamp(1, MAX_SAMPLES);
    (0..count)
        .map(|index| {
            let fraction = if count == 1 {
                0.5
            } else {
                index as f64 / (count - 1) as f64
            };
            (range.start_secs + span * fraction, SAMPLE_SECS)
        })
        .collect()
}

/// A copy of the request covering one sample segment, stripped of re-encoded audio
/// (which is sized exactly from its bitrate instead of sampled).
fn sample_request(request: &EncodeRequest, start: f64, length: f64) -> EncodeRequest {
    let mut sample = request.clone();
    sample
        .audio_tracks
        .retain(|track| track.encoder == AudioEncoder::Copy);
    sample.range = EncodeRange::new(start, start + length);
    sample
}

/// The preview frame height for a width and display aspect, rounded to even
/// pixels. Plain numbers instead of a filter expression: some frames report an
/// undefined aspect ratio, which makes `dar` fail to even evaluate.
fn preview_height(max_width: u32, display_aspect: f64) -> u32 {
    if !display_aspect.is_finite() || display_aspect <= 0.0 {
        return (max_width * 9 / 16) & !1;
    }
    ((f64::from(max_width) / display_aspect).round() as u32).max(2) & !1
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
    sample_aspect_ratio: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    side_data_list: Option<Vec<ProbeSideData>>,
    channels: Option<u32>,
    tags: Option<HashMap<String, String>>,
}

/// Parses ffprobe's `num/den` rate strings into a `(numerator, denominator)` pair.
fn parse_rate(rate: Option<&String>) -> Option<(u32, u32)> {
    let (num, den) = rate?.split_once('/')?;
    let num = num.trim().parse().ok()?;
    let den = den.trim().parse().ok()?;
    (num > 0 && den > 0).then_some((num, den))
}

#[derive(Deserialize)]
struct ProbeSideData {
    side_data_type: Option<String>,
    rotation: Option<f64>,
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
            .map(|stream| {
                let (width, height) = display_dimensions(&stream);
                MediaStream {
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
                    width,
                    height,
                    frame_rate: parse_rate(stream.avg_frame_rate.as_ref())
                        .or_else(|| parse_rate(stream.r_frame_rate.as_ref())),
                    channels: stream.channels,
                    language: tag(&stream.tags, "language"),
                    title: tag(&stream.tags, "title"),
                }
            })
            .collect(),
    })
}

fn parse_ppm(bytes: &[u8]) -> Result<PreviewFrame> {
    let mut cursor = 0;
    let magic = ppm_token(bytes, &mut cursor)?;
    if magic != b"P6" {
        return Err(Error::InvalidPreview("expected a binary PPM frame".into()));
    }
    let width = parse_ppm_number(ppm_token(bytes, &mut cursor)?, "width")?;
    let height = parse_ppm_number(ppm_token(bytes, &mut cursor)?, "height")?;
    let maximum = parse_ppm_number(ppm_token(bytes, &mut cursor)?, "color depth")?;
    if (maximum != 255 && maximum != 65535) || width == 0 || height == 0 {
        return Err(Error::InvalidPreview(
            "unsupported PPM dimensions or color depth".into(),
        ));
    }
    if bytes.get(cursor) == Some(&b'\r') {
        cursor += 1;
        if bytes.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }
    } else if bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    } else {
        return Err(Error::InvalidPreview("PPM header is incomplete".into()));
    }
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| Error::InvalidPreview("PPM dimensions overflow".into()))?;
    let bytes_per_sample = if maximum == 65535 { 2 } else { 1 };
    let rgb_len = pixel_count
        .checked_mul(3 * bytes_per_sample)
        .ok_or_else(|| Error::InvalidPreview("PPM frame is too large".into()))?;
    let raw = bytes
        .get(cursor..cursor + rgb_len)
        .ok_or_else(|| Error::InvalidPreview("PPM pixel data is truncated".into()))?;
    // 16-bit PPM samples are big-endian; previews only need 8 bits, so the low
    // byte of each sample is dropped.
    let rgb: Vec<u8> = if bytes_per_sample == 2 {
        raw.as_chunks::<2>().0.iter().map(|pair| pair[0]).collect()
    } else {
        raw.to_vec()
    };
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel in rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(PreviewFrame {
        width,
        height,
        rgba,
    })
}

fn ppm_token<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8]> {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        if bytes.get(*cursor) != Some(&b'#') {
            break;
        }
        while bytes.get(*cursor).is_some_and(|byte| *byte != b'\n') {
            *cursor += 1;
        }
    }
    let start = *cursor;
    while bytes
        .get(*cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
    if start == *cursor {
        Err(Error::InvalidPreview("PPM header is incomplete".into()))
    } else {
        Ok(&bytes[start..*cursor])
    }
}

fn parse_ppm_number(token: &[u8], name: &str) -> Result<u32> {
    std::str::from_utf8(token)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::InvalidPreview(format!("invalid PPM {name}")))
}

/// The dimensions the video actually displays at: anamorphic SAR is baked
/// into the width, and 90°/270° rotation metadata swaps the axes. FFmpeg
/// autorotates and the encode chain normalizes SAR, so every filter and the
/// preview then work in these display dimensions with square pixels.
fn display_dimensions(stream: &ProbeStream) -> (Option<u32>, Option<u32>) {
    let (Some(width), Some(height)) = (stream.width, stream.height) else {
        return (stream.width, stream.height);
    };
    let sar = stream.sample_aspect_ratio.as_deref().and_then(parse_ratio);
    let display_width = sar.map_or(width, |sar| (f64::from(width) * sar).round() as u32 & !1);
    let rotation = stream_rotation(stream).rem_euclid(360.0);
    if rotation == 90.0 || rotation == 270.0 {
        (Some(height), Some(display_width.max(2)))
    } else {
        (Some(display_width.max(2)), Some(height))
    }
}

/// Accepts ffprobe's ratio forms: `375:512`, `0`, or a bare decimal.
fn parse_ratio(text: &str) -> Option<f64> {
    let value = if let Some((left, right)) = text.split_once(':') {
        let left: f64 = left.trim().parse().ok()?;
        let right: f64 = right.trim().parse().ok()?;
        left / right
    } else {
        text.trim().parse().ok()?
    };
    (value.is_finite() && value > 0.0).then_some(value)
}

fn stream_rotation(stream: &ProbeStream) -> f64 {
    if let Some(side_data) = &stream.side_data_list
        && let Some(rotation) = side_data
            .iter()
            .find(|data| data.side_data_type.as_deref() == Some("Display Matrix"))
            .and_then(|data| data.rotation)
    {
        return rotation;
    }
    tag(&stream.tags, "rotate")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.0)
}

fn tag(tags: &Option<HashMap<String, String>>, key: &str) -> Option<String> {
    tags.as_ref()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

/// Reads the last `crop=w:h:x:y` that FFmpeg's cropdetect filter logged.
fn parse_cropdetect(log: &str, width: u32, height: u32) -> Option<CropMargins> {
    let values = log.lines().rev().find_map(|line| {
        let crop = line.split("crop=").nth(1)?.split_whitespace().next()?;
        let values: Vec<i64> = crop
            .split(':')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        (values.len() == 4).then_some(values)
    })?;
    let (crop_width, crop_height, x, y) = (values[0], values[1], values[2], values[3]);
    let (width, height) = (i64::from(width), i64::from(height));
    if crop_width <= 0
        || crop_height <= 0
        || x < 0
        || y < 0
        || x + crop_width > width
        || y + crop_height > height
    {
        return None;
    }
    let even = |value: i64| u32::try_from(value).unwrap_or(0) & !1;
    Some(CropMargins {
        top: even(y),
        bottom: even(height - crop_height - y),
        left: even(x),
        right: even(width - crop_width - x),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EncodeSettings, Preset};

    #[test]
    fn parses_probe_json() {
        let info = parse_probe(Path::new("movie.mkv"), br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001"},{"index":1,"codec_type":"audio","codec_name":"aac","channels":2,"tags":{"language":"eng"}}],"format":{"format_name":"matroska","duration":"12.5","size":"1000","bit_rate":"640"}}"#).expect("valid fixture");
        assert_eq!(info.duration_secs, 12.5);
        assert_eq!(info.video_stream().and_then(|s| s.width), Some(1920));
        assert_eq!(
            info.video_stream().and_then(|s| s.frame_rate),
            Some((30_000, 1_001))
        );
        assert_eq!(
            info.audio_streams()
                .next()
                .and_then(|s| s.language.as_deref()),
            Some("eng")
        );
    }

    #[test]
    fn probe_frame_rate_falls_back_from_average_to_real() {
        let info = parse_probe(Path::new("clip.mp4"), br#"{"streams":[{"index":0,"codec_type":"video","avg_frame_rate":"0/0","r_frame_rate":"25/1"}],"format":null}"#).expect("valid fixture");
        assert_eq!(
            info.video_stream().and_then(|s| s.frame_rate),
            Some((25, 1))
        );
        // Zero rates are not real frame rates and stay absent.
        let broken = parse_probe(Path::new("clip.mp4"), br#"{"streams":[{"index":0,"codec_type":"video","avg_frame_rate":"0/0","r_frame_rate":"0/30"}],"format":null}"#).expect("valid fixture");
        assert_eq!(broken.video_stream().and_then(|s| s.frame_rate), None);
    }

    #[test]
    fn command_is_generated_only_in_adapter() {
        let request = EncodeRequest {
            input: "in file.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 10.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::ALL,
            audio_tracks: vec![crate::AudioTrackSettings {
                source_index: 2,
                encoder: AudioEncoder::Aac,
                bitrate_kbps: 192,
            }],
            overwrite: false,
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
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:2"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a:0", "aac"]));
    }

    #[test]
    fn hardware_encoder_uses_its_native_quality_controls() {
        let settings = EncodeSettings {
            video_encoder: VideoEncoder::NvencH265,
            quality: 21,
            ..EncodeSettings::default()
        };
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 10.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::ALL,
            audio_tracks: Vec::new(),
            overwrite: false,
            settings,
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "hevc_nvenc"]));
        assert!(args.windows(2).any(|pair| pair == ["-cq", "21"]));
        assert!(!args.iter().any(|arg| arg == "-crf"));
    }

    #[test]
    fn frame_rate_target_becomes_an_output_rate_argument() {
        let settings = EncodeSettings {
            frame_rate: Some(24),
            ..EncodeSettings::default()
        };
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 10.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::ALL,
            audio_tracks: Vec::new(),
            overwrite: false,
            settings,
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(args.windows(2).any(|pair| pair == ["-r", "24"]));

        // The default keeps the source's rate and emits nothing.
        let mut request = request;
        request.settings.frame_rate = None;
        assert!(
            !FfmpegEngine::encode_args(&request)
                .iter()
                .any(|arg| arg == "-r")
        );
    }

    #[test]
    fn trim_range_becomes_seek_and_duration_arguments() {
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 60.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::new(12.5, 42.75),
            audio_tracks: Vec::new(),
            overwrite: false,
            settings: EncodeSettings::default(),
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(args.windows(2).any(|pair| pair == ["-ss", "12.500"]));
        assert!(args.windows(2).any(|pair| pair == ["-t", "30.250"]));
        assert_eq!(request.output_duration_secs(), 30.25);
    }

    #[test]
    fn preview_is_a_single_scaled_ppm_frame() {
        let args = FfmpegEngine::preview_args(
            Path::new("source movie.mkv"),
            12.3456,
            640,
            16.0 / 9.0,
            true,
        );
        assert!(args.windows(2).any(|pair| pair == ["-ss", "12.346"]));
        assert!(args.iter().any(|arg| arg == "-noaccurate_seek"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-vf", "scale=640:360,format=rgb24"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-vcodec", "ppm"]));
    }

    #[test]
    fn parses_sixteen_bit_preview_frame() {
        let frame = parse_ppm(b"P6\n2 1\n65535\n\x12\x34\xAB\xCD\x00\xFF\x56\x78\x9A\xBC\x0F\x0F")
            .expect("valid 16-bit PPM");
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.rgba, [0x12, 0xAB, 0x00, 255, 0x56, 0x9A, 0x0F, 255]);
    }

    #[test]
    fn parses_uncompressed_preview_frame() {
        let frame = parse_ppm(b"P6\n# generated\n2 1\n255\n\x01\x02\x03\x04\x05\x06")
            .expect("valid PPM frame");
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn crop_runs_before_aspect_preserving_scale() {
        let settings = EncodeSettings {
            crop: crate::CropMargins {
                top: 10,
                bottom: 20,
                left: 30,
                right: 40,
            },
            max_width: Some(1280),
            max_height: Some(720),
            ..EncodeSettings::default()
        };
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 60.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::ALL,
            audio_tracks: Vec::new(),
            overwrite: false,
            settings,
        };
        let args = FfmpegEngine::encode_args(&request);
        assert!(args.windows(2).any(|pair| {
            pair == [
                "-vf",
                "scale='trunc(if(gt(sar,0),iw*sar,iw)/2)*2':'ih',setsar=1,crop=iw-70:ih-30:30:10,scale='min(1280,iw)':'min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
            ]
        }));
    }

    #[test]
    fn temporary_output_requires_explicit_overwrite() {
        let destination = test_output_path("no-overwrite");
        std::fs::write(&destination, b"original").expect("create destination fixture");
        let temporary = TemporaryOutput::new(&destination).expect("create temporary output");
        std::fs::write(temporary.path(), b"replacement").expect("write temporary fixture");
        let result = temporary.commit(&destination, false);
        assert!(matches!(result, Err(Error::OutputExists(_))));
        assert_eq!(
            std::fs::read(&destination).expect("read original destination"),
            b"original"
        );
        let _ = std::fs::remove_file(destination);
    }

    #[test]
    fn successful_overwrite_is_committed_after_completion() {
        let destination = test_output_path("overwrite");
        std::fs::write(&destination, b"original").expect("create destination fixture");
        let temporary = TemporaryOutput::new(&destination).expect("create temporary output");
        std::fs::write(temporary.path(), b"replacement").expect("write temporary fixture");
        assert_eq!(
            std::fs::read(&destination).expect("read destination before commit"),
            b"original"
        );
        temporary
            .commit(&destination, true)
            .expect("commit replacement");
        assert_eq!(
            std::fs::read(&destination).expect("read committed destination"),
            b"replacement"
        );
        let _ = std::fs::remove_file(destination);
    }

    fn test_output_path(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "carbon-disc-{label}-{}-{nanos}.mp4",
            std::process::id()
        ))
    }

    #[test]
    fn size_samples_cover_short_ranges_in_full() {
        assert_eq!(
            sample_segments(crate::EncodeRange::new(2.0, 5.0), 5.0),
            vec![(2.0, 3.0)]
        );
        assert_eq!(
            sample_segments(crate::EncodeRange::ALL, 3.0),
            vec![(0.0, 3.0)]
        );
    }

    #[test]
    fn size_samples_spread_across_long_ranges_and_stay_inside() {
        let segments = sample_segments(crate::EncodeRange::new(100.0, 400.0), 400.0);
        assert_eq!(segments.len(), MAX_SAMPLES);
        for (start, length) in &segments {
            assert!(*start >= 100.0 && start + length <= 400.0);
        }
        assert_eq!(segments.first(), Some(&(100.0, SAMPLE_SECS)));
        assert_eq!(segments.last(), Some(&(397.0, SAMPLE_SECS)));
        // A range that only fits a few whole samples gets that many.
        let medium = sample_segments(crate::EncodeRange::new(0.0, 100.0), 100.0);
        assert_eq!(medium.len(), 3);
        assert_eq!(medium.first(), Some(&(0.0, SAMPLE_SECS)));
        assert_eq!(medium.last(), Some(&(97.0, SAMPLE_SECS)));
    }

    #[test]
    fn size_sample_request_measures_passthrough_audio_but_not_reencoded() {
        let request = EncodeRequest {
            input: "in.mkv".into(),
            output: "out.mp4".into(),
            source_duration_secs: 120.0,
            source_width: Some(1920),
            source_height: Some(1080),
            range: crate::EncodeRange::ALL,
            audio_tracks: vec![
                crate::AudioTrackSettings {
                    source_index: 1,
                    encoder: AudioEncoder::Aac,
                    bitrate_kbps: 192,
                },
                crate::AudioTrackSettings {
                    source_index: 2,
                    encoder: AudioEncoder::Copy,
                    bitrate_kbps: 0,
                },
            ],
            overwrite: false,
            settings: EncodeSettings::default(),
        };
        let sample = sample_request(&request, 12.0, 8.0);
        assert_eq!(sample.audio_tracks.len(), 1);
        assert_eq!(sample.audio_tracks[0].source_index, 2);
        let args = FfmpegEngine::encode_args(&sample);
        assert!(args.windows(2).any(|pair| pair == ["-ss", "12.000"]));
        assert!(args.windows(2).any(|pair| pair == ["-t", "8.000"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a:0", "copy"]));
        assert!(!args.iter().any(|arg| arg == "-c:a:1"));
    }

    #[test]
    fn bundled_tools_are_found_beside_the_executable() {
        let dir = std::env::temp_dir().join(format!("carbon-disc-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create bundle fixture dir");
        let ffmpeg = if cfg!(windows) {
            dir.join("ffmpeg.exe")
        } else {
            dir.join("ffmpeg")
        };
        std::fs::write(&ffmpeg, b"").expect("create ffmpeg fixture");
        assert_eq!(tool_beside(&dir, "ffmpeg"), Some(ffmpeg.clone()));
        assert_eq!(tool_beside(&dir, "ffprobe"), None);
        let _ = std::fs::remove_file(&ffmpeg);
        let _ = std::fs::remove_dir(&dir);
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

    #[test]
    fn probe_reports_display_dimensions_for_anamorphic_video() {
        let info = parse_probe(
            Path::new("anamorphic.mp4"),
            br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"sample_aspect_ratio":"375:512"}],"format":{"format_name":"mov,mp4"}}"#,
        )
        .expect("valid fixture");
        let video = info.video_stream().expect("video stream");
        assert_eq!((video.width, video.height), (Some(1406), Some(1080)));
    }

    #[test]
    fn probe_swaps_dimensions_for_rotated_video() {
        let info = parse_probe(
            Path::new("portrait.mp4"),
            br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"side_data_list":[{"side_data_type":"Display Matrix","rotation":-90.0}]}],"format":{"format_name":"mov,mp4"}}"#,
        )
        .expect("valid fixture");
        let video = info.video_stream().expect("video stream");
        assert_eq!((video.width, video.height), (Some(1080), Some(1920)));
    }

    #[test]
    fn cropdetect_output_becomes_margins() {
        let log = "[Parsed_cropdetect_0 @ 0x1] x1:0 x2:1919 y1:138 y2:941 w:1920 h:800 x:0 y:140 pts:1 t:0.04 crop=1920:800:0:140\n\
                   [Parsed_cropdetect_0 @ 0x1] x1:0 x2:1919 y1:138 y2:941 w:1920 h:800 x:0 y:140 pts:2 t:0.08 crop=1920:800:0:140\n";
        assert_eq!(
            parse_cropdetect(log, 1920, 1080),
            Some(CropMargins {
                top: 140,
                bottom: 140,
                left: 0,
                right: 0
            })
        );
        let black = "[Parsed_cropdetect_0 @ 0x1] crop=-1904:-1072:1912:1080\n";
        assert_eq!(parse_cropdetect(black, 1920, 1080), None);
    }
}
