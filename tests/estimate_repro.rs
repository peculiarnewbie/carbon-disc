//! Throwaway repro for the estimate failure; not committed.
use std::sync::Arc;

use carbon_disc::engine::MediaEngine;
use carbon_disc::engine::ffmpeg::FfmpegEngine;
use carbon_disc::model::{
    AudioEncoder, AudioTrackSettings, EncodeRange, EncodeRequest, EncodeSettings,
};

#[test]
fn estimate_movie() {
    let source = std::path::PathBuf::from(
        "/home/bolt/Videos/Movies/Marty Supreme (2025) [1080p] [WEBRip] [x265] [10bit] [5.1] [YTS.BZ]/Marty Supreme (2025) [1080p] [WEBRip] [x265] [10bit] [5.1] [YTS.BZ].mkv",
    );
    if !source.exists() {
        // Try to locate the actual video file in the folder.
        let dir = source.parent().unwrap();
        let found = std::fs::read_dir(dir).ok().and_then(|entries| {
            entries.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("mkv") || e.eq_ignore_ascii_case("mp4"))
                    .unwrap_or(false)
            })
        });
        let Some(source) = found else {
            eprintln!("SKIP: movie not found");
            return;
        };
        run(&source);
        return;
    }
    run(&source);
}

fn run(source: &std::path::Path) {
    let engine = FfmpegEngine::default();
    let media = engine.probe(source).expect("probe");
    eprintln!(
        "duration {:.0}s, video {}x{}",
        media.duration_secs,
        media.video_stream().and_then(|s| s.width).unwrap_or(0),
        media.video_stream().and_then(|s| s.height).unwrap_or(0)
    );
    let request = EncodeRequest {
        output: "/tmp/estimate-repro-out.mkv".into(),
        audio_tracks: vec![AudioTrackSettings {
            source_index: media.audio_streams().next().map(|t| t.index).unwrap_or(1),
            encoder: AudioEncoder::Aac,
            bitrate_kbps: 160,
        }],
        range: EncodeRange::ALL,
        settings: EncodeSettings {
            audio_encoder: AudioEncoder::Aac,
            audio_bitrate_kbps: 160,
            ..Default::default()
        },
        overwrite: true,
        input: source.to_path_buf(),
        source_duration_secs: media.duration_secs,
        source_width: media.video_stream().and_then(|s| s.width),
        source_height: media.video_stream().and_then(|s| s.height),
    };
    match engine.estimate_size(
        &request,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        &mut |_| {},
    ) {
        Ok(measurement) => eprintln!(
            "OK: {:.0} MB total",
            measurement.estimate_for(&request).total_bytes() as f64 / 1e6
        ),
        Err(error) => eprintln!("FAILED: {error}"),
    }
}
