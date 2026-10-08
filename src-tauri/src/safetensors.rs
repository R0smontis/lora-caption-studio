//! SafeTensors 后处理：移除整个 `__metadata__`，保留张量描述和二进制数据。
//! 数据偏移在 SafeTensors 中相对于数据区起点，因此重写不同长度的 JSON 头不会
//! 改变任何张量。输出后再次读取并校验元数据为空、张量数据 SHA-256 完全一致。

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAX_HEADER_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoraMetadataReport {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub metadata_present: bool,
    pub metadata_count: usize,
    pub metadata_keys: Vec<String>,
    pub tensor_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoraSanitizeResult {
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    /// sanitized | already_clean | error
    pub status: String,
    pub removed_count: usize,
    pub tensor_data_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct ParsedHeader {
    value: Map<String, Value>,
    header_len: u64,
    file_len: u64,
}

fn parse_header(path: &Path) -> Result<ParsedHeader, String> {
    let file = File::open(path).map_err(|e| format!("读取模型失败：{e}"))?;
    let file_len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut reader = BufReader::new(file);
    let mut length = [0u8; 8];
    reader
        .read_exact(&mut length)
        .map_err(|_| "不是有效的 SafeTensors 文件：缺少头长度".to_string())?;
    let header_len = u64::from_le_bytes(length);
    if header_len == 0 || header_len > MAX_HEADER_BYTES || 8 + header_len > file_len {
        return Err(format!("SafeTensors 头长度异常：{header_len}"));
    }
    let mut header = vec![0u8; header_len as usize];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("读取 SafeTensors 头失败：{e}"))?;
    let value: Value =
        serde_json::from_slice(&header).map_err(|e| format!("SafeTensors JSON 头无效：{e}"))?;
    let value = value
        .as_object()
        .cloned()
        .ok_or_else(|| "SafeTensors 头必须是 JSON 对象".to_string())?;
    validate_tensor_entries(&value)?;
    Ok(ParsedHeader {
        value,
        header_len,
        file_len,
    })
}

/// 除 `__metadata__` 外，SafeTensors 顶层只能是必需的张量描述。
/// 这项验证防止非标准文件把训练参数伪装成其他顶层 JSON 字段后逃过清理。
fn validate_tensor_entries(value: &Map<String, Value>) -> Result<(), String> {
    for (name, descriptor) in value {
        if name == "__metadata__" {
            continue;
        }
        let descriptor = descriptor
            .as_object()
            .ok_or_else(|| format!("非标准 SafeTensors 顶层字段：{name}"))?;
        let valid = descriptor.get("dtype").is_some_and(Value::is_string)
            && descriptor.get("shape").is_some_and(Value::is_array)
            && descriptor
                .get("data_offsets")
                .and_then(Value::as_array)
                .is_some_and(|offsets| {
                    offsets.len() == 2 && offsets.iter().all(|offset| offset.as_u64().is_some())
                });
        if !valid {
            return Err(format!("非标准 SafeTensors 张量描述：{name}"));
        }
    }
    Ok(())
}

fn metadata_keys(value: &Map<String, Value>) -> Vec<String> {
    let mut keys = value
        .get("__metadata__")
        .and_then(Value::as_object)
        .map(|metadata| metadata.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    keys.sort_by_key(|key| key.to_lowercase());
    keys
}

pub fn inspect(path: &Path) -> LoraMetadataReport {
    let base = || LoraMetadataReport {
        path: path.to_string_lossy().to_string(),
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("未知文件")
            .to_string(),
        size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        metadata_present: false,
        metadata_count: 0,
        metadata_keys: Vec::new(),
        tensor_count: 0,
        error: None,
    };
    match parse_header(path) {
        Ok(parsed) => {
            let keys = metadata_keys(&parsed.value);
            LoraMetadataReport {
                metadata_present: parsed.value.contains_key("__metadata__"),
                metadata_count: keys.len(),
                metadata_keys: keys,
                tensor_count: parsed
                    .value
                    .keys()
                    .filter(|key| key.as_str() != "__metadata__")
                    .count(),
                ..base()
            }
        }
        Err(error) => LoraMetadataReport {
            error: Some(error),
            ..base()
        },
    }
}

pub fn select_files() -> Vec<LoraMetadataReport> {
    rfd::FileDialog::new()
        .add_filter("LoRA SafeTensors", &["safetensors"])
        .pick_files()
        .unwrap_or_default()
        .iter()
        .map(|path| inspect(path))
        .collect()
}

pub fn inspect_files(paths: &[String]) -> Vec<LoraMetadataReport> {
    paths.iter().map(|path| inspect(Path::new(path))).collect()
}

fn clean_output_path(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("model");
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let mut candidate = parent.join(format!("{stem}.metadata-removed.safetensors"));
    let mut suffix = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem}.metadata-removed-{suffix}.safetensors"));
        suffix += 1;
    }
    candidate
}

fn hash_range(path: &Path, offset: u64) -> Result<[u8; 32], String> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().into())
}

pub fn sanitize(path: &Path) -> LoraSanitizeResult {
    let source_path = path.to_string_lossy().to_string();
    let parsed = match parse_header(path) {
        Ok(parsed) => parsed,
        Err(error) => {
            return LoraSanitizeResult {
                source_path,
                output_path: None,
                status: "error".into(),
                removed_count: 0,
                tensor_data_verified: false,
                error: Some(error),
            }
        }
    };
    let removed_count = metadata_keys(&parsed.value).len();
    if !parsed.value.contains_key("__metadata__") {
        return LoraSanitizeResult {
            source_path,
            output_path: None,
            status: "already_clean".into(),
            removed_count: 0,
            tensor_data_verified: true,
            error: None,
        };
    }

    let output = clean_output_path(path);
    let temporary = output.with_extension("safetensors.tmp");
    let result = (|| -> Result<bool, String> {
        let mut clean_header = parsed.value.clone();
        clean_header.remove("__metadata__");
        let mut encoded = serde_json::to_vec(&Value::Object(clean_header))
            .map_err(|e| format!("编码净化头失败：{e}"))?;
        let padding = (8 - (encoded.len() % 8)) % 8;
        encoded.extend(std::iter::repeat_n(b' ', padding));

        let mut source = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
        source
            .seek(SeekFrom::Start(8 + parsed.header_len))
            .map_err(|e| e.to_string())?;
        let mut target = BufWriter::new(File::create(&temporary).map_err(|e| e.to_string())?);
        target
            .write_all(&(encoded.len() as u64).to_le_bytes())
            .and_then(|_| target.write_all(&encoded))
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut source, &mut target).map_err(|e| e.to_string())?;
        target.flush().map_err(|e| e.to_string())?;
        drop(target);

        let verified_header = parse_header(&temporary)?;
        if verified_header.value.contains_key("__metadata__") {
            return Err("二次扫描失败：输出文件仍包含 __metadata__".into());
        }
        let source_data_len = parsed.file_len - 8 - parsed.header_len;
        let output_data_len = verified_header.file_len - 8 - verified_header.header_len;
        if source_data_len != output_data_len {
            return Err("张量数据长度校验失败".into());
        }
        let source_hash = hash_range(path, 8 + parsed.header_len)?;
        let output_hash = hash_range(&temporary, 8 + verified_header.header_len)?;
        if source_hash != output_hash {
            return Err("张量数据 SHA-256 校验失败".into());
        }
        fs::rename(&temporary, &output).map_err(|e| e.to_string())?;
        Ok(true)
    })();

    match result {
        Ok(verified) => LoraSanitizeResult {
            source_path,
            output_path: Some(output.to_string_lossy().to_string()),
            status: "sanitized".into(),
            removed_count,
            tensor_data_verified: verified,
            error: None,
        },
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            LoraSanitizeResult {
                source_path,
                output_path: None,
                status: "error".into(),
                removed_count,
                tensor_data_verified: false,
                error: Some(error),
            }
        }
    }
}

pub fn sanitize_files(paths: &[String]) -> Vec<LoraSanitizeResult> {
    paths.iter().map(|path| sanitize(Path::new(path))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &Path) -> Vec<u8> {
        let tensors = serde_json::json!({
            "__metadata__": {
                "ss_learning_rate": "0.0001",
                "ss_steps": "1200",
                "modelspec.title": "private title",
                "custom_training_note": "secret"
            },
            "lora.test": { "dtype": "F32", "shape": [2], "data_offsets": [0, 8] }
        });
        let mut header = serde_json::to_vec(&tensors).unwrap();
        header.extend(std::iter::repeat_n(b' ', (8 - header.len() % 8) % 8));
        let payload = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut file = File::create(path).unwrap();
        file.write_all(&(header.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&header).unwrap();
        file.write_all(&payload).unwrap();
        payload
    }

    #[test]
    fn removes_all_metadata_and_keeps_tensor_bytes_identical() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("model.safetensors");
        let payload = fixture(&source);
        let before = inspect(&source);
        assert_eq!(before.metadata_count, 4);

        let result = sanitize(&source);
        assert_eq!(result.status, "sanitized");
        assert_eq!(result.removed_count, 4);
        assert!(result.tensor_data_verified);
        let output = PathBuf::from(result.output_path.unwrap());
        let after = inspect(&output);
        assert!(!after.metadata_present);
        assert_eq!(after.metadata_count, 0);
        let parsed = parse_header(&output).unwrap();
        let mut file = File::open(output).unwrap();
        file.seek(SeekFrom::Start(8 + parsed.header_len)).unwrap();
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, payload);
    }

    #[test]
    fn rejects_non_safetensors_input() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.safetensors");
        fs::write(&source, b"not a model").unwrap();
        assert!(inspect(&source).error.is_some());
        assert_eq!(sanitize(&source).status, "error");
    }

    #[test]
    fn rejects_hidden_non_tensor_top_level_fields() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("hidden.safetensors");
        let mut header = serde_json::to_vec(&serde_json::json!({
            "training_parameters": { "learning_rate": 0.001 }
        }))
        .unwrap();
        header.extend(std::iter::repeat_n(b' ', (8 - header.len() % 8) % 8));
        let mut file = File::create(&source).unwrap();
        file.write_all(&(header.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&header).unwrap();
        drop(file);
        assert!(inspect(&source)
            .error
            .unwrap()
            .contains("非标准 SafeTensors"));
    }
}
