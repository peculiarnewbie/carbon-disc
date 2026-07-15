# Carbon Disc

Carbon Disc is an open-source desktop video transcoder written in Rust and powered by
FFmpeg. It aims for HandBrake's approachable workflow while keeping the media backend
behind a small, testable Rust interface.

> Early development: probing, frame previews, start/end range cutting, audio-track
> selection, crop and resolution controls, safe output replacement, hardware encoder
> discovery, custom presets, recursive batch import, persistent queues, progress,
> cancellation, and FFmpeg transcoding are implemented. Subtitles and filters are
> planned next.

## Architecture

The dependency direction is deliberately one-way:

```text
gui ──commands──> queue/coordinator ──requests──> MediaEngine trait
                                                   │
                                                   └── FfmpegEngine adapter
model <──────────────── shared domain values ───────────────┘
```

- `model` owns backend-neutral media metadata and encode requests.
- `engine::MediaEngine` is the application/backend contract.
- `engine::ffmpeg` is the only module that knows FFmpeg/FFprobe arguments and output.
- `queue` owns background work and publishes immutable UI snapshots through events.
- `persistence` owns versioned JSON documents for custom presets and unfinished jobs.
- `gui` follows edirstat's native egui layout and dark slate/violet visual language.

This process boundary is intentional. It avoids unsafe libav FFI, isolates GPL tooling,
makes cancellation reliable, and allows a future libav or remote worker adapter without
rewriting the app.

## Requirements

- Rust 1.85 or newer
- `ffmpeg` and `ffprobe` on `PATH`

Override the tools when developing against a custom build:

```bash
export CARBON_DISC_FFMPEG=/path/to/ffmpeg
export CARBON_DISC_FFPROBE=/path/to/ffprobe
cargo run
```

The local `/home/bolt/git/other/HandBrake` checkout is HandBrake source, not an FFmpeg
checkout. It is useful as a product/behavior reference, but is not linked into Carbon Disc.

## Run

```bash
cargo run --release
cargo run -- --probe /path/to/video.mkv
cargo run -- --capabilities
cargo test
```

The capabilities command performs a one-frame initialization check before reporting a
hardware encoder, avoiding devices that are compiled into FFmpeg but unusable on the
current machine. The GUI performs the same discovery in the background and limits its
encoder menu accordingly.

Custom presets and unfinished queue entries are saved as versioned JSON in the platform's
local application-data directory. Restored jobs never assume overwrite permission: a
missing source or existing destination is shown as a failed job that needs attention.
Batch files and folders use the active encode settings, select the first audio track, and
write a sibling `<source>-encoded.<container>` destination.

## License

GPL-2.0-or-later. This is compatible with distributing a GPL-enabled FFmpeg build; actual
distributions must also comply with the licenses of the codecs and FFmpeg configuration
they ship.
