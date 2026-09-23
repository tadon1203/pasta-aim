use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    entries: Vec<ModelEntry>,
}

impl ModelCatalog {
    pub fn scan() -> Result<Self> {
        let dir = paths::models_dir();
        tracing::debug!(path = %dir.display(), "scanning models directory");
        fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

        let mut entries = Vec::new();
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() || !is_onnx(&path) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            entries.push(ModelEntry {
                name: name.to_owned(),
                path,
            });
        }
        entries.sort_by_key(|entry| entry.name.to_ascii_lowercase());
        tracing::debug!(path = %dir.display(), count = entries.len(), "model scan completed");
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn path_for(&self, name: &str) -> Option<PathBuf> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.path.clone())
    }

    pub fn sole_model(&self) -> Option<&ModelEntry> {
        (self.entries.len() == 1).then(|| &self.entries[0])
    }
}

pub fn resolve_model(name: &str) -> Result<PathBuf> {
    validate_model_name(name)?;
    let path = paths::models_dir().join(name);
    if !path.is_file() {
        bail!("model is not present in models/: {name}");
    }
    tracing::debug!(model = name, path = %path.display(), "resolved model path");
    Ok(path)
}

pub fn open_models_folder() -> Result<()> {
    let dir = paths::models_dir();
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Command::new("explorer")
        .arg(&dir)
        .spawn()
        .with_context(|| format!("failed to open {}", dir.display()))?;
    tracing::debug!(path = %dir.display(), "opened models folder");
    Ok(())
}

pub fn validate_model_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("model filename is empty");
    }
    let path = PathBuf::from(name);
    if path.components().count() != 1 || !is_onnx(&path) {
        bail!("model must be a .onnx filename inside models/");
    }
    Ok(())
}

fn is_onnx(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_must_be_local_onnx_files() {
        assert!(validate_model_name("yolo26n.onnx").is_ok());
        assert!(validate_model_name("nested/yolo.onnx").is_err());
        assert!(validate_model_name("model.txt").is_err());
    }
}
