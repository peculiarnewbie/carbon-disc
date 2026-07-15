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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if let Some(path) = args.probe {
        let info = FfmpegEngine::default().probe(&path)?;
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Carbon Disc")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([820.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Carbon Disc",
        options,
        Box::new(move |context| Ok(Box::new(carbon_disc::gui::App::new(context, args.path)))),
    )?;
    Ok(())
}
