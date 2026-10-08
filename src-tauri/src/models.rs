use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenaiCompatible,
    Anthropic,
    Gemini,
}

/// Editable key form: `secret` is only ever carried from the frontend into
/// `save_connection`; it never reaches the database or logs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyInput {
    pub id: Option<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked: Option<String>,
}

/// Non-secret key reference (id/label/mask only), used in runtime events and
/// persisted task items.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyRef {
    pub id: String,
    pub label: String,
    pub masked: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

/// 单个渠道的（连接, 模型）组合。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiChannelSelection {
    pub connection_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiConnection {
    pub id: String,
    pub name: String,
    pub provider: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub key_refs: Vec<ApiKeyInput>,
    pub total_concurrency: usize,
    pub per_key_concurrency: usize,
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_models: Option<Vec<ModelOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_cached_at: Option<i64>,
    /// 该服务商推荐的请求体额外参数（如思考额度 reasoning_effort、
    /// thinking.budget_tokens、enable_thinking 等），随请求体一并发送。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalRule {
    pub id: String,
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedModelConfig {
    pub enabled: bool,
    pub connection_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_params: Option<serde_json::Value>,
}

/// Everything a task needs, frozen when the batch starts. The scheduler and
/// providers never re-read mutable connection parameters while running.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSelectionSnapshot {
    pub connection_id: String,
    pub connection_name: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_model_id: Option<String>,
    /// 叠加渠道：同一模型可同时使用多个 API 连接（主连接 connection_id 恒在其中）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connection_ids: Vec<String>,
    /// 渠道级选择：每渠道独立连接与模型；为空时回退 connection_ids / connection_id。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ApiChannelSelection>,
    /// 本地打标置信度阈值（sigmoid 后），默认 0.35。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_threshold: Option<f32>,
    /// 最多输出标签数，0 表示不限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_max_tags: Option<u32>,
    /// 是否保留下划线（false 时 "_" 转空格）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_keep_underscores: Option<bool>,
    pub system_prompt: String,
    pub temperature: f64,
    pub max_tokens: u32,
    #[serde(default)]
    pub refusal_rules: Vec<RefusalRule>,
    pub timeout_seconds: u64,
    pub total_concurrency: usize,
    pub per_key_concurrency: usize,
    /// 本任务冻结的请求体覆盖参数，优先级高于连接默认参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_model: Option<AdvancedModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    pub kind: JobKind,
    pub image_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    pub selection_snapshot: ApiSelectionSnapshot,
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Caption,
    Revision,
    AiSample,
}

impl JobKind {
    pub fn scope(self) -> &'static str {
        match self {
            JobKind::Caption => "caption",
            JobKind::Revision => "revision",
            JobKind::AiSample => "ai_sample",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
}

impl UsageMetrics {
    pub fn empty() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalMatch {
    pub rule: String,
    pub excerpt: String,
    pub structured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageJob {
    pub id: String,
    pub image_path: String,
    pub caption_path: String,
    /// 该图是否已复核完成（revision 任务成功），用于复核去重。
    #[serde(default)]
    pub reviewed: bool,
    /// 被明确成功修订的次数（每次复核成功写入文件 +1；仅发请求不算）。
    #[serde(default)]
    pub revision_count: u64,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    pub caption: String,
    pub status: String,
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<RefusalMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<UsageMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// AI 抽样模型给出的 0–100 分，仅用于抽样排序，不写入标签文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionEditRule {
    pub mode: String,
    pub action: String,
    pub position: String,
    pub content: String,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub index: Option<usize>,
    pub regex: bool,
    pub case_sensitive: bool,
    pub all_matches: bool,
    pub deduplicate: bool,
    /// 仅当内容不包含指定文本时才插入（缺失检测，避免重复添加）。
    #[serde(default)]
    pub only_if_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPreview {
    pub path: String,
    pub before: String,
    pub after: String,
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A previewed batch edit. `apply_caption_edit` consumes the id and verifies
/// that no file changed since the preview was generated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionEditBatch {
    pub id: String,
    pub rule: CaptionEditRule,
    pub previews: Vec<EditPreview>,
    pub affected: u64,
    pub skipped: u64,
    pub errors: u64,
    pub created_at: String,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<EditApplyResult>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditBatchRecord {
    pub id: String,
    pub rule: CaptionEditRule,
    pub affected: u64,
    pub skipped: u64,
    pub errors: u64,
    pub applied: bool,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditApplyResult {
    pub path: String,
    /// applied | skipped | conflict | error
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoBatchResult {
    pub batch_id: String,
    pub restored: Vec<String>,
    pub skipped: Vec<String>,
    pub conflicts: Vec<String>,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionVersion {
    pub id: i64,
    pub caption_path: String,
    pub old_content: String,
    pub new_content: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub id: String,
    pub kind: String,
    pub connection_id: String,
    pub connection_name: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    pub status: String,
    pub total: u64,
    pub succeeded: u64,
    pub refused: u64,
    pub failed: u64,
    pub skipped: u64,
    pub cancelled: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskItem {
    pub id: i64,
    pub run_id: String,
    pub image_path: String,
    pub caption_path: String,
    pub status: String,
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
    pub created_at: String,
}

/// Live per-key state surfaced through `key-runtime-event` and
/// `get_task_runtime`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyRuntimeState {
    pub key: ApiKeyRef,
    pub active: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_remaining_ms: Option<u64>,
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRuntimeState {
    pub batch_id: String,
    pub status: String,
    pub total: u32,
    pub running: u32,
    pub finished: u32,
    pub keys: Vec<ApiKeyRuntimeState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapData {
    pub connections: Vec<ApiConnection>,
    pub tag_presets: Vec<serde_json::Value>,
    pub revision_presets: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalStats {
    pub total: u64,
    pub succeeded: u64,
    pub refused: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsQuery {
    /// kind | batch | connection | model | preset | key
    pub group_by: String,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub batch_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub preset_id: Option<String>,
    #[serde(default)]
    pub key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsBreakdownRow {
    pub group: String,
    pub label: String,
    pub total: u64,
    pub succeeded: u64,
    pub refused: u64,
    pub failed: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub average_tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsBreakdown {
    pub rows: Vec<StatsBreakdownRow>,
    pub summary: HistoricalStats,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTestResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub models_fetched: usize,
    pub sample_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Base URL that actually worked, when it differs from the entered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEvent {
    pub batch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<ImageJob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

impl TaskEvent {
    pub fn log(batch_id: &str, level: &str, message: impl Into<String>) -> Self {
        Self {
            batch_id: batch_id.into(),
            scope: None,
            event_type: "log".into(),
            job: None,
            message: Some(message.into()),
            level: Some(level.into()),
        }
    }

    pub fn job(batch_id: &str, job: ImageJob) -> Self {
        Self {
            batch_id: batch_id.into(),
            scope: None,
            event_type: "progress".into(),
            job: Some(job),
            message: None,
            level: None,
        }
    }

    pub fn state(batch_id: &str, event_type: &str, message: Option<String>) -> Self {
        Self {
            batch_id: batch_id.into(),
            scope: None,
            event_type: event_type.into(),
            job: None,
            message,
            level: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRuntimeEvent {
    pub batch_id: String,
    pub keys: Vec<ApiKeyRuntimeState>,
}

pub fn masked_secret(secret: &str) -> String {
    let suffix: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("••••{}", suffix)
}

/// Header names whose values must never be stored in plaintext or logged.
pub fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "authorization"
        || lower == "proxy-authorization"
        || lower == "x-api-key"
        || lower == "api-key"
        || lower == "apikey"
        || lower == "x-goog-api-key"
        || lower == "x-auth-token"
        || lower.contains("token")
        || lower.contains("secret")
}

pub fn is_masked_value(value: &str) -> bool {
    value.starts_with("••••")
}
