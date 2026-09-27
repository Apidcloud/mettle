//! Defaults for file-level execution batches, read from the nearest project root.

use std::fs;
use std::num::NonZeroUsize;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(rename = "name")]
    _name: Option<String>,
    #[serde(rename = "version")]
    _version: Option<String>,
    #[serde(rename = "workingDir")]
    pub working_dir: Option<std::path::PathBuf>,
    pub run: BatchDefaults,
    pub test: BatchDefaults,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatchDefaults {
    jobs: Option<NonZeroUsize>,
}

impl BatchDefaults {
    pub fn jobs(&self) -> usize {
        self.jobs.map_or(1, NonZeroUsize::get)
    }
}

pub fn load(root: Option<&Path>) -> Result<ProjectConfig, String> {
    let Some(root) = root else {
        return Ok(ProjectConfig::default());
    };
    let manifest = root.join("mettle.toml");
    let source = fs::read_to_string(&manifest)
        .map_err(|error| format!("could not read {}: {error}", manifest.display()))?;
    toml::from_str(&source).map_err(|error| {
        format!(
            "invalid project configuration in {}:\n{error}",
            manifest.display()
        )
    })
}
