use std::path::PathBuf;

use carbon_disc::{MediaEngine, engine::ffmpeg::FfmpegEngine};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Open this media file when the GUI starts.
    path: Option<PathBuf>,
    /// Probe a media file as JSON and exit without opening the GUI.
    #[arg(long, value_name = "FILE")]
    probe: Option<PathBuf>,
    /// Print encoders that FFmpeg can initialize on this machine and exit.
    #[arg(long)]
    capabilities: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if let Some(path) = args.probe {
        let info = FfmpegEngine::default().probe(&path)?;
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    if args.capabilities {
        for encoder in FfmpegEngine::default().capabilities()?.video_encoders {
            let kind = if encoder.is_hardware() {
                "hardware"
            } else {
                "software"
            };
            println!("{}\t{kind}", encoder.display_name());
        }
        return Ok(());
    }
    const ICON_SIZE: u32 = 128;
    let icon = eframe::egui::IconData {
        rgba: carbon_disc::gui::logo_rgba(ICON_SIZE),
        width: ICON_SIZE,
        height: ICON_SIZE,
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Carbon Disc")
            .with_inner_size([1360.0, 900.0])
            .with_min_inner_size([960.0, 600.0])
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "Carbon Disc",
        options,
        Box::new(move |context| Ok(Box::new(carbon_disc::gui::App::new(context, args.path)))),
    )?;
    Ok(())
}
