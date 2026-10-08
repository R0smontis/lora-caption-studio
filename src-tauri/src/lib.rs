#[cfg(test)]
mod e2e;
mod file_ops;
mod local_model;
mod models;
mod providers;
mod safetensors;
mod storage;
mod tasks;

use crate::{
    file_ops::{
        image_preview as make_image_preview, load_thumbnails as make_thumbnails,
        preview_caption_edit_batch, select_text_files as pick_text_files,
        select_text_folder as pick_text_folder,
    },
    models::{
        ApiConnection, BatchRequest, BootstrapData, CaptionEditBatch, CaptionEditRule,
        CaptionVersion, ConnectionTestResult, EditBatchRecord, HistoricalStats, ImageJob,
        ModelOption, StatsBreakdown, StatsQuery, TaskRuntimeState, UndoBatchResult,
    },
    storage::{Storage, CAPTION_SOURCE_MANUAL, CAPTION_SOURCE_UNDO},
    tasks::AppRuntime,
};
use chrono::Utc;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

#[tauri::command]
async fn bootstrap(runtime: State<'_, Arc<AppRuntime>>) -> Result<BootstrapData, String> {
    let storage = runtime.storage.clone();
    tauri::async_runtime::spawn_blocking(move || storage.bootstrap())
        .await
        .map_err(|error| format!("初始化线程异常：{error}"))?
}

#[cfg(windows)]
fn drive_name(path: &Path) -> Option<String> {
    let value = path.to_string_lossy();
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        Some(format!("{}:", (bytes[0] as char).to_ascii_uppercase()))
    } else {
        None
    }
}

#[cfg(windows)]
fn is_non_system_path(path: &Path) -> bool {
    let system = std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".into())
        .trim_end_matches(['\\', '/'])
        .to_ascii_uppercase();
    drive_name(path).is_some_and(|drive| drive != system)
}

#[cfg(windows)]
fn is_local_writable_drive(path: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    const DRIVE_REMOVABLE_KIND: u32 = 2;
    const DRIVE_FIXED_KIND: u32 = 3;
    let Some(drive) = drive_name(path) else {
        return false;
    };
    let root = format!("{drive}\\");
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
    kind == DRIVE_FIXED_KIND || kind == DRIVE_REMOVABLE_KIND
}

#[cfg(windows)]
fn prepare_model_directory(path: &Path) -> bool {
    if !is_non_system_path(path)
        || !is_local_writable_drive(path)
        || fs::create_dir_all(path).is_err()
    {
        return false;
    }
    let probe = path.join(format!(".write-test-{}", std::process::id()));
    let writable = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut file| file.write_all(b"ok"))
        .is_ok();
    let _ = fs::remove_file(probe);
    writable
}

#[cfg(windows)]
fn choose_model_directory(document_dir: Option<PathBuf>) -> PathBuf {
    if let Ok(configured) = std::env::var("LORA_CAPTION_MODEL_DIR") {
        let configured = PathBuf::from(configured);
        if prepare_model_directory(&configured) {
            return configured;
        }
    }
    if let Some(document_dir) = document_dir {
        let candidate = document_dir.join("LoRA Caption Studio").join("models");
        if prepare_model_directory(&candidate) {
            return candidate;
        }
    }
    let drive_mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    for letter in b'D'..=b'Z' {
        let bit = 1u32 << (letter - b'A');
        if drive_mask & bit == 0 {
            continue;
        }
        let candidate = PathBuf::from(format!("{}:\\LoRA Caption Studio\\models", letter as char));
        if prepare_model_directory(&candidate) {
            return candidate;
        }
    }
    // Keep the application usable for API-based tagging. The model downloader
    // will report that a writable non-system volume is required.
    PathBuf::from("D:\\LoRA Caption Studio\\models")
}

#[cfg(not(windows))]
fn choose_model_directory(document_dir: Option<PathBuf>) -> PathBuf {
    document_dir
        .unwrap_or_else(std::env::temp_dir)
        .join("LoRA Caption Studio")
        .join("models")
}

#[tauri::command]
fn save_connection(
    runtime: State<'_, Arc<AppRuntime>>,
    connection: ApiConnection,
) -> Result<ApiConnection, String> {
    runtime.storage.save_connection(connection)
}

#[tauri::command]
fn delete_connection(runtime: State<'_, Arc<AppRuntime>>, id: String) -> Result<(), String> {
    runtime.storage.delete_connection(&id)
}

#[tauri::command]
async fn list_models(
    runtime: State<'_, Arc<AppRuntime>>,
    connection_id: String,
    force: bool,
) -> Result<Vec<ModelOption>, String> {
    let connection = runtime.storage.get_connection(&connection_id)?;
    if !force {
        if let (Some(models), Some(cached_at)) = (
            connection.cached_models.clone(),
            connection.models_cached_at,
        ) {
            if Utc::now().timestamp_millis() - cached_at < 600_000 {
                return Ok(models);
            }
        }
    }
    let first = connection
        .key_refs
        .first()
        .and_then(|key| key.id.as_deref())
        .ok_or("连接中没有 API Key")?;
    let secret = runtime.storage.key_secret(first).ok_or("API Key 不存在")?;
    let (models, effective_base) = providers::list_models(&runtime.http, &connection, &secret)
        .await
        .map_err(|e| e.message)?;
    if effective_base != connection.base_url {
        // Auto-complete: persist the working Base URL layout.
        let mut updated = connection.clone();
        updated.base_url = effective_base;
        if let Ok(saved) = runtime.storage.save_connection(updated) {
            runtime.storage.write_log(
                "info",
                "system",
                &format!(
                    "已自动补全 Base URL：{} -> {}",
                    connection.base_url, saved.base_url
                ),
            );
        }
    }
    runtime
        .storage
        .update_models(&connection_id, models.clone())?;
    Ok(models)
}

/// Connection test: fetches the model list with a bounded timeout and reports
/// latency and a model sample. The result never throws.
#[tauri::command]
async fn test_connection(
    runtime: State<'_, Arc<AppRuntime>>,
    connection_id: String,
) -> Result<ConnectionTestResult, String> {
    let connection = runtime.storage.get_connection_full(&connection_id)?;
    let first = connection
        .key_refs
        .first()
        .and_then(|key| key.id.as_deref())
        .ok_or("连接中没有 API Key")?;
    let secret = runtime.storage.key_secret(first).ok_or("API Key 不存在")?;
    let result = providers::test_connection(&runtime.http, &connection, &secret).await?;
    if let Some(effective) = result.effective_base_url.clone() {
        if effective != connection.base_url {
            let mut updated = connection.clone();
            updated.base_url = effective;
            if runtime.storage.save_connection(updated).is_ok() {
                runtime.storage.write_log(
                    "info",
                    "system",
                    &format!(
                        "连接测试：已自动补全 Base URL {} -> {}",
                        connection.base_url,
                        result.effective_base_url.as_deref().unwrap_or("")
                    ),
                );
            }
        }
    }
    Ok(result)
}

#[tauri::command]
async fn select_images(
    runtime: State<'_, Arc<AppRuntime>>,
    multiple: bool,
) -> Result<Vec<ImageJob>, String> {
    let reviewed: file_ops::ReviewedSet = runtime
        .storage
        .list_reviewed_caption_paths()
        .map(|paths| paths.into_iter().map(|p| p.to_lowercase()).collect())
        .unwrap_or_default();
    let revision_counts: file_ops::RevisionCounts = runtime
        .storage
        .count_revised_per_caption()
        .map(|counts| {
            counts
                .into_iter()
                .map(|(p, n)| (p.to_lowercase(), n))
                .collect()
        })
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        file_ops::select_images(multiple, &reviewed, &revision_counts)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn select_folder(
    runtime: State<'_, Arc<AppRuntime>>,
    recursive: bool,
) -> Result<Vec<ImageJob>, String> {
    let reviewed: file_ops::ReviewedSet = runtime
        .storage
        .list_reviewed_caption_paths()
        .map(|paths| paths.into_iter().map(|p| p.to_lowercase()).collect())
        .unwrap_or_default();
    let revision_counts: file_ops::RevisionCounts = runtime
        .storage
        .count_revised_per_caption()
        .map(|counts| {
            counts
                .into_iter()
                .map(|(p, n)| (p.to_lowercase(), n))
                .collect()
        })
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        file_ops::select_folder(recursive, &reviewed, &revision_counts)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn select_text_files() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(pick_text_files)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn select_text_folder(recursive: bool) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || pick_text_folder(recursive))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn select_lora_files() -> Result<Vec<safetensors::LoraMetadataReport>, String> {
    tauri::async_runtime::spawn_blocking(safetensors::select_files)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn inspect_lora_metadata(
    paths: Vec<String>,
) -> Result<Vec<safetensors::LoraMetadataReport>, String> {
    tauri::async_runtime::spawn_blocking(move || safetensors::inspect_files(&paths))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn sanitize_lora_metadata(
    paths: Vec<String>,
) -> Result<Vec<safetensors::LoraSanitizeResult>, String> {
    tauri::async_runtime::spawn_blocking(move || safetensors::sanitize_files(&paths))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn load_thumbnails(paths: Vec<String>) -> Result<HashMap<String, String>, String> {
    tauri::async_runtime::spawn_blocking(move || make_thumbnails(&paths))
        .await
        .map_err(|e| e.to_string())
}

/// Full-size preview fitted into a `max_dimension` box.
#[tauri::command]
async fn get_image_preview(path: String, max_dimension: u32) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || make_image_preview(&path, max_dimension))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn save_preset(
    runtime: State<'_, Arc<AppRuntime>>,
    kind: String,
    preset: Value,
) -> Result<(), String> {
    if kind != "tag" && kind != "revision" {
        return Err("未知预设类型".into());
    }
    runtime.storage.save_preset(&kind, preset)
}

#[tauri::command]
fn delete_preset(
    runtime: State<'_, Arc<AppRuntime>>,
    kind: String,
    id: String,
) -> Result<(), String> {
    if kind != "tag" && kind != "revision" {
        return Err("未知预设类型".into());
    }
    runtime.storage.delete_preset(&kind, &id)
}

#[tauri::command]
fn start_batch(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
    request: BatchRequest,
) -> Result<String, String> {
    tasks::start_batch(app, runtime.inner().clone(), request)
}

#[tauri::command]
fn cancel_batch(runtime: State<'_, Arc<AppRuntime>>, batch_id: String) {
    tasks::cancel_batch(&runtime, &batch_id);
}

#[tauri::command]
fn list_local_models(runtime: State<'_, Arc<AppRuntime>>) -> Vec<local_model::LocalModelState> {
    runtime.models.list()
}

/// 选择文件夹并返回路径（供抽选流程使用）。
#[tauri::command]
fn select_folder_path(_recursive: bool) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    dialog
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

/// 扫描文件夹内全部图片路径（供抽选预览使用，不移动任何文件）。
#[tauri::command]
fn scan_folder_paths(folder: String, recursive: bool) -> Result<Vec<String>, String> {
    Ok(file_ops::scan_images(Path::new(&folder), recursive)?
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// 扫描文件夹并返回完整任务条目，供 AI 抽样保留已有标签状态。
#[tauri::command]
async fn scan_folder_jobs(
    runtime: State<'_, Arc<AppRuntime>>,
    folder: String,
    recursive: bool,
) -> Result<Vec<ImageJob>, String> {
    let reviewed: file_ops::ReviewedSet = runtime
        .storage
        .list_reviewed_caption_paths()
        .map(|paths| paths.into_iter().map(|path| path.to_lowercase()).collect())
        .unwrap_or_default();
    let revision_counts: file_ops::RevisionCounts = runtime
        .storage
        .count_revised_per_caption()
        .map(|counts| {
            counts
                .into_iter()
                .map(|(path, count)| (path.to_lowercase(), count))
                .collect()
        })
        .unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        file_ops::scan_images(Path::new(&folder), recursive)?
            .into_iter()
            .map(|path| file_ops::image_job(&path, &reviewed, &revision_counts))
            .collect()
    })
    .await
    .map_err(|error| error.to_string())?
}

/// 随机抽选：从文件夹抽 count 张返回（含任务元数据），其余不移动（待预览确认）。
#[tauri::command]
async fn sample_images(
    runtime: State<'_, Arc<AppRuntime>>,
    folder: String,
    count: usize,
    recursive: bool,
) -> Result<Vec<ImageJob>, String> {
    let reviewed: file_ops::ReviewedSet = runtime
        .storage
        .list_reviewed_caption_paths()
        .map(|paths| paths.into_iter().map(|p| p.to_lowercase()).collect())
        .unwrap_or_default();
    let revision_counts: file_ops::RevisionCounts = runtime
        .storage
        .count_revised_per_caption()
        .map(|counts| {
            counts
                .into_iter()
                .map(|(p, n)| (p.to_lowercase(), n))
                .collect()
        })
        .unwrap_or_default();
    let (kept, _) = file_ops::sample_paths(Path::new(&folder), recursive, count)?;
    Ok(kept
        .into_iter()
        .map(|path| {
            file_ops::image_job(&path, &reviewed, &revision_counts).unwrap_or_else(|error| {
                ImageJob {
                    id: Uuid::new_v4().to_string(),
                    image_path: path.to_string_lossy().into_owned(),
                    caption_path: path.with_extension("txt").to_string_lossy().into_owned(),
                    file_name: path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("image")
                        .into(),
                    thumbnail: None,
                    caption: String::new(),
                    reviewed: false,
                    revision_count: 0,
                    status: "pending".into(),
                    selected: false,
                    error: Some(error),
                    error_kind: None,
                    refusal: None,
                    metrics: None,
                    key_label: None,
                    updated_at: None,
                    sample_score: None,
                }
            })
        })
        .collect())
}

/// 应用抽样结果：把未保留的图片移入 `<folder>/.refusal/`，返回移动/跳过清单。
#[tauri::command]
fn apply_sample(
    folder: String,
    kept_paths: Vec<String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    file_ops::apply_sample_refusal(Path::new(&folder), &kept_paths)
}

/// AI 抽样明确判断后立即移动图片与同名标签到 .ai-selected / .ai-rejected。
#[tauri::command]
fn move_ai_sample_image(
    folder: String,
    path: String,
    keep: bool,
) -> Result<(String, String), String> {
    file_ops::move_ai_sample_image(Path::new(&folder), Path::new(&path), keep)
}

#[tauri::command]
fn return_ai_sample_image(folder: String, path: String) -> Result<(String, String), String> {
    file_ops::return_ai_sample_image(Path::new(&folder), Path::new(&path))
}

/// 重置 AI 抽样：恢复两个结果目录中的图片与同名标签到原相对位置。
#[tauri::command]
fn reset_ai_sample(folder: String) -> Result<(Vec<String>, Vec<String>), String> {
    file_ops::reset_ai_sample(Path::new(&folder))
}

/// 已复核完成的图片路径（用于复核去重）。
#[tauri::command]
fn list_reviewed_paths(runtime: State<'_, Arc<AppRuntime>>) -> Result<Vec<String>, String> {
    runtime.storage.list_reviewed_caption_paths()
}

/// 手动忽略某张图的复核错误（此后视为已复核）。
#[tauri::command]
fn ignore_review_error(
    runtime: State<'_, Arc<AppRuntime>>,
    caption_path: String,
) -> Result<(), String> {
    runtime.storage.ignore_review_error(&caption_path)
}

#[tauri::command]
fn download_local_model(
    app: AppHandle,
    runtime: State<'_, Arc<AppRuntime>>,
    model_id: String,
) -> Result<(), String> {
    local_model::LocalModelStore::download(&runtime.http, runtime.models.clone(), app, model_id)
}

#[tauri::command]
fn get_task_runtime(
    runtime: State<'_, Arc<AppRuntime>>,
    batch_id: String,
) -> Option<TaskRuntimeState> {
    runtime.get_task_runtime(&batch_id)
}

#[tauri::command]
fn save_caption(
    runtime: State<'_, Arc<AppRuntime>>,
    path: String,
    content: String,
    source: String,
) -> Result<(), String> {
    let old = file_ops::read_text(Path::new(&path))?;
    let source = if source.trim().is_empty() {
        CAPTION_SOURCE_MANUAL
    } else {
        &source
    };
    runtime
        .storage
        .commit_mutation(&path, &old, &content, source, None, "manual_save")
        .map(|_| ())
}

#[tauri::command]
fn preview_caption_edit(
    runtime: State<'_, Arc<AppRuntime>>,
    paths: Vec<String>,
    rule: CaptionEditRule,
) -> Result<CaptionEditBatch, String> {
    preview_caption_edit_batch(&runtime.storage, &paths, &rule)
}

#[tauri::command]
fn apply_caption_edit(
    runtime: State<'_, Arc<AppRuntime>>,
    preview_batch_id: String,
) -> Result<CaptionEditBatch, String> {
    file_ops::apply_caption_edit(&runtime.storage, &preview_batch_id)
}

#[tauri::command]
fn list_versions(
    runtime: State<'_, Arc<AppRuntime>>,
    path: Option<String>,
) -> Result<Vec<CaptionVersion>, String> {
    runtime.storage.list_versions(path.as_deref())
}

#[tauri::command]
fn list_edit_batches(runtime: State<'_, Arc<AppRuntime>>) -> Result<Vec<EditBatchRecord>, String> {
    runtime.storage.list_edit_batches()
}

#[tauri::command]
fn query_stats(runtime: State<'_, Arc<AppRuntime>>) -> Result<HistoricalStats, String> {
    runtime.storage.query_stats()
}

#[tauri::command]
fn query_stats_breakdown(
    runtime: State<'_, Arc<AppRuntime>>,
    query: StatsQuery,
) -> Result<StatsBreakdown, String> {
    runtime.storage.query_stats_breakdown(&query)
}

#[tauri::command]
fn restore_version(runtime: State<'_, Arc<AppRuntime>>, version_id: i64) -> Result<(), String> {
    let version = runtime.storage.get_version(version_id)?;
    let current = file_ops::read_text(Path::new(&version.caption_path))?;
    runtime.storage.commit_mutation(
        &version.caption_path,
        &current,
        &version.old_content,
        CAPTION_SOURCE_UNDO,
        None,
        "restore",
    )?;
    Ok(())
}

/// Batch undo: only files whose current content still equals the batch's new
/// content are restored; externally modified files are listed as conflicts
/// and left untouched.
#[tauri::command]
fn undo_edit_batch(
    runtime: State<'_, Arc<AppRuntime>>,
    batch_id: String,
) -> Result<UndoBatchResult, String> {
    let mut result = UndoBatchResult {
        batch_id: batch_id.clone(),
        restored: Vec::new(),
        skipped: Vec::new(),
        conflicts: Vec::new(),
        errors: Vec::new(),
    };
    let mut versions = runtime.storage.versions_for_batch(&batch_id)?;
    versions.reverse(); // earliest mutation first
    let mut first_by_path: HashMap<String, &CaptionVersion> = HashMap::new();
    for version in &versions {
        first_by_path
            .entry(version.caption_path.clone())
            .or_insert(version);
    }
    let mut latest_by_path: HashMap<String, &CaptionVersion> = HashMap::new();
    for version in versions.iter().rev() {
        latest_by_path.insert(version.caption_path.clone(), version);
    }
    for (path, first) in first_by_path {
        if result.restored.contains(&path)
            || result.skipped.contains(&path)
            || result.conflicts.contains(&path)
        {
            continue;
        }
        let current = match file_ops::read_text(Path::new(&path)) {
            Ok(value) => value,
            Err(error) => {
                result.errors.push((path.clone(), error));
                continue;
            }
        };
        let expected_new = latest_by_path
            .get(&path)
            .map(|version| version.new_content.clone())
            .unwrap_or_default();
        if current == expected_new {
            match runtime.storage.commit_mutation(
                &path,
                &current,
                &first.old_content,
                CAPTION_SOURCE_UNDO,
                Some(&batch_id),
                "batch_undo",
            ) {
                Ok(_) => result.restored.push(path),
                Err(error) => result.errors.push((path, error)),
            }
        } else if current == first.old_content {
            result.skipped.push(path);
        } else {
            result.conflicts.push(path);
        }
    }
    Ok(result)
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let storage = Storage::open(&app_dir)?;
            let model_dir = choose_model_directory(app.path().document_dir().ok());
            let cuda_cache_dir = model_dir.join("cuda-cache");
            let _ = fs::create_dir_all(&cuda_cache_dir);
            std::env::set_var("CUDA_CACHE_PATH", &cuda_cache_dir);
            std::env::set_var("HF_HOME", model_dir.join("huggingface-cache"));
            std::env::set_var("HUGGINGFACE_HUB_CACHE", model_dir.join("huggingface-cache"));
            // 本地打标模型依赖 ONNX Runtime：打包的 CPU 版 dll 作为资源随应用分发，
            // 运行时显式加载；开发模式下资源不存在时回退系统默认搜索。
            match app.path().resource_dir() {
                Ok(resource_dir) => {
                    // 兼容两种布局：资源在 resource_dir 下，或在 resources/ 子目录。
                    let mut ort_dll = resource_dir.join("onnxruntime/onnxruntime.dll");
                    if !ort_dll.is_file() {
                        ort_dll = resource_dir.join("resources/onnxruntime/onnxruntime.dll");
                    }
                    if ort_dll.is_file() {
                        std::env::set_var("ORT_DYLIB_PATH", &ort_dll);
                        // CUDA EP 依赖 cudart/cublas/cudnn 运行库：随应用打包在 cuda/
                        // 子目录，注入进程 PATH 使其可被动态加载。
                        let mut cuda_dirs = vec![
                            resource_dir.join("cuda"),
                            resource_dir.join("resources/cuda"),
                        ];
                        cuda_dirs.retain(|dir| dir.is_dir());
                        if let Some(cuda_dir) = cuda_dirs.first() {
                            if let Ok(existing) = std::env::var("PATH") {
                                std::env::set_var(
                                    "PATH",
                                    format!("{};{existing}", cuda_dir.display()),
                                );
                            } else {
                                std::env::set_var("PATH", cuda_dir.display().to_string());
                            }
                            storage.write_log(
                                "info",
                                "system",
                                &format!("CUDA 运行库目录：{}", cuda_dir.display()),
                            );
                        }
                        storage.write_log(
                            "info",
                            "system",
                            &format!("ONNX Runtime 已就绪：{}", ort_dll.display()),
                        );
                    } else {
                        storage.write_log(
                            "info",
                            "system",
                            &format!(
                                "未找到随包 ONNX Runtime（resource_dir={}），回退系统搜索",
                                resource_dir.display()
                            ),
                        );
                    }
                }
                Err(error) => {
                    storage.write_log("warn", "system", &format!("resource_dir 不可用：{error}"))
                }
            }
            let runtime = Arc::new(AppRuntime::new_with_model_dir(storage, model_dir.clone())?);
            runtime.storage.write_log("info", "system", "应用启动");
            runtime.storage.write_log(
                "info",
                "system",
                &format!("本地打标模型目录：{}", model_dir.display()),
            );
            app.manage(runtime.clone());
            let legacy_models = app_dir.join("models");
            if legacy_models.is_dir() && legacy_models != model_dir {
                let target_models = model_dir.clone();
                let migration_storage = runtime.storage.clone();
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let migration = tauri::async_runtime::spawn_blocking(move || {
                        local_model::migrate_legacy_models(&legacy_models, &target_models)
                    })
                    .await;
                    match migration {
                        Ok(Ok(model_ids)) => {
                            if !model_ids.is_empty() {
                                migration_storage.write_log(
                                    "info",
                                    "system",
                                    &format!("已将 {} 个旧模型迁移到非系统盘", model_ids.len()),
                                );
                                for model_id in model_ids {
                                    let _ = app_handle.emit(
                                        "local-model-event",
                                        local_model::LocalModelEvent {
                                            model_id,
                                            status: "done".into(),
                                            received: 0,
                                            total: 0,
                                            error: None,
                                        },
                                    );
                                }
                            }
                        }
                        Ok(Err(error)) => migration_storage.write_log(
                            "warn",
                            "system",
                            &format!("旧模型迁移未完成，源文件已保留：{error}"),
                        ),
                        Err(error) => migration_storage.write_log(
                            "warn",
                            "system",
                            &format!("旧模型迁移线程异常：{error}"),
                        ),
                    }
                });
            }
            // SQLite 在线备份可能需要数秒。延后到首屏和 bootstrap 完成后，
            // 并放到阻塞线程执行，避免 Windows 将初次打开判定为“未响应”。
            let storage_for_backup = runtime.storage.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    storage_for_backup.backup_if_due();
                })
                .await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            save_connection,
            delete_connection,
            list_models,
            test_connection,
            select_images,
            select_folder,
            select_text_files,
            select_text_folder,
            select_lora_files,
            inspect_lora_metadata,
            sanitize_lora_metadata,
            load_thumbnails,
            get_image_preview,
            save_preset,
            delete_preset,
            start_batch,
            cancel_batch,
            list_local_models,
            list_reviewed_paths,
            ignore_review_error,
            select_folder_path,
            scan_folder_paths,
            scan_folder_jobs,
            sample_images,
            apply_sample,
            move_ai_sample_image,
            return_ai_sample_image,
            reset_ai_sample,
            download_local_model,
            get_task_runtime,
            save_caption,
            preview_caption_edit,
            apply_caption_edit,
            list_versions,
            list_edit_batches,
            query_stats,
            query_stats_breakdown,
            restore_version,
            undo_edit_batch
        ])
        .run(tauri::generate_context!())
        .expect("failed to run LoRA Caption Studio");
}
