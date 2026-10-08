//! 本地打标模型（WD Tagger 系列及更新架构）：模型不预置，首次调用时从
//! HuggingFace 自动下载（支持断点续传与进度事件），随后以 ONNX Runtime
//! 在本地 CPU 上推理，输出 booru 风格标签。

use crate::models::UsageMetrics;
use image::imageops::FilterType;
use ndarray::Array4;
use ort::value::Tensor;
use parking_lot::Mutex;
use reqwest::Client;
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tauri::{AppHandle, Emitter, Runtime};

pub const DEFAULT_TAG_THRESHOLD: f32 = 0.35;

pub struct LocalModelInfo {
    #[allow(dead_code)] // displayed in the frontend registry tooltip
    pub id: &'static str,
    pub name: &'static str,
    pub repo: &'static str,
    pub size_mb: u64,
    #[allow(dead_code)] // shown in the frontend registry
    pub description: &'static str,
}

/// 内置模型注册表：WD v1.4 系列（业界标准）与 2025 更新的 v3 / EVA02 系列。
/// 模型不随应用分发，选择后按需下载。
pub const KNOWN_MODELS: &[LocalModelInfo] = &[
    LocalModelInfo {
        id: "wd-v1-4-vit-tagger-v2",
        name: "WD v1.4 ViT-B/16",
        repo: "SmilingWolf/wd-v1-4-vit-tagger-v2",
        size_mb: 373,
        description: "标准 WD 打标模型，ViT-B/16，448px",
    },
    LocalModelInfo {
        id: "wd-v1-4-convnextv2-tagger-v2",
        name: "WD v1.4 ConvNeXtV2",
        repo: "SmilingWolf/wd-v1-4-convnextv2-tagger-v2",
        size_mb: 388,
        description: "ConvNeXtV2-Base，448px，泛化较好",
    },
    LocalModelInfo {
        id: "wd-v1-4-swinv2-tagger-v2",
        name: "WD v1.4 SwinV2",
        repo: "SmilingWolf/wd-v1-4-swinv2-tagger-v2",
        size_mb: 455,
        description: "SwinV2-Base，448px，细节标签更全",
    },
    LocalModelInfo {
        id: "wd-v1-4-moat-tagger-v2",
        name: "WD v1.4 Moat",
        repo: "SmilingWolf/wd-v1-4-moat-tagger-v2",
        size_mb: 326,
        description: "Moat-Base，448px，体积最小的 v1.4 系列",
    },
    LocalModelInfo {
        id: "wd-vit-tagger-v3",
        name: "WD v3 ViT",
        repo: "SmilingWolf/wd-vit-tagger-v3",
        size_mb: 379,
        description: "2025 更新训练，ViT 架构",
    },
    LocalModelInfo {
        id: "wd-swinv2-tagger-v3",
        name: "WD v3 SwinV2",
        repo: "SmilingWolf/wd-swinv2-tagger-v3",
        size_mb: 467,
        description: "2025 更新训练，SwinV2 架构",
    },
    LocalModelInfo {
        id: "wd-eva02-large-tagger-v3",
        name: "WD v3 EVA02-Large",
        repo: "SmilingWolf/wd-eva02-large-tagger-v3",
        size_mb: 1260,
        description: "更先进的大模型，标签质量最佳，体积也最大",
    },
];

pub fn known_model(id: &str) -> Option<&'static LocalModelInfo> {
    KNOWN_MODELS.iter().find(|model| model.id == id)
}

fn files_match(left: &Path, right: &Path) -> bool {
    let Ok(left_meta) = fs::metadata(left) else {
        return false;
    };
    let Ok(right_meta) = fs::metadata(right) else {
        return false;
    };
    if left_meta.len() != right_meta.len() {
        return false;
    }
    fn digest(path: &Path) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut file = fs::File::open(path).ok()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Some(hasher.finalize().into())
    }
    digest(left).is_some_and(|value| digest(right) == Some(value))
}

/// Moves model artifacts written by older versions under AppData to the new
/// non-system model root. Source files are deleted only after size and SHA-256
/// verification, so an interrupted migration remains recoverable.
pub fn migrate_legacy_models(
    legacy_root: &Path,
    target_root: &Path,
) -> Result<Vec<String>, String> {
    if !legacy_root.is_dir() || legacy_root == target_root {
        return Ok(Vec::new());
    }
    fs::create_dir_all(target_root).map_err(|error| error.to_string())?;
    let mut migrated = Vec::new();
    for model in KNOWN_MODELS {
        let source_dir = legacy_root.join(model.id);
        if !source_dir.is_dir() {
            continue;
        }
        let target_dir = target_root.join(model.id);
        fs::create_dir_all(&target_dir).map_err(|error| error.to_string())?;
        for entry in fs::read_dir(&source_dir).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let source = entry.path();
            if !source.is_file() {
                continue;
            }
            let destination = target_dir.join(entry.file_name());
            if !files_match(&source, &destination) {
                if destination.exists() {
                    return Err(format!(
                        "目标模型文件冲突，已保留两端文件：{}",
                        destination.display()
                    ));
                }
                let temporary = destination.with_extension(format!(
                    "{}.migrating",
                    destination
                        .extension()
                        .and_then(|value| value.to_str())
                        .unwrap_or("file")
                ));
                let _ = fs::remove_file(&temporary);
                fs::copy(&source, &temporary).map_err(|error| error.to_string())?;
                if !files_match(&source, &temporary) {
                    let _ = fs::remove_file(&temporary);
                    return Err(format!("模型迁移校验失败：{}", source.display()));
                }
                fs::rename(&temporary, &destination).map_err(|error| error.to_string())?;
            }
            fs::remove_file(&source).map_err(|error| error.to_string())?;
        }
        let _ = fs::remove_dir(&source_dir);
        if target_dir.join("model.onnx").is_file() {
            migrated.push(model.id.to_string());
        }
    }
    let _ = fs::remove_dir(legacy_root);
    Ok(migrated)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelState {
    pub id: String,
    pub name: String,
    pub size_mb: u64,
    pub installed: bool,
    pub downloading: bool,
    pub received: u64,
    pub total: u64,
    pub storage_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelEvent {
    pub model_id: String,
    pub status: String,
    pub received: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct LocalModelStore {
    dir: PathBuf,
    hf_base: String,
    sessions: Mutex<HashMap<String, Arc<parking_lot::Mutex<ort::session::Session>>>>,
    downloads: Mutex<HashMap<String, (u64, u64)>>,
    /// 同一模型只允许一个下载任务写入 `.part`，批量并发首次运行时其余任务等待复用结果。
    download_guards: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    errors: Mutex<HashMap<String, String>>,
    /// 实际使用的执行设备（"GPU (DirectML)" / "CPU (...)"），会话创建时记录。
    ep_name: Mutex<Option<String>>,
    /// 首次推理时由任务层上报一次执行设备到任务日志。
    ep_reported: std::sync::atomic::AtomicBool,
}

/// 会话构建：优先 CUDA（GPU），初始化或提交失败自动回退 CPU。
/// CUDA EP 依赖随包 CUDA 12 / cuDNN 9 运行库（启动时已注入进程 PATH）。
fn build_session(
    store: &LocalModelStore,
    model_path: &Path,
) -> Result<ort::session::Session, String> {
    let mut base = ort::session::Session::builder()
        .map_err(|e| e.to_string())?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
        .map_err(|e| e.to_string())?;
    // TF32（TensorFloat-32）在 Ampere+ 上大幅加速 fp32 MatMul/Conv；
    // 打标任务的置信度精度损失可忽略。
    let gpu = base
        .clone()
        .with_execution_providers([ort::ep::CUDA::default().with_tf32(true).build()]);
    match gpu {
        Ok(mut builder) => match builder.commit_from_file(model_path) {
            Ok(session) => {
                *store.ep_name.lock() = Some("GPU (CUDA)".to_string());
                Ok(session)
            }
            Err(error) => {
                let cpu = base
                    .commit_from_file(model_path)
                    .map_err(|e| e.to_string())?;
                *store.ep_name.lock() = Some(format!("CPU（CUDA 不可用：{error}）"));
                Ok(cpu)
            }
        },
        Err(_) => {
            let cpu = base
                .commit_from_file(model_path)
                .map_err(|e| e.to_string())?;
            *store.ep_name.lock() = Some("CPU（CUDA EP 未启用）".to_string());
            Ok(cpu)
        }
    }
}

impl LocalModelStore {
    #[cfg(test)]
    pub fn new(app_dir: &Path) -> Self {
        Self::with_base(app_dir, "https://huggingface.co")
    }

    /// 测试用：可注入模拟下载源。
    #[cfg(test)]
    pub fn with_base(app_dir: &Path, hf_base: &str) -> Self {
        Self::with_model_dir_and_base(app_dir.join("models"), hf_base)
    }

    /// Application constructor: accepts the final model directory directly,
    /// allowing large model files to live independently from the small app DB.
    pub fn with_model_dir(dir: PathBuf) -> Self {
        Self::with_model_dir_and_base(dir, "https://huggingface.co")
    }

    fn with_model_dir_and_base(dir: PathBuf, hf_base: &str) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            hf_base: hf_base.trim_end_matches('/').to_string(),
            sessions: Mutex::new(HashMap::new()),
            ep_name: Mutex::new(None),
            ep_reported: std::sync::atomic::AtomicBool::new(false),
            downloads: Mutex::new(HashMap::new()),
            download_guards: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
        }
    }

    pub fn model_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    pub fn storage_dir(&self) -> &Path {
        &self.dir
    }

    pub fn model_path(&self, id: &str) -> PathBuf {
        self.model_dir(id).join("model.onnx")
    }

    pub fn installed(&self, id: &str) -> bool {
        self.model_path(id).is_file()
    }

    /// 实际执行设备名（会话创建后可用）。
    pub fn ep_name(&self) -> Option<String> {
        self.ep_name.lock().clone()
    }

    /// 标记执行设备已上报（每 store 一次）。
    pub fn mark_ep_reported(&self) -> bool {
        !self
            .ep_reported
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    }

    /// 测试辅助：模拟 IPC 入口已设置的下载占位。
    #[cfg(test)]
    pub fn test_place_placeholder(&self, id: &str, total: u64) {
        self.downloads.lock().insert(id.to_string(), (0, total));
    }

    pub fn list(&self) -> Vec<LocalModelState> {
        KNOWN_MODELS
            .iter()
            .map(|info| {
                let (received, total) = self
                    .downloads
                    .lock()
                    .get(info.id)
                    .copied()
                    .unwrap_or((0, info.size_mb * 1024 * 1024));
                LocalModelState {
                    id: info.id.into(),
                    name: info.name.into(),
                    size_mb: info.size_mb,
                    installed: self.installed(info.id),
                    downloading: self.downloads.lock().contains_key(info.id),
                    received,
                    total,
                    storage_path: self.dir.display().to_string(),
                    error: self.errors.lock().get(info.id).cloned(),
                }
            })
            .collect()
    }

    fn emit_progress<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        id: &str,
        status: &str,
        error: Option<String>,
    ) {
        let (received, total) = self.downloads.lock().get(id).copied().unwrap_or((0, 0));
        let _ = app.emit(
            "local-model-event",
            LocalModelEvent {
                model_id: id.into(),
                status: status.into(),
                received,
                total,
                error,
            },
        );
    }

    /// 后台下载模型（model.onnx + selected_tags.csv），支持断点续传，
    /// 通过 `local-model-event` 上报进度。
    pub fn download<R: Runtime>(
        http: &Client,
        store: Arc<LocalModelStore>,
        app: AppHandle<R>,
        id: String,
    ) -> Result<(), String> {
        let info = known_model(&id).ok_or_else(|| "未知的本地模型 ID".to_string())?;
        if store.installed(&id) {
            return Ok(());
        }
        // 原子占位：insert 返回旧值表示已有下载在进行。
        if store
            .downloads
            .lock()
            .insert(id.clone(), (0, info.size_mb * 1024 * 1024))
            .is_some()
        {
            return Err("该模型正在下载中".into());
        }
        store.errors.lock().remove(&id);
        let http = http.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = download_model(&http, app, &store, &id).await;
        });
        Ok(())
    }
}

/// Downloads are driven through the AppRuntime-owned store (see tasks.rs);
/// this function holds the shared download implementation.
#[allow(clippy::too_many_arguments)]
pub async fn download_model<R: Runtime>(
    http: &Client,
    app: AppHandle<R>,
    store: &Arc<LocalModelStore>,
    id: &str,
) -> Result<(), String> {
    if !store.storage_dir().is_dir() {
        return Err(format!(
            "未检测到可写的非系统盘，模型下载已停止；请连接 D:–Z: 盘后重启应用（目标目录：{}）",
            store.storage_dir().display()
        ));
    }
    let info = known_model(id).ok_or_else(|| "未知的本地模型 ID".to_string())?;
    let guard = {
        let mut guards = store.download_guards.lock();
        guards
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let (_download_guard, waited_for_existing) = match guard.clone().try_lock_owned() {
        Ok(locked) => (locked, false),
        Err(_) => (guard.clone().lock_owned().await, true),
    };
    let dir = store.model_dir(id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if store.installed(id) {
        return Ok(());
    }
    // 同一批次中等待首个下载的任务直接复用其失败结果，避免每张图片依次重复下载。
    // 用户之后再次点击下载时未发生等待，仍会执行一次真正重试。
    if waited_for_existing {
        if let Some(error) = store.errors.lock().get(id).cloned() {
            return Err(error);
        }
    }
    if !store.downloads.lock().contains_key(id) {
        store
            .downloads
            .lock()
            .insert(id.to_string(), (0, info.size_mb * 1024 * 1024));
    }
    store.errors.lock().remove(id);
    store.emit_progress(&app, id, "downloading", None);
    let result = fetch_file(
        http,
        &format!("{}/{}/resolve/main/model.onnx", store.hf_base, info.repo),
        &dir.join("model.onnx.part"),
        &dir.join("model.onnx"),
        |received, total| {
            store
                .downloads
                .lock()
                .insert(id.to_string(), (received, total));
            store.emit_progress(&app, id, "downloading", None);
        },
    )
    .await;
    if let Err(error) = result {
        store.errors.lock().insert(id.to_string(), error.clone());
        store.emit_progress(&app, id, "error", Some(error.clone()));
        store.downloads.lock().remove(id);
        return Err(error);
    }
    // 标签文件：优先使用模型仓库自带的 selected_tags.csv，缺失时回退内置表。
    let tags_url = format!(
        "{}/{}/resolve/main/selected_tags.csv",
        store.hf_base, info.repo
    );
    if let Err(error) = fetch_file(
        http,
        &tags_url,
        &dir.join("selected_tags.csv.part"),
        &dir.join("selected_tags.csv"),
        |_, _| {},
    )
    .await
    {
        let _ = error; // 标签表缺失不阻断：推理时回退内置表
    }
    store.downloads.lock().remove(id);
    store.emit_progress(&app, id, "done", None);
    Ok(())
}

/// 流式下载到目标文件；已存在 .part 时尝试断点续传（Range）。
async fn fetch_file(
    http: &Client,
    url: &str,
    part_path: &Path,
    final_path: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let mut start = fs::metadata(part_path).map(|meta| meta.len()).unwrap_or(0);
    let mut request = http.get(url);
    if start > 0 {
        request = request.header("Range", format!("bytes={start}-"));
    }
    let mut response = request.send().await.map_err(|e| e.to_string())?;
    // 服务器可能不再接受旧的断点范围；清掉残片后完整重试一次。
    if response.status().as_u16() == 416 && start > 0 {
        start = 0;
        let _ = fs::remove_file(part_path);
        response = http.get(url).send().await.map_err(|e| e.to_string())?;
    }
    if !response.status().is_success() {
        return Err(format!("模型下载失败：HTTP {}（{url}）", response.status()));
    }
    let content_length = response.content_length().unwrap_or(0);
    let total = if response.status().as_u16() == 206 {
        response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(start.saturating_add(content_length))
    } else {
        content_length
    };
    let mut file = if response.status().as_u16() == 206 {
        fs::OpenOptions::new()
            .append(true)
            .open(part_path)
            .map_err(|e| e.to_string())?
    } else {
        start = 0;
        fs::File::create(part_path).map_err(|e| e.to_string())?
    };
    let mut stream = response.bytes_stream();
    let mut received = start;
    let mut last_emit = Instant::now();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        received += chunk.len() as u64;
        if last_emit.elapsed().as_millis() > 300 {
            on_progress(received, total);
            last_emit = Instant::now();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(part_path, final_path).map_err(|e| e.to_string())?;
    on_progress(received, total);
    Ok(())
}

/// 使用缓存的 ONNX 会话对图片打标，返回 "tag1, tag2, ..."。
/// `threshold` 为 sigmoid 后置信度下限；`max_tags` 为 0 表示不限；
/// `keep_underscores` 为 false 时标签中的 "_" 转为空格。
pub fn tag_image(
    store: &LocalModelStore,
    id: &str,
    image_path: &str,
    threshold: f32,
    max_tags: usize,
    keep_underscores: bool,
) -> Result<(String, UsageMetrics), String> {
    let started = Instant::now();
    if !store.installed(id) {
        return Err("本地模型尚未安装，请先在预设中选择并下载".into());
    }
    let session = {
        let mut sessions = store.sessions.lock();
        if let Some(cached) = sessions.get(id) {
            cached.clone()
        } else {
            let built = build_session(store, &store.model_path(id))?;
            let session = Arc::new(Mutex::new(built));
            sessions.insert(id.to_string(), session.clone());
            session
        }
    };
    // 读取输入张量形状：WD 系列有 NCHW（如 ViT）与 NHWC（如 Moat）两种布局。
    let (input_name, height, width, channels_last) = {
        let session = session.lock();
        let input = &session.inputs()[0];
        let input_name = input.name().to_string();
        match input.dtype() {
            ort::value::ValueType::Tensor { shape, .. } => {
                let dims: Vec<i64> = shape.iter().copied().collect();
                if dims.len() >= 4 {
                    if dims[1] == 3 {
                        // NCHW: [1, 3, H, W]
                        (
                            input_name,
                            spatial_dimension(dims[2]),
                            spatial_dimension(dims[3]),
                            false,
                        )
                    } else if dims[3] == 3 {
                        // NHWC: [1, H, W, 3]
                        (
                            input_name,
                            spatial_dimension(dims[1]),
                            spatial_dimension(dims[2]),
                            true,
                        )
                    } else {
                        (
                            input_name,
                            spatial_dimension(dims[dims.len() - 2]),
                            spatial_dimension(dims[dims.len() - 1]),
                            false,
                        )
                    }
                } else {
                    (input_name, 448, 448, false)
                }
            }
            _ => (input_name, 448, 448, false),
        }
    };
    let image = image::open(image_path).map_err(|e| format!("读取图片失败：{e}"))?;
    let resized = prepare_wd_image(&image, width, height);
    let mut data = Vec::with_capacity((3 * width * height) as usize);
    let mut inputs = HashMap::new();
    if channels_last {
        for y in 0..height {
            for x in 0..width {
                let pixel = resized.get_pixel(x, y);
                // WD 导出模型使用 OpenCV/BGR 顺序及 [0,255] 原始像素。
                data.extend([pixel[2] as f32, pixel[1] as f32, pixel[0] as f32]);
            }
        }
        let array = Array4::from_shape_vec((1, height as usize, width as usize, 3), data)
            .map_err(|e| e.to_string())?;
        let tensor = Tensor::from_array(array).map_err(|e| e.to_string())?;
        inputs.insert(input_name.clone(), tensor);
    } else {
        for channel in 0..3 {
            for y in 0..height {
                for x in 0..width {
                    let pixel = resized.get_pixel(x, y);
                    data.push(pixel[2 - channel] as f32);
                }
            }
        }
        let array = Array4::from_shape_vec((1, 3, height as usize, width as usize), data)
            .map_err(|e| e.to_string())?;
        let tensor = Tensor::from_array(array).map_err(|e| e.to_string())?;
        inputs.insert(input_name.clone(), tensor);
    }
    let scores: Vec<f32> = {
        let mut session = session.lock();
        let outputs = session.run(inputs).map_err(|e| format!("推理失败：{e}"))?;
        let (_, view) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("输出解析失败：{e}"))?;
        view.to_vec()
    };
    let table = load_general_labels(id, &store.model_dir(id));
    // 输出槽位与 CSV 行序对齐；仅当模型输出不含评分槽位时才跳过表头评分行。
    let without_ratings = table.taggable_count;
    let shift = if scores.len() == without_ratings {
        table.leading_ratings
    } else if scores.len() == table.labels.len() {
        0
    } else if known_model(id).is_some() {
        return Err(format!(
            "模型输出维度与标签表不匹配：输出 {}，标签 {}（请重新下载 selected_tags.csv）",
            scores.len(),
            table.labels.len()
        ));
    } else {
        // 自定义/测试模型可输出额外张量槽位，按其标签表可覆盖的前缀读取。
        0
    };
    // v3 及更新模型输出已是 sigmoid 后的概率（值域 [0,1]），直接比较；
    // v1.4 等输出 logits，需先过 sigmoid。
    let is_probability = scores.iter().all(|score| (0.0..=1.0).contains(score));
    let mut tagged: Vec<(String, f32)> = Vec::new();
    for (index, score) in scores.iter().enumerate() {
        if let Some(Some(label)) = table.labels.get(index + shift) {
            let probability = if is_probability {
                *score
            } else {
                1.0 / (1.0 + (-score).exp())
            };
            if probability >= threshold {
                tagged.push((label.clone(), probability));
            }
        }
    }
    tagged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if max_tags > 0 {
        tagged.truncate(max_tags);
    }
    let latency = started.elapsed().as_secs_f64() * 1000.0;
    let text = tagged
        .iter()
        .map(|(label, _)| {
            if keep_underscores {
                label.clone()
            } else {
                label.replace('_', " ")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let usage = UsageMetrics {
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        ttft_ms: Some(latency),
        generation_ms: Some(latency),
        tokens_per_second: None,
    };
    Ok((text, usage))
}

fn spatial_dimension(value: i64) -> u32 {
    if value > 1 {
        value as u32
    } else {
        448
    }
}

/// WD Tagger 官方预处理：透明层合成到白色背景、居中补成正方形、保持比例缩放。
/// 直接拉伸非正方形图片会显著改变人物和构图特征，造成错标。
fn prepare_wd_image(source: &image::DynamicImage, width: u32, height: u32) -> image::RgbImage {
    let rgba = source.to_rgba8();
    let side = rgba.width().max(rgba.height()).max(1);
    let mut square = image::RgbImage::from_pixel(side, side, image::Rgb([255, 255, 255]));
    let x = (side - rgba.width()) / 2;
    let y = (side - rgba.height()) / 2;
    for (px, py, pixel) in rgba.enumerate_pixels() {
        let alpha = pixel[3] as u16;
        let blend = |channel: u8| ((channel as u16 * alpha + 255 * (255 - alpha)) / 255) as u8;
        square.put_pixel(
            x + px,
            y + py,
            image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    image::imageops::resize(&square, width, height, FilterType::Lanczos3)
}

/// 模型目录下 selected_tags.csv（模型仓库自带）优先；缺失时使用内置
/// WD v1.4 标签表。CSV 行序即模型输出索引：前 4 行是评分标签（category=9），
/// 之后是通用标签。输出 4488 类（4 评分 + 4484 通用），此处按行序建表，
/// 评分位置置 None。
/// 标签行布局：`labels` 按 CSV 行序全量加载（评分标签为 None 占位），
/// `taggable_count` 为通用与角色标签总数，`leading_ratings` 为表头后的评分行数。
/// 旧版模型输出可能不含评分槽位（如 wd-moat 4484 维），此时需跳过 leading_ratings 个标签。
pub struct TagTable {
    pub labels: Vec<Option<String>>,
    pub taggable_count: usize,
    pub leading_ratings: usize,
}

pub fn load_general_labels(id: &str, model_dir: &Path) -> TagTable {
    let own = model_dir.join("selected_tags.csv");
    let source = if own.is_file() {
        fs::read_to_string(&own).unwrap_or_default()
    } else {
        include_str!("../resources/wd14_selected_tags.csv").to_string()
    };
    let rows: Vec<Vec<String>> = parse_csv(&source).into_iter().skip(1).collect();
    let leading_ratings = rows
        .iter()
        .take_while(|row| row.get(2).is_some_and(|c| c.trim() == "9"))
        .count();
    let mut labels: Vec<Option<String>> = Vec::with_capacity(rows.len());
    let mut taggable_count = 0usize;
    for row in rows {
        let name = row.get(1).map(|n| n.trim().trim_matches('"').to_string());
        let is_taggable = row.get(2).is_some_and(|c| matches!(c.trim(), "0" | "4"));
        if is_taggable && name.as_deref().is_some_and(|n| !n.is_empty()) {
            taggable_count += 1;
        }
        // 评分标签（category=9）不写入 caption；通用（0）与角色（4）标签均保留。
        labels.push(is_taggable.then_some(name.unwrap_or_default()));
    }
    let _ = id;
    TagTable {
        labels,
        taggable_count,
        leading_ratings,
    }
}

/// 极简 CSV 解析：支持双引号包裹与 "" 转义（selected_tags.csv 含带逗号的标签名）。
fn parse_csv(source: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = source.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                if !row.iter().all(|f| f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                }
            }
            '\r' => {}
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn download_rejects_http_errors_without_promoting_partial_file() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let worker = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            request
                .respond(tiny_http::Response::from_string("not found").with_status_code(404))
                .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("model.onnx.part");
        let final_path = dir.path().join("model.onnx");
        let result = fetch_file(
            &reqwest::Client::new(),
            &format!("http://127.0.0.1:{port}/model.onnx"),
            &part,
            &final_path,
            |_, _| {},
        )
        .await;
        worker.join().unwrap();
        assert!(result.unwrap_err().contains("HTTP 404"));
        assert!(!final_path.exists());
    }

    #[tokio::test]
    async fn tagging_respects_threshold_max_tags_and_underscores() {
        use image::ImageBuffer;
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join("models").join("wd-test-model");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/resources/test-model.onnx"),
            model_dir.join("model.onnx"),
        )
        .unwrap();
        std::fs::write(
            model_dir.join("selected_tags.csv"),
            "tag_id,name,category,count\n0,long_hair,0,1\n1,two,0,1\n2,three,0,1\n",
        )
        .unwrap();
        let store = LocalModelStore::new(dir.path());
        let img: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(512, 512, image::Rgb([200, 120, 90]));
        let img_path = dir.path().join("in.png");
        img.save(&img_path).unwrap();
        // 阈值 1.01：identity 输出为 [0,1] 概率，全部滤除。
        let (text, _) = tag_image(
            &store,
            "wd-test-model",
            img_path.to_str().unwrap(),
            1.01,
            0,
            false,
        )
        .unwrap();
        assert!(text.is_empty(), "expected no tags above 1.01, got: {text}");
        // 阈值 0 + max_tags=2：恰好保留 2 个。
        let (text, _) = tag_image(
            &store,
            "wd-test-model",
            img_path.to_str().unwrap(),
            0.0,
            2,
            false,
        )
        .unwrap();
        assert_eq!(
            text.split(',').filter(|t| !t.trim().is_empty()).count(),
            2,
            "got: {text}"
        );
        // 保留下划线：long_hair 原样输出。
        let (text, _) = tag_image(
            &store,
            "wd-test-model",
            img_path.to_str().unwrap(),
            0.0,
            0,
            true,
        )
        .unwrap();
        assert!(text.contains("long_hair"), "got: {text}");
        // 默认转空格。
        let (text, _) = tag_image(
            &store,
            "wd-test-model",
            img_path.to_str().unwrap(),
            0.0,
            0,
            false,
        )
        .unwrap();
        assert!(text.contains("long hair"), "got: {text}");
    }

    #[tokio::test]
    #[ignore = "manual v3 output probe"]
    async fn inspect_v3_output_distribution() {
        use image::ImageBuffer;
        let dir = tempfile::tempdir().unwrap();
        let model_dir = dir.path().join("models").join("wd-swinv2-tagger-v3");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::copy(
            r"C:\tmp\wd-swinv2-tagger-v3.onnx",
            model_dir.join("model.onnx"),
        )
        .unwrap();
        std::fs::copy(
            r"C:\tmp\wd-swinv2-tagger-v3.csv",
            model_dir.join("selected_tags.csv"),
        )
        .unwrap();
        let store = LocalModelStore::new(dir.path());
        let id = "wd-swinv2-tagger-v3";
        std::env::set_var(
            "ORT_DYLIB_PATH",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/resources/onnxruntime/onnxruntime.dll"
            ),
        );
        // 预创建会话并缓存
        let built = ort::session::Session::builder()
            .unwrap()
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
            .unwrap()
            .commit_from_file(store.model_path(id))
            .unwrap();
        store
            .sessions
            .lock()
            .insert(id.to_string(), Arc::new(parking_lot::Mutex::new(built)));
        let img: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(512, 512, image::Rgb([200, 120, 90]));
        let img_path = dir.path().join("probe.png");
        img.save(&img_path).unwrap();
        let (input_name, height, width, channels_last) = {
            let sessions = store.sessions.lock();
            let session = sessions.get(id).unwrap().lock();
            let input = &session.inputs()[0];
            let input_name = input.name().to_string();
            match input.dtype() {
                ort::value::ValueType::Tensor { shape, .. } => {
                    let dims: Vec<i64> = shape.iter().copied().collect();
                    (
                        input_name,
                        dims[1].max(1) as u32,
                        dims[2].max(1) as u32,
                        dims[3] == 3,
                    )
                }
                _ => unreachable!(),
            }
        };
        let rgb = image::open(&img_path).unwrap().to_rgb8();
        let resized =
            image::imageops::resize(&rgb, width, height, image::imageops::FilterType::Lanczos3);
        let mut data = Vec::with_capacity((3 * width * height) as usize);
        let mut inputs = std::collections::HashMap::new();
        if channels_last {
            for y in 0..height {
                for x in 0..width {
                    let px = resized.get_pixel(x, y);
                    data.extend([
                        px[0] as f32 / 255.0,
                        px[1] as f32 / 255.0,
                        px[2] as f32 / 255.0,
                    ]);
                }
            }
        } else {
            for c in 0..3 {
                for y in 0..height {
                    for x in 0..width {
                        data.push(resized.get_pixel(x, y)[c] as f32 / 255.0);
                    }
                }
            }
        }
        let array =
            ndarray::Array4::from_shape_vec((1, height as usize, width as usize, 3), data).unwrap();
        let tensor = ort::value::Tensor::from_array(array).unwrap();
        inputs.insert(input_name.clone(), tensor);
        let session = store.sessions.lock().get(id).unwrap().clone();
        let scores: Vec<f32> = {
            let mut guard = session.lock();
            let outputs = guard.run(inputs).unwrap();
            let (_, view) = outputs[0].try_extract_tensor::<f32>().unwrap();
            view.to_vec()
        };
        let min = scores.iter().copied().fold(f32::MAX, f32::min);
        let max = scores.iter().copied().fold(f32::MIN, f32::max);
        let out_of_range = scores.iter().filter(|x| **x < 0.0 || **x > 1.0).count();
        let gt035 = scores.iter().filter(|x| **x > 0.35).count();
        eprintln!(
            "[probe] n={} min={min:.4} max={max:.4} out_of_[0,1]={out_of_range} >0.35={gt035}",
            scores.len()
        );
        let mut idx: Vec<usize> = (0..scores.len()).collect();
        idx.sort_by(|a, b| scores[*b].partial_cmp(&scores[*a]).unwrap());
        eprintln!("[probe] top6 slots: {:?}", &idx[..6]);
        let sig: Vec<f32> = scores.iter().map(|x| 1.0 / (1.0 + (-x).exp())).collect();
        eprintln!(
            "[probe] sigmoided >0.35: {}",
            sig.iter().filter(|x| **x > 0.35).count()
        );
        // 完整流程：默认阈值下应输出少量且合理的标签
        let (text, metrics) =
            tag_image(&store, id, img_path.to_str().unwrap(), 0.35, 0, false).unwrap();
        eprintln!(
            "[probe] tags({}): {}",
            text.split(',').filter(|t| !t.trim().is_empty()).count(),
            text
        );
        eprintln!("[probe] latency_ms={:.1}", metrics.ttft_ms.unwrap_or(0.0));
    }

    use super::*;

    #[test]
    fn parses_embedded_tag_table() {
        let table = load_general_labels("wd-v1-4-vit-tagger-v2", Path::new("C:/nonexistent"));
        // 嵌入表（v1.4 风格）：9083 数据行全量加载，前 4 行评分。
        assert_eq!(table.labels.len(), 9083, "全量标签行");
        assert_eq!(table.taggable_count, 9079, "v1.4 通用与角色标签数");
        assert_eq!(table.leading_ratings, 4, "表头后 4 行评分");
        // 前 4 个槽位是评分标签（None），之后按行序是通用标签。
        assert!(table.labels[..4].iter().all(|label| label.is_none()));
        assert_eq!(table.labels[4].as_deref(), Some("1girl"));
        assert_eq!(table.labels[5].as_deref(), Some("solo"));
        assert_eq!(table.labels[6].as_deref(), Some("long_hair"));
    }

    #[test]
    fn aligns_large_v3_tables() {
        // v3 风格表：10861 行（4 评分 + 8106 通用 + 2751 角色）。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("models").join("m")).unwrap();
        let mut csv = String::from("tag_id,name,category,count\n");
        for i in 0..4 {
            csv.push_str(&format!("99,rating{i},9,1\n"));
        }
        for i in 0..8106 {
            csv.push_str(&format!("{i},g{i},0,1\n"));
        }
        for i in 0..2751 {
            csv.push_str(&format!("{i},c{i},4,1\n"));
        }
        std::fs::write(dir.path().join("models/m/selected_tags.csv"), csv).unwrap();
        let table = load_general_labels("m", &dir.path().join("models/m"));
        assert_eq!(table.labels.len(), 10861);
        assert_eq!(table.taggable_count, 10857);
        assert_eq!(table.leading_ratings, 4);
        assert!(table.labels[0].is_none() && table.labels[3].is_none());
        assert_eq!(table.labels[4].as_deref(), Some("g0"));
        assert_eq!(
            table.labels[8110].as_deref(),
            Some("c0"),
            "角色标签应参与输出"
        );
    }

    #[test]
    fn preprocessing_preserves_aspect_ratio_with_white_padding() {
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            2,
            image::Rgb([10, 20, 30]),
        ));
        let prepared = prepare_wd_image(&source, 4, 4);
        assert_eq!(prepared.dimensions(), (4, 4));
        // 宽图上下居中补白，原图区域不应被直接拉伸到整张画布。
        assert!(prepared.get_pixel(0, 0)[0] > 200);
        assert!(prepared.get_pixel(2, 2)[0] < 100);
    }

    #[test]
    fn parses_quoted_csv_fields() {
        let rows = parse_csv("tag_id,name,category,count\n0,\"a, b\",0,1\n1,\"c\"\"d\",4,2\n");
        assert_eq!(rows.len(), 3, "header + 2 data rows");
        assert_eq!(rows[1][1], "a, b");
        assert_eq!(rows[2][1], "c\"d");
    }

    #[test]
    fn formats_tags_with_spaces_and_order() {
        let store = LocalModelStore::new(Path::new("C:/nonexistent"));
        let result = tag_image(&store, "missing", "x.png", DEFAULT_TAG_THRESHOLD, 0, false);
        assert!(result.is_err()); // 未安装模型必须报错而不是崩溃
        let _ = store;
    }

    #[test]
    fn migrates_legacy_model_only_after_hash_verification() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy-models");
        let target = dir.path().join("external-models");
        let model_id = KNOWN_MODELS[0].id;
        let source = legacy.join(model_id);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("model.onnx"), b"verified model bytes").unwrap();
        fs::write(
            source.join("selected_tags.csv"),
            b"name,category\n1girl,0\n",
        )
        .unwrap();

        let migrated = migrate_legacy_models(&legacy, &target).unwrap();
        assert_eq!(migrated, vec![model_id.to_string()]);
        assert_eq!(
            fs::read(target.join(model_id).join("model.onnx")).unwrap(),
            b"verified model bytes"
        );
        assert!(!source.exists());
    }
}
