use crate::{
    file_ops,
    file_ops::{image_job, read_text},
    local_model,
    models::{
        masked_secret, ApiChannelSelection, ApiConnection, ApiKeyRef, ApiKeyRuntimeState,
        BatchRequest, ImageJob, JobKind, KeyRuntimeEvent, TaskEvent, TaskItem, TaskRun,
        TaskRuntimeState, UsageMetrics,
    },
    providers::{
        detect_refusal, stream_caption, with_request_params, ApiError, ApiErrorKind,
        ProviderResponse,
    },
    storage::{Storage, CAPTION_SOURCE_LOCAL, CAPTION_SOURCE_REVISION},
};
use chrono::Utc;
use parking_lot::Mutex;
use reqwest::Client;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Runtime};
use tokio::{sync::OwnedSemaphorePermit, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub struct AppRuntime {
    pub storage: Arc<Storage>,
    pub http: Client,
    pub models: Arc<local_model::LocalModelStore>,
    pub cancellations: Mutex<HashMap<String, CancellationToken>>,
    runs: Mutex<HashMap<String, Arc<Mutex<RunInfo>>>>,
}

pub(crate) struct RunInfo {
    pub batch_id: String,
    pub status: String,
    pub total: u32,
    pub running: u32,
    pub finished: u32,
    pub slots: Vec<Arc<KeySlot>>,
}

impl AppRuntime {
    #[cfg(test)]
    pub fn new(storage: Storage, app_dir: &std::path::Path) -> Result<Self, String> {
        Self::new_with_model_dir(storage, app_dir.join("models"))
    }

    pub fn new_with_model_dir(
        storage: Storage,
        model_dir: std::path::PathBuf,
    ) -> Result<Self, String> {
        let http = Client::builder()
            .user_agent("LoRA-Caption-Studio/0.1")
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            storage: Arc::new(storage),
            http,
            models: Arc::new(local_model::LocalModelStore::with_model_dir(model_dir)),
            cancellations: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
        })
    }

    pub fn get_task_runtime(&self, batch_id: &str) -> Option<TaskRuntimeState> {
        let run = self.runs.lock().get(batch_id)?.clone();
        let run = run.lock();
        let keys = run
            .slots
            .iter()
            .map(|slot| {
                let cooldown = slot
                    .cooldown_until
                    .lock()
                    .map(|until| {
                        let now = Instant::now();
                        if until <= now {
                            None
                        } else {
                            Some(until.duration_since(now).as_millis() as u64)
                        }
                    })
                    .unwrap_or(None);
                ApiKeyRuntimeState {
                    key: ApiKeyRef {
                        id: slot.id.clone(),
                        label: slot.label.clone(),
                        masked: slot.masked.clone(),
                    },
                    active: slot.active.load(Ordering::SeqCst) as u32,
                    succeeded: slot.succeeded.load(Ordering::SeqCst) as u32,
                    failed: slot.failed.load(Ordering::SeqCst) as u32,
                    tokens: slot.tokens.load(Ordering::SeqCst),
                    cooldown_remaining_ms: cooldown,
                    disabled: slot.disabled.load(Ordering::SeqCst),
                    disable_reason: slot.disable_reason.lock().clone(),
                }
            })
            .collect();
        Some(TaskRuntimeState {
            batch_id: run.batch_id.clone(),
            status: run.status.clone(),
            total: run.total,
            running: run.running,
            finished: run.finished,
            keys,
        })
    }
}

pub(crate) struct KeySlot {
    id: String,
    label: String,
    masked: String,
    secret: String,
    /// 所属连接（含 base_url/headers/timeout/provider，请求按此路由）。
    connection: Arc<ApiConnection>,
    /// 该渠道使用的模型 ID（渠道级，可与主模型不同）。
    model_id: String,
    connection_name: String,
    active: AtomicUsize,
    succeeded: AtomicU64,
    failed: AtomicU64,
    tokens: AtomicU64,
    disabled: AtomicBool,
    disable_reason: Mutex<Option<String>>,
    cooldown_until: Mutex<Option<Instant>>,
    semaphore: Arc<tokio::sync::Semaphore>,
}

struct KeyLease {
    slot: Arc<KeySlot>,
    _permit: OwnedSemaphorePermit,
}

impl Drop for KeyLease {
    fn drop(&mut self) {
        self.slot.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn emit<R: tauri::Runtime>(
    app: &AppHandle<R>,
    storage: &Storage,
    mut event: TaskEvent,
    scope: &str,
) {
    event.scope = Some(scope.to_string());
    if let Some(message) = &event.message {
        storage.write_log(event.level.as_deref().unwrap_or("info"), scope, message);
    }
    let _ = app.emit("task-event", event);
}

fn emit_key_runtime<R: tauri::Runtime>(app: &AppHandle<R>, runtime: &AppRuntime, batch_id: &str) {
    if let Some(state) = runtime.get_task_runtime(batch_id) {
        let _ = app.emit(
            "key-runtime-event",
            KeyRuntimeEvent {
                batch_id: batch_id.into(),
                keys: state.keys,
            },
        );
    }
}

/// Picks the least-active available key. `prefer_avoid` migrates the request
/// away from a key that just failed when another key can take it.
async fn acquire_key(
    slots: &[Arc<KeySlot>],
    cancel: &CancellationToken,
    prefer_avoid: Option<&str>,
) -> Result<KeyLease, String> {
    loop {
        if cancel.is_cancelled() {
            return Err("任务已取消".into());
        }
        let now = Instant::now();
        let mut candidates = slots
            .iter()
            .filter(|slot| {
                if slot.disabled.load(Ordering::SeqCst) {
                    return false;
                }
                slot.cooldown_until
                    .lock()
                    .map(|until| until <= now)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(avoid) = prefer_avoid {
            if candidates.iter().any(|slot| slot.id != avoid) {
                candidates.retain(|slot| slot.id != avoid);
            }
        }
        candidates.sort_by_key(|slot| slot.active.load(Ordering::SeqCst));
        for slot in candidates {
            if let Ok(permit) = slot.semaphore.clone().try_acquire_owned() {
                slot.active.fetch_add(1, Ordering::SeqCst);
                return Ok(KeyLease {
                    slot,
                    _permit: permit,
                });
            }
        }
        if slots
            .iter()
            .all(|slot| slot.disabled.load(Ordering::SeqCst))
        {
            return Err("所有 API Key 均已停用".into());
        }
        tokio::select! { _ = cancel.cancelled() => return Err("任务已取消".into()), _ = tokio::time::sleep(Duration::from_millis(200)) => {} }
    }
}

fn initial_job(path: &str, status: &str) -> ImageJob {
    let empty = file_ops::ReviewedSet::new();
    let no_counts = file_ops::RevisionCounts::new();
    let mut job = image_job(Path::new(path), &empty, &no_counts).unwrap_or_else(|error| ImageJob {
        id: Uuid::new_v4().to_string(),
        image_path: path.into(),
        caption_path: Path::new(path)
            .with_extension("txt")
            .to_string_lossy()
            .into(),
        file_name: Path::new(path)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("image")
            .into(),
        thumbnail: None,
        caption: String::new(),
        reviewed: false,
        revision_count: 0,
        status: status.into(),
        selected: false,
        error: Some(error),
        error_kind: Some("fatal".into()),
        refusal: None,
        metrics: None,
        key_label: None,
        updated_at: None,
        sample_score: None,
    });
    job.status = status.into();
    job
}

/// ApiError 分类 → 前端可显示的失败原因分类。
fn error_kind_name(error: &ApiError) -> String {
    match error.kind {
        ApiErrorKind::RateLimit => "rate_limit",
        ApiErrorKind::Auth => "auth",
        ApiErrorKind::Quota => "quota",
        ApiErrorKind::PromptRejected => "prompt_rejected",
        ApiErrorKind::Cancelled => "cancelled",
        ApiErrorKind::Transient => {
            if error.message.contains("被截断") {
                "truncated"
            } else {
                "transient"
            }
        }
        ApiErrorKind::Fatal => {
            if error.message.contains("Token 上限") {
                "truncated"
            } else {
                "fatal"
            }
        }
    }
    .to_string()
}

/// AI 抽样响应协议：第一行必须给出 KEEP/REJECT，第二行可给简短理由，
/// 第三行可给 0–100 分，用于多轮筛选的最终择优收敛。
/// 对常见的冒号、大小写和 Markdown 加粗做容错，但不猜测模糊自然语言。
fn parse_ai_sample_decision(text: &str) -> Result<(bool, String, Option<f64>), String> {
    let cleaned = text.replace("**", "");
    let lines: Vec<&str> = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let first = lines.first().copied().unwrap_or_default();
    let token = first
        .split_once(':')
        .or_else(|| first.split_once('：'))
        .map(|(_, value)| value.trim())
        .unwrap_or(first)
        .to_ascii_uppercase();
    let keep = match token.as_str() {
        "KEEP" => true,
        "REJECT" => false,
        _ => return Err(format!("AI 抽样返回格式无效：{first}")),
    };
    let reason = lines
        .get(1)
        .map(|line| {
            line.split_once(':')
                .or_else(|| line.split_once('：'))
                .map(|(_, value)| value.trim())
                .unwrap_or(line)
                .to_string()
        })
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| {
            if keep {
                "符合筛选要求"
            } else {
                "不符合筛选要求"
            }
            .into()
        });
    let score = lines.get(2).and_then(|line| {
        let value = line
            .split_once(':')
            .or_else(|| line.split_once('：'))
            .map(|(_, value)| value.trim())
            .unwrap_or(line);
        value
            .parse::<f64>()
            .ok()
            .filter(|score| (0.0..=100.0).contains(score))
    });
    Ok((keep, reason, score))
}

fn apply_key_error(slot: &KeySlot, error: &ApiError, attempt: usize) {
    match error.kind {
        ApiErrorKind::RateLimit => {
            let fallback = if attempt == 0 { 5 } else { 10 };
            *slot.cooldown_until.lock() =
                Some(Instant::now() + error.retry_after.unwrap_or(Duration::from_secs(fallback)));
        }
        ApiErrorKind::Auth => {
            slot.disabled.store(true, Ordering::SeqCst);
            *slot.disable_reason.lock() = Some("认证失败（401/403），Key 已停用".to_string());
        }
        ApiErrorKind::Quota => {
            slot.disabled.store(true, Ordering::SeqCst);
            *slot.disable_reason.lock() = Some("额度用尽（quota/billing），Key 已停用".to_string());
        }
        _ => {}
    }
}

const ADVANCED_REVIEW_PROMPT: &str = r#"你是 LoRA 图像标签的高级质量审核模型。当前请求提供原图、原始标签和一个长度异常缩短的候选标签。
请重新对照图片审核两份标签，修正候选中的漏标、错标、重复和格式问题。
只输出最终完整替换标签，不要解释、不要使用 Markdown，也不要输出 APPROVE/REJECT。最终结果必须保留原标签中仍能从图片确认的重要内容。"#;

fn merge_usage(primary: &UsageMetrics, review: &UsageMetrics) -> UsageMetrics {
    UsageMetrics {
        input_tokens: match (primary.input_tokens, review.input_tokens) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
        output_tokens: match (primary.output_tokens, review.output_tokens) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
        total_tokens: match (primary.total_tokens, review.total_tokens) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
        ttft_ms: review.ttft_ms.or(primary.ttft_ms),
        generation_ms: match (primary.generation_ms, review.generation_ms) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        },
        tokens_per_second: review.tokens_per_second.or(primary.tokens_per_second),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_advanced_length_review(
    runtime: &AppRuntime,
    snapshot: &crate::models::ApiSelectionSnapshot,
    batch_id: &str,
    preset_id: Option<&str>,
    slots: &[Arc<KeySlot>],
    cancel: &CancellationToken,
    path: &str,
    original: &str,
    candidate: &str,
) -> Result<(ProviderResponse, String, String), String> {
    let config = snapshot
        .advanced_model
        .as_ref()
        .ok_or("未配置高级审核模型")?;
    if !config.enabled
        || config.connection_id.trim().is_empty()
        || config.model_id.trim().is_empty()
        || slots.is_empty()
    {
        return Err("高级审核模型未启用或配置不完整".into());
    }
    let review_context = format!("<original_caption>\n{original}\n</original_caption>\n<candidate_caption>\n{candidate}\n</candidate_caption>");
    let mut previous_key: Option<String> = None;
    let mut last_error = String::new();
    let mut accumulated_usage: Option<UsageMetrics> = None;
    for attempt in 0..3 {
        let lease = acquire_key(slots, cancel, previous_key.as_deref()).await?;
        let key_id = lease.slot.id.clone();
        let key_label = format!("{}/{}", lease.slot.connection_name, lease.slot.label);
        let connection =
            with_request_params(&lease.slot.connection, config.request_params.as_ref());
        match stream_caption(
            &runtime.http,
            &connection,
            &lease.slot.secret,
            JobKind::Revision,
            &config.model_id,
            ADVANCED_REVIEW_PROMPT,
            &review_context,
            path,
            0.1,
            snapshot.max_tokens.max(512),
            cancel.clone(),
        )
        .await
        {
            Ok(mut response) => {
                lease
                    .slot
                    .tokens
                    .fetch_add(response.usage.total_tokens.unwrap_or(0), Ordering::SeqCst);
                let aggregate = accumulated_usage
                    .as_ref()
                    .map(|usage| merge_usage(usage, &response.usage))
                    .unwrap_or_else(|| response.usage.clone());
                accumulated_usage = Some(aggregate.clone());
                let refusal = detect_refusal(
                    &response.text,
                    &snapshot.refusal_rules,
                    response.structured_refusal.as_deref(),
                );
                if refusal.is_none()
                    && !response.text.trim().is_empty()
                    && response.text.chars().count() * 2 >= original.chars().count()
                {
                    lease.slot.succeeded.fetch_add(1, Ordering::SeqCst);
                    let _ = runtime.storage.add_metric(
                        batch_id,
                        "advanced_review",
                        &lease.slot.connection.id,
                        &lease.slot.connection_name,
                        &config.model_id,
                        preset_id,
                        Some(path),
                        Some(&key_id),
                        attempt as u32,
                        "succeeded",
                        &response.usage,
                    );
                    response.usage = aggregate;
                    return Ok((response, key_id, key_label));
                }
                lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                let _ = runtime.storage.add_metric(
                    batch_id,
                    "advanced_review",
                    &lease.slot.connection.id,
                    &lease.slot.connection_name,
                    &config.model_id,
                    preset_id,
                    Some(path),
                    Some(&key_id),
                    attempt as u32,
                    if refusal.is_some() {
                        "refused"
                    } else {
                        "failed"
                    },
                    &response.usage,
                );
                last_error = if refusal.is_some() {
                    "高级模型拒绝审核".into()
                } else {
                    "高级模型结果仍为空或长度异常".into()
                };
            }
            Err(error) => {
                apply_key_error(&lease.slot, &error, attempt);
                lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                last_error = error.message;
            }
        }
        previous_key = Some(key_id);
        if attempt < 2 {
            tokio::select! { _ = cancel.cancelled() => return Err("任务已取消".into()), _ = tokio::time::sleep(Duration::from_secs(if attempt == 0 { 2 } else { 4 })) => {} }
        }
    }
    Err(if last_error.is_empty() {
        "高级审核失败".into()
    } else {
        last_error
    })
}

#[allow(clippy::too_many_arguments)]
async fn process_one<R: Runtime>(
    app: AppHandle<R>,
    runtime: Arc<AppRuntime>,
    batch_id: String,
    request: BatchRequest,
    snapshot_connection_name: String,
    slots: Arc<Vec<Arc<KeySlot>>>,
    advanced_slots: Arc<Vec<Arc<KeySlot>>>,
    cancel: CancellationToken,
    path: String,
) {
    let scope = request.kind.scope();
    let run_slot = runtime.runs.lock().get(&batch_id).cloned();
    let mut job = initial_job(&path, "running");
    job.status = "running".into();
    job.error = None;
    if cancel.is_cancelled() {
        job.status = "cancelled".into();
        job.error = Some("任务已取消".into());
        if let Some(run) = &run_slot {
            run.lock().finished += 1;
        }
        emit(
            &app,
            &runtime.storage,
            TaskEvent::job(&batch_id, job.clone()),
            scope,
        );
        record_task_item(&runtime, &batch_id, &job, 0, None);
        return;
    }
    emit(
        &app,
        &runtime.storage,
        TaskEvent::job(&batch_id, job.clone()),
        scope,
    );
    let caption_path = Path::new(&job.caption_path);
    let existing = read_text(caption_path).unwrap_or_default();
    if request.kind == JobKind::Caption && !request.overwrite && caption_path.exists() {
        job.status = "skipped".into();
        job.caption = existing;
        job.updated_at = Some(Utc::now().to_rfc3339());
        if let Some(run) = &run_slot {
            run.lock().finished += 1;
        }
        emit(
            &app,
            &runtime.storage,
            TaskEvent::job(&batch_id, job.clone()),
            scope,
        );
        record_task_item(&runtime, &batch_id, &job, 0, None);
        return;
    }
    if request.kind == JobKind::Revision && existing.trim().is_empty() {
        job.status = "failed".into();
        job.error = Some("当前文本文档为空，未执行复核".into());
        if let Some(run) = &run_slot {
            run.lock().finished += 1;
        }
        emit(
            &app,
            &runtime.storage,
            TaskEvent::job(&batch_id, job.clone()),
            scope,
        );
        record_task_item(&runtime, &batch_id, &job, 0, None);
        return;
    }
    let snapshot = &request.selection_snapshot;
    if let Some(model_id) = &snapshot.local_model_id {
        // 本地打标模型：未安装时先自动下载，再推理。
        if cancel.is_cancelled() {
            job.status = "cancelled".into();
            job.error = Some("任务已取消".into());
            if let Some(run) = &run_slot {
                run.lock().finished += 1;
            }
            emit(
                &app,
                &runtime.storage,
                TaskEvent::job(&batch_id, job.clone()),
                scope,
            );
            record_task_item(&runtime, &batch_id, &job, 0, None);
            return;
        }
        emit(
            &app,
            &runtime.storage,
            TaskEvent::log(
                &batch_id,
                "info",
                format!("{} 使用本地模型 {} 打标", job.file_name, model_id),
            ),
            scope,
        );
        job.key_label = Some("本地模型".into());
        if !runtime.models.installed(model_id) {
            emit(
                &app,
                &runtime.storage,
                TaskEvent::log(
                    &batch_id,
                    "info",
                    format!("本地模型 {model_id} 未安装，开始自动下载…"),
                ),
                scope,
            );
            if let Err(error) =
                local_model::download_model(&runtime.http, app.clone(), &runtime.models, model_id)
                    .await
            {
                job.status = "failed".into();
                job.error = Some(format!("本地模型下载失败：{error}"));
                job.error_kind = Some("download_failed".into());
                if let Some(run) = &run_slot {
                    run.lock().finished += 1;
                }
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::job(&batch_id, job.clone()),
                    scope,
                );
                record_task_item(&runtime, &batch_id, &job, 0, None);
                return;
            }
            emit(
                &app,
                &runtime.storage,
                TaskEvent::log(&batch_id, "info", "本地模型下载完成"),
                scope,
            );
        }
        if cancel.is_cancelled() {
            job.status = "cancelled".into();
            job.error = Some("任务已取消".into());
            if let Some(run) = &run_slot {
                run.lock().finished += 1;
            }
            emit(
                &app,
                &runtime.storage,
                TaskEvent::job(&batch_id, job.clone()),
                scope,
            );
            record_task_item(&runtime, &batch_id, &job, 0, None);
            return;
        }
        // ONNX 推理是同步且耗时的 CPU/GPU 工作，放入 blocking 池，避免阻塞 Tauri
        // 的异步调度、取消命令和进度事件。
        let model_store = runtime.models.clone();
        let local_model_id = model_id.clone();
        let local_image_path = path.clone();
        let threshold = snapshot
            .local_threshold
            .unwrap_or(local_model::DEFAULT_TAG_THRESHOLD);
        let max_tags = snapshot.local_max_tags.unwrap_or(0) as usize;
        let keep_underscores = snapshot.local_keep_underscores.unwrap_or(false);
        let inference = tauri::async_runtime::spawn_blocking(move || {
            local_model::tag_image(
                &model_store,
                &local_model_id,
                &local_image_path,
                threshold,
                max_tags,
                keep_underscores,
            )
        })
        .await
        .unwrap_or_else(|error| Err(format!("本地推理线程异常：{error}")));
        match inference {
            Ok((text, metrics)) => {
                if runtime.models.mark_ep_reported() {
                    let device = runtime
                        .models
                        .ep_name()
                        .unwrap_or_else(|| "CPU".to_string());
                    emit(
                        &app,
                        &runtime.storage,
                        TaskEvent::log(&batch_id, "info", format!("本地推理执行设备：{device}")),
                        scope,
                    );
                }
                job.metrics = Some(metrics.clone());
                job.updated_at = Some(Utc::now().to_rfc3339());
                if cancel.is_cancelled() {
                    job.status = "cancelled".into();
                    job.error = Some("任务已取消；推理结果未写入".into());
                } else if text.trim().is_empty() {
                    // 高阈值或标签表异常可能得到空结果；绝不以空文本覆盖已有 caption。
                    job.status = "no_response".into();
                    job.error = Some("本地模型未生成达到阈值的标签，原文已保留".into());
                } else {
                    let status = if request.kind == JobKind::Revision {
                        "revised"
                    } else {
                        "succeeded"
                    };
                    let mut final_text = text;
                    if !existing.is_empty()
                        && final_text.chars().count() * 2 < existing.chars().count()
                        && (request.kind == JobKind::Revision
                            || snapshot
                                .advanced_model
                                .as_ref()
                                .is_some_and(|value| value.enabled))
                    {
                        emit(
                            &app,
                            &runtime.storage,
                            TaskEvent::log(
                                &batch_id,
                                "warn",
                                format!(
                                    "{} 检测到本地标签长度骤减，正在调用高级模型审核",
                                    job.file_name
                                ),
                            ),
                            scope,
                        );
                        match run_advanced_length_review(
                            &runtime,
                            snapshot,
                            &batch_id,
                            request.preset_id.as_deref(),
                            &advanced_slots,
                            &cancel,
                            &path,
                            &existing,
                            &final_text,
                        )
                        .await
                        {
                            Ok((review, _review_key_id, review_key_label)) => {
                                final_text = review.text.trim().to_string();
                                job.key_label = Some(review_key_label);
                                job.metrics = Some(merge_usage(&metrics, &review.usage));
                            }
                            Err(error) => {
                                job.status = "failed".into();
                                job.error_kind = Some("length_reduced".into());
                                job.error = Some(format!(
                                    "本地标签长度骤减；高级模型审核未通过：{error}。原文已保留"
                                ));
                                if let Some(run) = &run_slot {
                                    run.lock().finished += 1;
                                }
                                emit(
                                    &app,
                                    &runtime.storage,
                                    TaskEvent::job(&batch_id, job.clone()),
                                    scope,
                                );
                                record_task_item(&runtime, &batch_id, &job, 0, None);
                                return;
                            }
                        }
                    }
                    let write_result = runtime.storage.commit_mutation(
                        &job.caption_path,
                        &existing,
                        &final_text,
                        CAPTION_SOURCE_LOCAL,
                        Some(&batch_id),
                        "local_overwrite",
                    );
                    match write_result {
                        Ok(_) => {
                            job.status = status.into();
                            job.caption = final_text;
                        }
                        Err(error) => {
                            job.status = "failed".into();
                            job.error = Some(format!("保存标签失败：{error}"));
                            job.error_kind = Some(
                                if error.contains("发生变化") {
                                    "write_conflict"
                                } else {
                                    "write_error"
                                }
                                .into(),
                            );
                            job.caption = read_text(caption_path).unwrap_or(existing.clone());
                        }
                    }
                }
                // 本地打标不写入 request_metrics：性能指标仅统计 API 请求。
                if let Some(run) = &run_slot {
                    run.lock().finished += 1;
                }
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::job(&batch_id, job.clone()),
                    scope,
                );
                record_task_item(&runtime, &batch_id, &job, 0, None);
                return;
            }
            Err(error) => {
                job.status = "failed".into();
                job.error = Some(error);
                job.error_kind = Some("fatal".into());
                if let Some(run) = &run_slot {
                    run.lock().finished += 1;
                }
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::job(&batch_id, job.clone()),
                    scope,
                );
                record_task_item(&runtime, &batch_id, &job, 0, None);
                return;
            }
        }
    }
    let mut final_error: Option<ApiError> = None;
    let mut acquire_error: Option<String> = None;
    let mut last_key_id: Option<String> = None;
    let mut accumulated_usage: Option<UsageMetrics> = None;
    // 循环上限放宽到 6：实际重试次数由各错误类别的 max_attempts 决定。
    for attempt in 0..6 {
        if cancel.is_cancelled() {
            job.status = "cancelled".into();
            job.error = Some("任务已取消".into());
            break;
        }
        if attempt > 0 {
            let fallback = if attempt == 1 {
                3
            } else if attempt == 2 {
                5
            } else if attempt == 3 {
                7
            } else if attempt == 4 {
                9
            } else {
                12
            };
            let wait = final_error
                .as_ref()
                .and_then(|error| error.retry_after)
                .unwrap_or(Duration::from_secs(fallback));
            tokio::select! {
                _ = cancel.cancelled() => {
                    job.status = "cancelled".into();
                    job.error = Some("任务已取消".into());
                    break;
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
        let lease = match acquire_key(&slots, &cancel, last_key_id.as_deref()).await {
            Ok(value) => value,
            Err(error) => {
                job.status = if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                job.error = Some(error.clone());
                acquire_error = Some(error);
                break;
            }
        };
        let key_id = lease.slot.id.clone();
        let key_label = lease.slot.label.clone();
        job.key_label = Some(format!("{}/{}", lease.slot.connection_name, key_label));
        emit(
            &app,
            &runtime.storage,
            TaskEvent::log(
                &batch_id,
                "info",
                format!(
                    "{} 使用 {} 开始请求{}",
                    job.file_name,
                    key_label,
                    if attempt > 0 {
                        format!("（重试 {attempt}）")
                    } else {
                        String::new()
                    }
                ),
            ),
            scope,
        );
        let effective_connection =
            with_request_params(&lease.slot.connection, snapshot.request_params.as_ref());
        let result = stream_caption(
            &runtime.http,
            &effective_connection,
            &lease.slot.secret,
            request.kind,
            &lease.slot.model_id,
            &snapshot.system_prompt,
            &existing,
            &path,
            snapshot.temperature,
            snapshot.max_tokens,
            cancel.clone(),
        )
        .await;
        match result {
            Ok(response) => {
                lease
                    .slot
                    .tokens
                    .fetch_add(response.usage.total_tokens.unwrap_or(0), Ordering::SeqCst);
                let aggregate = accumulated_usage
                    .as_ref()
                    .map(|usage| merge_usage(usage, &response.usage))
                    .unwrap_or_else(|| response.usage.clone());
                accumulated_usage = Some(aggregate.clone());
                // AI 抽样的正常 REJECT 理由可能包含“无法确认/不符合”等字样，
                // 不应被通用文本拒绝规则误判；供应商结构化安全拒绝仍然生效。
                let refusal_text = if request.kind == JobKind::AiSample {
                    ""
                } else {
                    &response.text
                };
                let refusal = detect_refusal(
                    refusal_text,
                    &snapshot.refusal_rules,
                    response.structured_refusal.as_deref(),
                );
                job.metrics = Some(aggregate);
                job.updated_at = Some(Utc::now().to_rfc3339());
                let mut status;
                if let Some(refusal) = refusal {
                    if request.kind != JobKind::AiSample && attempt < 2 {
                        lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                        let _ = runtime.storage.add_metric(
                            &batch_id,
                            scope,
                            &lease.slot.connection.id,
                            &lease.slot.connection_name,
                            &lease.slot.model_id,
                            request.preset_id.as_deref(),
                            Some(&path),
                            Some(&key_id),
                            attempt as u32,
                            "refused",
                            &response.usage,
                        );
                        emit(
                            &app,
                            &runtime.storage,
                            TaskEvent::log(
                                &batch_id,
                                "warn",
                                format!(
                                    "{} 命中拒绝检测，自动重试 {}/2",
                                    job.file_name,
                                    attempt + 1
                                ),
                            ),
                            scope,
                        );
                        last_key_id = Some(key_id);
                        emit_key_runtime(&app, &runtime, &batch_id);
                        continue;
                    }
                    job.status = "refused".into();
                    lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                    job.refusal = Some(refusal);
                    job.caption = response.text;
                    status = "refused";
                    emit(
                        &app,
                        &runtime.storage,
                        TaskEvent::log(
                            &batch_id,
                            "warn",
                            format!("{} 命中拒绝检测，保留原文件", job.file_name),
                        ),
                        scope,
                    );
                } else if response.text.trim().is_empty() {
                    lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                    job.status = "failed".into();
                    job.error = Some("模型返回空文本".into());
                    job.error_kind = Some("empty_reply".into());
                    status = "failed";
                } else if request.kind == JobKind::AiSample {
                    match parse_ai_sample_decision(&response.text) {
                        Ok((keep, reason, score)) => {
                            status = if keep { "succeeded" } else { "skipped" };
                            job.status = status.into();
                            job.caption = reason;
                            job.sample_score = score;
                        }
                        Err(error) => {
                            lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                            status = "failed";
                            job.status = "failed".into();
                            job.error = Some(error);
                            job.error_kind = Some("invalid_decision".into());
                            job.caption = response.text;
                        }
                    }
                } else if request.kind != JobKind::AiSample
                    && !existing.is_empty()
                    && response.text.chars().count() * 2 < existing.chars().count()
                    && (request.kind == JobKind::Revision
                        || snapshot
                            .advanced_model
                            .as_ref()
                            .is_some_and(|value| value.enabled))
                {
                    emit(
                        &app,
                        &runtime.storage,
                        TaskEvent::log(
                            &batch_id,
                            "warn",
                            format!("{} 检测到标签长度骤减，正在调用高级模型审核", job.file_name),
                        ),
                        scope,
                    );
                    match run_advanced_length_review(
                        &runtime,
                        snapshot,
                        &batch_id,
                        request.preset_id.as_deref(),
                        &advanced_slots,
                        &cancel,
                        &path,
                        &existing,
                        &response.text,
                    )
                    .await
                    {
                        Ok((review, _review_key_id, review_key_label)) => {
                            let reviewed_text = review.text.trim().to_string();
                            match runtime.storage.commit_mutation(
                                &job.caption_path,
                                &existing,
                                &reviewed_text,
                                CAPTION_SOURCE_REVISION,
                                Some(&batch_id),
                                "advanced_length_review",
                            ) {
                                Ok(_) => {
                                    status = if request.kind == JobKind::Revision {
                                        "revised"
                                    } else {
                                        "succeeded"
                                    };
                                    job.status = status.into();
                                    job.caption = reviewed_text;
                                    job.key_label = Some(review_key_label);
                                    job.metrics = Some(merge_usage(
                                        accumulated_usage.as_ref().unwrap_or(&response.usage),
                                        &review.usage,
                                    ));
                                    emit(
                                        &app,
                                        &runtime.storage,
                                        TaskEvent::log(
                                            &batch_id,
                                            "info",
                                            format!(
                                                "{} 高级模型审核完成，已写入完整标签",
                                                job.file_name
                                            ),
                                        ),
                                        scope,
                                    );
                                }
                                Err(error) => {
                                    lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                                    job.status = "failed".into();
                                    job.error = Some(format!("高级模型审核结果保存失败：{error}"));
                                    job.error_kind = Some(
                                        if error.contains("发生变化") {
                                            "write_conflict"
                                        } else {
                                            "write_error"
                                        }
                                        .into(),
                                    );
                                    job.caption =
                                        read_text(caption_path).unwrap_or(existing.clone());
                                    status = "failed";
                                }
                            }
                        }
                        Err(error) => {
                            lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                            job.status = "failed".into();
                            job.error = Some(format!("输出长度骤减（原 {} 字符 → 新 {} 字符）；高级模型审核未通过：{}。原文已保留", existing.chars().count(), response.text.chars().count(), error));
                            job.error_kind = Some("length_reduced".into());
                            status = "failed";
                        }
                    }
                } else {
                    status = if request.kind == JobKind::Revision {
                        "revised"
                    } else {
                        "succeeded"
                    };
                    let write_result = runtime.storage.commit_mutation(
                        &job.caption_path,
                        &existing,
                        &response.text,
                        CAPTION_SOURCE_REVISION,
                        Some(&batch_id),
                        if request.kind == JobKind::Revision {
                            "revision_overwrite"
                        } else {
                            "caption_overwrite"
                        },
                    );
                    match write_result {
                        Ok(_) => {
                            job.status = status.into();
                            job.caption = response.text;
                        }
                        Err(error) => {
                            lease.slot.failed.fetch_add(1, Ordering::SeqCst);
                            job.status = "failed".into();
                            job.error = Some(format!("保存标签失败：{error}"));
                            job.error_kind = Some(
                                if error.contains("发生变化") {
                                    "write_conflict"
                                } else {
                                    "write_error"
                                }
                                .into(),
                            );
                            job.caption = read_text(caption_path).unwrap_or(existing.clone());
                            status = "failed";
                        }
                    }
                }
                if matches!(status, "succeeded" | "revised" | "skipped") {
                    lease.slot.succeeded.fetch_add(1, Ordering::SeqCst);
                }
                let _ = runtime.storage.add_metric(
                    &batch_id,
                    scope,
                    &lease.slot.connection.id,
                    &lease.slot.connection_name,
                    &lease.slot.model_id,
                    request.preset_id.as_deref(),
                    Some(&path),
                    Some(&key_id),
                    attempt as u32,
                    status,
                    &response.usage,
                );
                if let Some(run) = &run_slot {
                    run.lock().finished += 1;
                }
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::job(&batch_id, job.clone()),
                    scope,
                );
                record_task_item(&runtime, &batch_id, &job, attempt as u32, Some(&key_id));
                emit_key_runtime(&app, &runtime, &batch_id);
                return;
            }
            Err(error) => {
                apply_key_error(&lease.slot, &error, attempt);
                // 无响应（超时/连接）重试上限更高：最多 6 次（约 1 分钟退避）；
                // 限流/额度/认证维持 2 次。
                let max_attempts = if error.kind == ApiErrorKind::Transient {
                    6
                } else {
                    2
                };
                let retryable = matches!(
                    error.kind,
                    ApiErrorKind::RateLimit
                        | ApiErrorKind::Transient
                        | ApiErrorKind::Auth
                        | ApiErrorKind::Quota
                        | ApiErrorKind::PromptRejected
                ) && attempt < max_attempts;
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::log(
                        &batch_id,
                        if retryable { "warn" } else { "error" },
                        format!("{}：{}", job.file_name, error.message),
                    ),
                    scope,
                );
                if error.kind == ApiErrorKind::Cancelled {
                    job.status = "cancelled".into();
                    job.error = Some(error.message);
                    break;
                }
                final_error = Some(error);
                last_key_id = Some(key_id);
                if !retryable {
                    break;
                }
                emit_key_runtime(&app, &runtime, &batch_id);
            }
        }
    }
    let cancelled = job.status == "cancelled";
    if cancelled {
        if let Some(run) = &run_slot {
            run.lock().finished += 1;
        }
        emit(
            &app,
            &runtime.storage,
            TaskEvent::job(&batch_id, job.clone()),
            scope,
        );
        record_task_item(&runtime, &batch_id, &job, 0, None);
        emit_key_runtime(&app, &runtime, &batch_id);
        return;
    }
    // 无响应（超时/连接等瞬态错误，非截断）：标记 no_response——不算错误、
    // 不算打标成功，也不写入性能指标；截断/拒绝等确定性错误仍为 failed。
    let is_no_response = final_error
        .as_ref()
        .is_some_and(|e| e.kind == ApiErrorKind::Transient && !e.message.contains("被截断"));
    job.status = if is_no_response {
        "no_response".into()
    } else {
        "failed".into()
    };
    job.error = Some(
        acquire_error.as_deref().map(String::from).or_else(|| {
            final_error.as_ref().map(|e| {
                if e.kind == ApiErrorKind::PromptRejected {
                    "提示词被服务端拒绝（The prompt could not be submitted.）：可能是图片过大、请求超长或内容不合规，重试无意义"
                        .to_string()
                } else {
                    e.message.clone()
                }
            })
        }).unwrap_or_else(|| "请求失败".into()),
    );
    job.error_kind = Some(if is_no_response {
        "no_response".to_string()
    } else {
        final_error
            .as_ref()
            .map(error_kind_name)
            .or_else(|| acquire_error.as_ref().map(|_| "no_key".to_string()))
            .unwrap_or_else(|| "fatal".to_string())
    });
    job.updated_at = Some(Utc::now().to_rfc3339());
    if let Some(run) = &run_slot {
        run.lock().finished += 1;
    }
    // no_response 不写入性能指标（不算错误也不算成功）。
    if !is_no_response {
        let empty = UsageMetrics::empty();
        let _ = runtime.storage.add_metric(
            &batch_id,
            scope,
            &snapshot.connection_id,
            &snapshot_connection_name,
            &snapshot.model_id,
            request.preset_id.as_deref(),
            Some(&path),
            None,
            0,
            "failed",
            &empty,
        );
    }
    emit(
        &app,
        &runtime.storage,
        TaskEvent::job(&batch_id, job.clone()),
        scope,
    );
    record_task_item(&runtime, &batch_id, &job, 0, None);
    emit_key_runtime(&app, &runtime, &batch_id);
}

fn record_task_item(
    runtime: &AppRuntime,
    batch_id: &str,
    job: &ImageJob,
    retries: u32,
    key_id: Option<&str>,
) {
    let _ = runtime.storage.insert_task_item(&TaskItem {
        id: 0,
        run_id: batch_id.into(),
        image_path: job.image_path.clone(),
        caption_path: job.caption_path.clone(),
        status: job.status.clone(),
        retries,
        error: job.error.clone(),
        key_id: key_id.map(String::from),
        input_tokens: job.metrics.as_ref().and_then(|m| m.input_tokens),
        output_tokens: job.metrics.as_ref().and_then(|m| m.output_tokens),
        ttft_ms: job.metrics.as_ref().and_then(|m| m.ttft_ms),
        tokens_per_second: job.metrics.as_ref().and_then(|m| m.tokens_per_second),
        created_at: Utc::now().to_rfc3339(),
    });
}

pub(crate) fn build_slots(
    connection: Arc<ApiConnection>,
    model_id: &str,
    per_key_concurrency: usize,
    storage: &Storage,
) -> Result<Vec<Arc<KeySlot>>, String> {
    let mut slots = Vec::new();
    let connection_name = connection.name.clone();
    for key in &connection.key_refs {
        let id = key.id.as_deref().ok_or("API Key 缺少 ID")?;
        if let Some(secret) = storage.key_secret(id) {
            slots.push(Arc::new(KeySlot {
                id: id.into(),
                label: key.label.clone(),
                masked: masked_secret(&secret),
                secret,
                connection: connection.clone(),
                model_id: model_id.to_string(),
                connection_name: connection_name.clone(),
                active: AtomicUsize::new(0),
                succeeded: AtomicU64::new(0),
                failed: AtomicU64::new(0),
                tokens: AtomicU64::new(0),
                disabled: AtomicBool::new(false),
                disable_reason: Mutex::new(None),
                cooldown_until: Mutex::new(None),
                semaphore: Arc::new(tokio::sync::Semaphore::new(per_key_concurrency)),
            }));
        }
    }
    if slots.is_empty() {
        return Err("连接中没有可用 API Key".into());
    }
    Ok(slots)
}

/// Dispatcher: launches up to `total_concurrency` in-flight tasks, keeps the
/// queue warm, and emits explicit `cancelled` events for everything queued
/// when the token fires. Extracted from `start_batch` so scheduler behavior
/// is testable with a mock runtime.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_batch<R: Runtime>(
    app: AppHandle<R>,
    runtime: Arc<AppRuntime>,
    request: BatchRequest,
    connection_name: String,
    slots: Arc<Vec<Arc<KeySlot>>>,
    advanced_slots: Arc<Vec<Arc<KeySlot>>>,
    cancel: CancellationToken,
    run_info: Arc<Mutex<RunInfo>>,
    batch_id: String,
) {
    let snapshot = request.selection_snapshot.clone();
    let scope = request.kind.scope();
    let _ = runtime.storage.insert_task_run(&TaskRun {
        id: batch_id.clone(),
        kind: request.kind.scope().into(),
        connection_id: snapshot.connection_id.clone(),
        connection_name: connection_name.clone(),
        model_id: snapshot.model_id.clone(),
        preset_id: request.preset_id.clone(),
        status: "running".into(),
        total: request.image_paths.len() as u64,
        succeeded: 0,
        refused: 0,
        failed: 0,
        skipped: 0,
        cancelled: 0,
        input_tokens: 0,
        output_tokens: 0,
        started_at: Utc::now().to_rfc3339(),
        finished_at: None,
    });
    emit(
        &app,
        &runtime.storage,
        TaskEvent::state(&batch_id, "started", None),
        scope,
    );
    let mut joins = JoinSet::new();
    let mut queued = request.image_paths.iter();
    let mut cancelled_all = false;
    loop {
        if cancel.is_cancelled() {
            cancelled_all = true;
            // Immediately emit explicit cancelled events for everything
            // that never started.
            for path in queued.by_ref() {
                let job = initial_job(path, "cancelled");
                run_info.lock().finished += 1;
                emit(
                    &app,
                    &runtime.storage,
                    TaskEvent::job(&batch_id, job.clone()),
                    scope,
                );
                record_task_item(&runtime, &batch_id, &job, 0, None);
            }
            break;
        }
        while joins.len() < snapshot.total_concurrency {
            match queued.next() {
                Some(path) => {
                    run_info.lock().running += 1;
                    let app = app.clone();
                    let runtime = runtime.clone();
                    let batch = batch_id.clone();
                    let req = request.clone();
                    let conn_name = connection_name.clone();
                    let keys = slots.clone();
                    let advanced_keys = advanced_slots.clone();
                    let token = cancel.clone();
                    let path = path.clone();
                    joins.spawn(async move {
                        process_one(
                            app,
                            runtime,
                            batch,
                            req,
                            conn_name,
                            keys,
                            advanced_keys,
                            token,
                            path,
                        )
                        .await;
                    });
                }
                None => break,
            }
        }
        if joins.is_empty() {
            break;
        }
        if joins.join_next().await.is_some() {
            let mut info = run_info.lock();
            info.running = info.running.saturating_sub(1);
        }
    }
    // Drain running tasks (they emit their own final events).
    while joins.join_next().await.is_some() {
        let mut info = run_info.lock();
        info.running = info.running.saturating_sub(1);
    }
    runtime.cancellations.lock().remove(&batch_id);
    runtime.runs.lock().remove(&batch_id);
    let _ = runtime.storage.finish_task_run(&batch_id);
    run_info.lock().status = if cancelled_all {
        "cancelled"
    } else {
        "completed"
    }
    .into();
    emit(
        &app,
        &runtime.storage,
        TaskEvent::state(
            &batch_id,
            "completed",
            Some(if cancelled_all {
                "批次已停止".into()
            } else {
                "批次处理完成".into()
            }),
        ),
        scope,
    );
    emit_key_runtime(&app, &runtime, &batch_id);
}

pub fn start_batch(
    app: AppHandle,
    runtime: Arc<AppRuntime>,
    request: BatchRequest,
) -> Result<String, String> {
    if request.image_paths.is_empty() {
        return Err("任务中没有图片".into());
    }
    let snapshot = request.selection_snapshot.clone();
    if snapshot.model_id.trim().is_empty() {
        return Err("未选择模型".into());
    }
    // Freeze the connections once: the task never re-reads mutable config.
    let is_local = snapshot.local_model_id.is_some();
    if request.kind == JobKind::AiSample && is_local {
        return Err("AI 抽样需要选择支持视觉输入的 API 模型".into());
    }
    // 渠道解析：channels（渠道级连接+模型）> connection_ids > 主连接（兼容旧预设）。
    let mut channels: Vec<ApiChannelSelection> = snapshot.channels.clone();
    if channels.is_empty() {
        if snapshot.connection_ids.is_empty() {
            channels.push(ApiChannelSelection {
                connection_id: snapshot.connection_id.clone(),
                model_id: snapshot.model_id.clone(),
            });
        } else {
            channels.extend(
                snapshot
                    .connection_ids
                    .iter()
                    .map(|cid| ApiChannelSelection {
                        connection_id: cid.clone(),
                        model_id: snapshot.model_id.clone(),
                    }),
            );
        }
    }
    let channels: Vec<ApiChannelSelection> = channels
        .into_iter()
        .filter(|channel| !channel.connection_id.trim().is_empty())
        .collect();
    let connections: Vec<Arc<ApiConnection>> = if is_local {
        Vec::new()
    } else {
        let mut list = Vec::new();
        for channel in &channels {
            match runtime.storage.get_connection_full(&channel.connection_id) {
                Ok(connection) => list.push(Arc::new(connection)),
                Err(error) => {
                    return Err(format!("渠道 {} 不可用：{error}", channel.connection_id))
                }
            }
        }
        list
    };
    let slots = if is_local {
        Arc::new(Vec::new())
    } else {
        let mut all = Vec::new();
        for (channel, connection) in channels.iter().zip(connections.iter()) {
            match build_slots(
                connection.clone(),
                &channel.model_id,
                snapshot.per_key_concurrency,
                &runtime.storage,
            ) {
                Ok(slots) => all.extend(slots),
                Err(error) => {
                    // 单个渠道无可用 Key：跳过该渠道，其余渠道继续。
                    eprintln!("[tasks] 渠道 {} 跳过：{error}", connection.name);
                }
            }
        }
        if all.is_empty() {
            return Err("所有叠加渠道都没有可用 API Key".into());
        }
        Arc::new(all)
    };
    let advanced_slots = if let Some(advanced) = snapshot
        .advanced_model
        .as_ref()
        .filter(|value| value.enabled)
    {
        if advanced.connection_id.trim().is_empty() || advanced.model_id.trim().is_empty() {
            return Err("高级模型已启用，但 API 连接或模型为空".into());
        }
        let connection = Arc::new(
            runtime
                .storage
                .get_connection_full(&advanced.connection_id)
                .map_err(|error| format!("高级模型连接不可用：{error}"))?,
        );
        Arc::new(
            build_slots(
                connection,
                &advanced.model_id,
                snapshot.per_key_concurrency,
                &runtime.storage,
            )
            .map_err(|error| format!("高级模型没有可用 Key：{error}"))?,
        )
    } else {
        Arc::new(Vec::new())
    };
    let connection = connections.first().cloned();
    let batch_id = Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    runtime
        .cancellations
        .lock()
        .insert(batch_id.clone(), cancel.clone());
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: batch_id.clone(),
        status: "running".into(),
        total: request.image_paths.len() as u32,
        running: 0,
        finished: 0,
        slots: slots.iter().chain(advanced_slots.iter()).cloned().collect(),
    }));
    runtime
        .runs
        .lock()
        .insert(batch_id.clone(), run_info.clone());
    let connection_name = if connections.len() > 1 {
        format!(
            "{} 等 {} 个渠道",
            connections
                .first()
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            connections.len()
        )
    } else {
        connection
            .as_ref()
            .map(|connection| connection.name.clone())
            .unwrap_or_default()
    };
    let id_for_task = batch_id.clone();
    tauri::async_runtime::spawn(dispatch_batch(
        app,
        runtime,
        request,
        connection_name,
        slots,
        advanced_slots,
        cancel,
        run_info,
        id_for_task,
    ));
    Ok(batch_id)
}

pub fn cancel_batch(runtime: &AppRuntime, batch_id: &str) {
    if let Some(token) = runtime.cancellations.lock().get(batch_id) {
        token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot(label: &str, active: usize) -> Arc<KeySlot> {
        Arc::new(KeySlot {
            id: label.into(),
            label: label.into(),
            masked: "••••x".into(),
            secret: "x".into(),
            model_id: "mock-model".into(),
            connection: Arc::new(ApiConnection {
                id: "conn-t".into(),
                name: "测试连接".into(),
                provider: crate::models::ProviderKind::OpenaiCompatible,
                base_url: "https://t.example/v1".into(),
                headers: Default::default(),
                key_refs: Vec::new(),
                total_concurrency: 1,
                per_key_concurrency: 1,
                timeout_seconds: 30,
                cached_models: None,
                models_cached_at: None,
                last_model: None,
                default_params: None,
            }),
            connection_name: "测试连接".into(),
            active: AtomicUsize::new(active),
            succeeded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            tokens: AtomicU64::new(0),
            disabled: AtomicBool::new(false),
            disable_reason: Mutex::new(None),
            cooldown_until: Mutex::new(None),
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }
    #[test]
    fn maps_error_kinds_to_classification_labels() {
        let cases = [
            (ApiErrorKind::RateLimit, "限流文案", "rate_limit"),
            (ApiErrorKind::Auth, "401", "auth"),
            (ApiErrorKind::Quota, "quota", "quota"),
            (
                ApiErrorKind::PromptRejected,
                "The prompt could not be submitted",
                "prompt_rejected",
            ),
            (ApiErrorKind::Transient, "API 响应被截断", "truncated"),
            (ApiErrorKind::Transient, "connection reset", "transient"),
            (ApiErrorKind::Fatal, "boom", "fatal"),
        ];
        for (kind, message, expected) in cases {
            let error = ApiError {
                kind,
                message: message.to_string(),
                retry_after: None,
                status: None,
            };
            assert_eq!(error_kind_name(&error), expected, "message: {message}");
        }
    }

    #[tokio::test]
    async fn selects_least_active_key() {
        let busy = slot("busy", 3);
        let idle = slot("idle", 0);
        let lease = acquire_key(&[busy, idle], &CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(lease.slot.label, "idle");
    }
    #[tokio::test]
    async fn migrates_away_from_preferred_key() {
        let first = slot("first", 0);
        let second = slot("second", 0);
        let lease = acquire_key(&[first, second], &CancellationToken::new(), Some("first"))
            .await
            .unwrap();
        assert_eq!(lease.slot.label, "second");
        // With no alternative, the avoided key is still used.
        let only = slot("only", 0);
        let lease = acquire_key(
            std::slice::from_ref(&only),
            &CancellationToken::new(),
            Some("only"),
        )
        .await
        .unwrap();
        assert_eq!(lease.slot.label, "only");
    }
    #[test]
    fn disables_auth_failed_key() {
        let key = slot("key", 0);
        apply_key_error(
            &key,
            &ApiError {
                kind: ApiErrorKind::Auth,
                message: "401".into(),
                retry_after: None,
                status: Some(401),
            },
            0,
        );
        assert!(key.disabled.load(Ordering::SeqCst));
        assert!(key.disable_reason.lock().is_some());
    }
    #[test]
    fn cooldown_uses_retry_after_and_fallback() {
        let key = slot("key", 0);
        apply_key_error(
            &key,
            &ApiError {
                kind: ApiErrorKind::RateLimit,
                message: "429".into(),
                retry_after: Some(Duration::from_secs(42)),
                status: Some(429),
            },
            0,
        );
        let remaining = key
            .cooldown_until
            .lock()
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(remaining >= Duration::from_secs(40));
        let key2 = slot("key2", 0);
        apply_key_error(
            &key2,
            &ApiError {
                kind: ApiErrorKind::RateLimit,
                message: "429".into(),
                retry_after: None,
                status: Some(429),
            },
            1,
        );
        let remaining = key2
            .cooldown_until
            .lock()
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(remaining >= Duration::from_secs(9));
    }

    #[test]
    fn parses_ai_sample_decision_protocol() {
        assert_eq!(
            parse_ai_sample_decision("DECISION: KEEP\nREASON: 构图和清晰度符合要求\nSCORE: 91")
                .unwrap(),
            (true, "构图和清晰度符合要求".into(), Some(91.0))
        );
        assert_eq!(
            parse_ai_sample_decision("**DECISION：REJECT**\n**REASON：主体模糊**").unwrap(),
            (false, "主体模糊".into(), None)
        );
        assert!(parse_ai_sample_decision("看起来不错，可以保留").is_err());
    }
}
