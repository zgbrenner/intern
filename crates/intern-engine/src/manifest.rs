//! The pinned local model set.
//!
//! The manifest is compiled into the binary, so the list of files Intern will
//! ever download cannot be changed at runtime. Every file is accepted only if
//! its byte length and SHA-256 both match.

use std::{collections::HashSet, ffi::OsStr, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineErrorCode, EngineResult};

/// What a manifest file is for.
///
/// There is exactly one role. Intern reads documents as text, and the engine
/// cannot send an image to the model at all, so pinning a vision projector
/// would download hundreds of megabytes that nothing could ever load.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// The text model.
    Model,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub model_id: String,
    /// Value sent as the OpenAI-style `model` field.
    pub served_model_name: String,
    pub files: Vec<ModelFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub name: String,
    pub role: ModelRole,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub const fn embedded_manifest_json() -> &'static str {
    include_str!("../../../src-tauri/resources/model-manifest.json")
}

impl ModelManifest {
    pub fn embedded() -> EngineResult<Self> {
        Self::parse(embedded_manifest_json())
    }

    pub fn parse(json: &str) -> EngineResult<Self> {
        let manifest: Self = serde_json::from_str(json).map_err(|_| invalid())?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> EngineResult<()> {
        // Exactly one file. A manifest that could name more would be a manifest
        // that could make a user download something Intern never loads.
        if self.schema_version != 2
            || self.model_id.is_empty()
            || self.served_model_name.is_empty()
            || self.files.len() != 1
        {
            return Err(invalid());
        }
        let mut names = HashSet::new();
        for file in &self.files {
            if !is_safe_filename(&file.name)
                || !file.url.starts_with("https://")
                || file.size == 0
                || !names.insert(file.name.as_str())
                || file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(invalid());
            }
        }
        if self.model().is_none()
            || self
                .files
                .iter()
                .filter(|file| file.role == ModelRole::Model)
                .count()
                != 1
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn model(&self) -> Option<&ModelFile> {
        self.files.iter().find(|file| file.role == ModelRole::Model)
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }
}

fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.bytes().any(|byte| byte.is_ascii_control())
        && Path::new(name).file_name() == Some(OsStr::new(name))
        && Path::new(name).components().count() == 1
}

const fn invalid() -> EngineError {
    EngineError::new(
        EngineErrorCode::ManifestInvalid,
        "embedded model manifest failed validation",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_manifest_is_valid_and_names_one_text_model() {
        let manifest = ModelManifest::embedded().unwrap();
        assert_eq!(manifest.schema_version, 2);
        assert!(manifest.model().is_some());
        assert!(manifest.total_bytes() > 0);
        for file in &manifest.files {
            assert!(
                file.url.starts_with("https://huggingface.co/"),
                "{}",
                file.url
            );
        }
    }

    #[test]
    fn the_whole_first_run_download_fits_a_sixteen_gigabyte_laptop() {
        let manifest = ModelManifest::embedded().unwrap();
        assert_eq!(manifest.files.len(), 1);
        // Everything a user must download before Intern will name a document.
        // Nothing here may be a file the engine cannot load.
        assert!(
            manifest.total_bytes() < 2 * 1024 * 1024 * 1024,
            "first-run download must stay under 2 GiB, got {} bytes",
            manifest.total_bytes()
        );
    }

    #[test]
    fn a_manifest_naming_more_than_the_text_model_is_refused() {
        let base = ModelManifest::embedded().unwrap();
        let mut extra = base.clone();
        let mut second = extra.files[0].clone();
        second.name = "mmproj.gguf".into();
        extra.files.push(second);
        assert!(extra.validate().is_err());
    }

    #[test]
    fn path_traversal_and_bad_digests_are_refused() {
        let base = ModelManifest::embedded().unwrap();
        let mut traversal = base.clone();
        traversal.files[0].name = "../evil.gguf".into();
        assert!(traversal.validate().is_err());

        let mut digest = base.clone();
        digest.files[0].sha256 = "NOTHEX".into();
        assert!(digest.validate().is_err());

        let mut scheme = base;
        scheme.files[0].url = "http://huggingface.co/x".into();
        assert!(scheme.validate().is_err());
    }
}
