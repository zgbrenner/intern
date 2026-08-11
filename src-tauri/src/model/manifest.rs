use std::{collections::HashSet, ffi::OsStr, path::Path};

use serde::{Deserialize, Serialize};

use super::{ModelError, ModelErrorCode, ModelResult};

const MODEL_ID: &str = "qwen2.5-vl-3b-instruct-q4-k-m";
const MODEL_NAME: &str = "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf";
const MODEL_URL: &str = "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf";
const MODEL_SIZE: u64 = 1_929_901_056;
const MODEL_SHA256: &str = "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12";
const PROJECTOR_NAME: &str = "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf";
const PROJECTOR_URL: &str = "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf";
const PROJECTOR_SIZE: u64 = 1_338_428_128;
const PROJECTOR_SHA256: &str = "b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub model_id: String,
    pub files: Vec<ModelFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub const fn embedded_manifest_json() -> &'static str {
    include_str!("../../resources/model-manifest.json")
}

impl ModelManifest {
    pub fn embedded() -> ModelResult<Self> {
        Self::parse(embedded_manifest_json())
    }

    pub fn parse(json: &str) -> ModelResult<Self> {
        let manifest: Self = serde_json::from_str(json).map_err(|_| invalid_manifest())?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> ModelResult<()> {
        if self.schema_version != 1 || self.model_id != MODEL_ID || self.files.len() != 2 {
            return Err(invalid_manifest());
        }

        let mut names = HashSet::new();
        for file in &self.files {
            if !is_safe_filename(&file.name)
                || !file.url.starts_with("https://")
                || !names.insert(file.name.as_str())
                || file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(invalid_manifest());
            }
        }

        if !matches_file(
            &self.files[0],
            MODEL_NAME,
            MODEL_URL,
            MODEL_SIZE,
            MODEL_SHA256,
        ) || !matches_file(
            &self.files[1],
            PROJECTOR_NAME,
            PROJECTOR_URL,
            PROJECTOR_SIZE,
            PROJECTOR_SHA256,
        ) {
            return Err(invalid_manifest());
        }
        Ok(())
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

fn matches_file(file: &ModelFile, name: &str, url: &str, size: u64, sha256: &str) -> bool {
    file.name == name && file.url == url && file.size == size && file.sha256 == sha256
}

const fn invalid_manifest() -> ModelError {
    ModelError::new(
        ModelErrorCode::ManifestInvalid,
        "embedded model manifest failed validation",
    )
}
