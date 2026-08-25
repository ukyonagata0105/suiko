use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::EvaluationError;

pub(super) fn read(path: &Path) -> Result<Vec<u8>, EvaluationError> {
    fs::read(path).map_err(|source| EvaluationError::Read {
        path: path.display().to_string(),
        source,
    })
}

pub(super) fn utf8(path: &Path, bytes: Vec<u8>) -> Result<String, EvaluationError> {
    String::from_utf8(bytes).map_err(|_| EvaluationError::Utf8(path.display().to_string()))
}

pub(super) fn parse_toml<T: DeserializeOwned>(
    path: &Path,
    source: &str,
) -> Result<T, EvaluationError> {
    toml::from_str(source).map_err(|error| EvaluationError::Parse {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

pub(super) fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T, EvaluationError> {
    parse_toml(path, &utf8(path, read(path)?)?)
}

pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, EvaluationError> {
    serde_json::from_slice(&read(path)?).map_err(|error| EvaluationError::Parse {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

pub(super) fn digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut output, "{byte:02x}").expect("write to String");
    }
    output
}
