use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{CustomPreset, EncodeRequest, EncodeSettings, Error, Result, model::Preset};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct PresetDocument {
    version: u32,
    presets: Vec<CustomPreset>,
}

#[derive(Debug, Serialize, Deserialize)]
struct QueueDocument {
    version: u32,
    jobs: Vec<EncodeRequest>,
}

/// Choices the GUI restores on the next launch so a regular workflow needs no setup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub settings: EncodeSettings,
    pub preset: Preset,
    pub custom_preset: Option<String>,
    /// The selected HandBrake "more" preset, if any.
    #[serde(default)]
    pub more_preset: Option<String>,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionDocument {
    version: u32,
    session: Session,
}

pub fn load_session() -> Result<Option<Session>> {
    let path = data_path("session.json")?;
    if !path.exists() {
        return Ok(None);
    }
    let document: SessionDocument = read_json(&path)?;
    validate_version(document.version)?;
    Ok(Some(document.session))
}

pub fn save_session(session: &Session) -> Result<()> {
    write_json(
        &data_path("session.json")?,
        &SessionDocument {
            version: SCHEMA_VERSION,
            session: session.clone(),
        },
    )
}

pub fn load_presets() -> Result<Vec<CustomPreset>> {
    let path = data_path("presets.json")?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let document: PresetDocument = read_json(&path)?;
    validate_version(document.version)?;
    Ok(document.presets)
}

pub fn save_presets(presets: &[CustomPreset]) -> Result<()> {
    write_json(
        &data_path("presets.json")?,
        &PresetDocument {
            version: SCHEMA_VERSION,
            presets: presets.to_vec(),
        },
    )
}

pub fn import_presets(path: &Path) -> Result<Vec<CustomPreset>> {
    let document: PresetDocument = read_json(path)?;
    validate_version(document.version)?;
    Ok(document.presets)
}

pub fn export_presets(path: &Path, presets: &[CustomPreset]) -> Result<()> {
    write_json(
        path,
        &PresetDocument {
            version: SCHEMA_VERSION,
            presets: presets.to_vec(),
        },
    )
}

pub fn load_queue() -> Result<Vec<EncodeRequest>> {
    let path = data_path("queue.json")?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let document: QueueDocument = read_json(&path)?;
    validate_version(document.version)?;
    Ok(document.jobs)
}

pub fn save_queue(jobs: &[EncodeRequest]) -> Result<()> {
    write_json(
        &data_path("queue.json")?,
        &QueueDocument {
            version: SCHEMA_VERSION,
            jobs: jobs.to_vec(),
        },
    )
}

fn data_path(file_name: &str) -> Result<PathBuf> {
    Ok(project_dirs()?.data_local_dir().join(file_name))
}

/// Where the app looks for a downloaded FFmpeg, beside the data documents.
pub fn tools_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_local_dir().join("tools"))
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("org", "CarbonDisc", "Carbon Disc").ok_or_else(|| {
        Error::InvalidRequest("the operating system has no application data directory".into())
    })
}

fn validate_version(version: u32) -> Result<()> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(Error::InvalidRequest(format!(
            "unsupported data schema version {version}"
        )))
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_future_schema_versions() {
        assert!(validate_version(SCHEMA_VERSION + 1).is_err());
    }

    #[test]
    fn preset_document_round_trips() {
        let document = PresetDocument {
            version: SCHEMA_VERSION,
            presets: vec![CustomPreset {
                name: "My preset".into(),
                description: "Test".into(),
                settings: crate::EncodeSettings::default(),
            }],
        };
        let bytes = serde_json::to_vec(&document).expect("serialize preset document");
        let restored: PresetDocument =
            serde_json::from_slice(&bytes).expect("deserialize preset document");
        assert_eq!(restored.presets, document.presets);
    }
}
