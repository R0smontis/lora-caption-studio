use crate::models::{
    ApiConnection, ConnectionTestResult, JobKind, ModelOption, ProviderKind, RefusalMatch,
    RefusalRule, UsageMetrics,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use regex::{Regex, RegexBuilder};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
    Client, Response, StatusCode,
};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    path::Path,
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiErrorKind {
    RateLimit,
    Auth,
    Quota,
    Transient,
    /// 服务端拒绝提交提示词（"The prompt could not be submitted."）：内容/参数问题，重试无意义。
    PromptRejected,
    Fatal,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub message: String,
    pub retry_after: Option<Duration>,
    /// HTTP status of the failed response, when one was received.
    pub status: Option<u16>,
}

impl ApiError {
    fn fatal(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::Fatal,
            message: message.into(),
            retry_after: None,
            status: None,
        }
    }
}

/// 404/405 mean the URL layout is wrong — a signal to try the next base URL
/// variant instead of failing the request.
fn is_path_error(error: &ApiError) -> bool {
    matches!(error.status, Some(404 | 405))
}

#[derive(Debug)]
pub struct ProviderResponse {
    pub text: String,
    pub usage: UsageMetrics,
    pub structured_refusal: Option<String>,
}

/// 递归合并 JSON 对象：extra 覆盖 base 的同名标量/数组，对象键递归合并。
fn merge_json(base: &mut Value, extra: &Value) {
    match (base, extra) {
        (Value::Object(base_map), Value::Object(extra_map)) => {
            for (key, value) in extra_map {
                match base_map.get_mut(key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, extra) => *base = extra.clone(),
    }
}

/// 返回带任务级请求覆盖参数的连接副本；任务参数优先于连接默认参数。
pub fn with_request_params(connection: &ApiConnection, extra: Option<&Value>) -> ApiConnection {
    let mut connection = connection.clone();
    if let Some(extra) = extra {
        let mut params = connection
            .default_params
            .clone()
            .unwrap_or_else(|| json!({}));
        merge_json(&mut params, extra);
        connection.default_params = Some(params);
    }
    connection
}

fn endpoint(base: &str, suffix: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

fn request_headers(connection: &ApiConnection) -> Result<HeaderMap, ApiError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &connection.headers {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| ApiError::fatal(format!("请求头名称错误：{e}")))?,
            HeaderValue::from_str(value)
                .map_err(|e| ApiError::fatal(format!("请求头内容错误：{e}")))?,
        );
    }
    Ok(headers)
}

fn apply_auth(
    request: reqwest::RequestBuilder,
    provider: &ProviderKind,
    key: &str,
) -> reqwest::RequestBuilder {
    match provider {
        ProviderKind::OpenaiCompatible => request.bearer_auth(key),
        ProviderKind::Anthropic => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        ProviderKind::Gemini => request.header("x-goog-api-key", key),
    }
}

async fn classify_error(response: Response) -> ApiError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = response.text().await.unwrap_or_default();
    let message = format!(
        "HTTP {}：{}",
        status.as_u16(),
        body.chars().take(600).collect::<String>()
    );
    let lower = body.to_ascii_lowercase();
    let kind = if status == StatusCode::TOO_MANY_REQUESTS {
        ApiErrorKind::RateLimit
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ApiErrorKind::Auth
    } else if lower.contains("the prompt could not be submitted") {
        // 常见于图片过大、base64 超长或内容不合规，重试无意义。
        ApiErrorKind::PromptRejected
    } else if lower.contains("quota")
        || lower.contains("insufficient_quota")
        || lower.contains("billing")
    {
        ApiErrorKind::Quota
    } else if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        ApiErrorKind::Transient
    } else {
        ApiErrorKind::Fatal
    };
    ApiError {
        kind,
        message,
        retry_after,
        status: Some(status.as_u16()),
    }
}

/// Candidate base URLs tried in order when the entered URL fails with a
/// path-level error (404/405): the entered form first, then common layouts
/// (`/v1`, `/v1beta`). Deduplicated and trimmed.
pub fn base_url_variants(base: &str, provider: &ProviderKind) -> Vec<String> {
    let trimmed = base.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut variants = Vec::new();
    let mut push = |value: String| {
        if !variants.contains(&value) {
            variants.push(value);
        }
    };
    push(trimmed.to_string());
    match provider {
        ProviderKind::OpenaiCompatible => {
            if (trimmed.contains("volces.com") || trimmed.contains("volcengine"))
                && !trimmed.contains("/api/v3")
            {
                push(format!("{trimmed}/api/v3"));
            }
            if let Some(stripped) = trimmed.strip_suffix("/v1") {
                push(stripped.trim_end_matches('/').to_string());
            } else if !trimmed.ends_with("/api/v3") {
                push(format!("{trimmed}/v1"));
            }
        }
        ProviderKind::Anthropic => {
            if let Some(stripped) = trimmed.strip_suffix("/v1") {
                push(stripped.trim_end_matches('/').to_string());
            }
        }
        ProviderKind::Gemini => {
            if trimmed.contains("/v1beta") {
                push(trimmed.replace("/v1beta", "/v1"));
            } else if trimmed.contains("/v1") {
                push(trimmed.replace("/v1", "/v1beta"));
            } else {
                push(format!("{trimmed}/v1"));
                push(format!("{trimmed}/v1beta"));
            }
        }
    }
    variants
}

fn models_url(connection: &ApiConnection) -> String {
    match connection.provider {
        ProviderKind::OpenaiCompatible => endpoint(&connection.base_url, "models"),
        ProviderKind::Anthropic => {
            if connection.base_url.trim_end_matches('/').ends_with("/v1") {
                endpoint(&connection.base_url, "models?limit=1000")
            } else {
                endpoint(&connection.base_url, "v1/models?limit=1000")
            }
        }
        ProviderKind::Gemini => endpoint(&connection.base_url, "models?pageSize=1000"),
    }
}

async fn list_models_at(
    client: &Client,
    connection: &ApiConnection,
    key: &str,
) -> Result<Vec<ModelOption>, ApiError> {
    let mut all = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..10 {
        let mut url = models_url(connection);
        if let Some(token) = &page_token {
            url.push_str("&pageToken=");
            url.push_str(token);
        }
        let request = client
            .get(url)
            .headers(request_headers(connection)?)
            .timeout(Duration::from_secs(connection.timeout_seconds));
        let response = apply_auth(request, &connection.provider, key)
            .send()
            .await
            .map_err(|e| ApiError {
                kind: if e.is_timeout() {
                    ApiErrorKind::Transient
                } else {
                    ApiErrorKind::Fatal
                },
                message: e.to_string(),
                retry_after: None,
                status: None,
            })?;
        if !response.status().is_success() {
            return Err(classify_error(response).await);
        }
        let value: Value = response
            .json()
            .await
            .map_err(|e| ApiError::fatal(format!("模型列表不是有效 JSON：{e}")))?;
        let array = value
            .get("data")
            .or_else(|| value.get("models"))
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::fatal("响应中没有模型列表"))?;
        let mut models = array
            .iter()
            .filter_map(|item| {
                let raw = item
                    .get("id")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)?;
                let id = raw.strip_prefix("models/").unwrap_or(raw).to_string();
                let supported = item
                    .get("supportedGenerationMethods")
                    .or_else(|| item.get("supported_actions"))
                    .and_then(Value::as_array);
                if matches!(connection.provider, ProviderKind::Gemini)
                    && supported.is_some()
                    && !supported.unwrap().iter().any(|v| {
                        v.as_str()
                            .map(|s| s.eq_ignore_ascii_case("generateContent"))
                            .unwrap_or(false)
                    })
                {
                    return None;
                }
                let name = item
                    .get("display_name")
                    .or_else(|| item.get("displayName"))
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                Some(ModelOption {
                    id,
                    name,
                    supports_vision: None,
                })
            })
            .collect::<Vec<_>>();
        all.append(&mut models);
        page_token = value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(String::from);
        if page_token.is_none() {
            break;
        }
    }
    all.sort_by_key(|model| model.id.to_ascii_lowercase());
    all.dedup_by(|a, b| a.id == b.id);
    Ok(all)
}

/// Fetches the model list, auto-trying common base URL layouts when the
/// entered URL yields 404/405. Returns the models plus the effective base
/// URL so callers can persist the auto-completed form. Gemini paginates
/// through `nextPageToken`; only models supporting `generateContent` are
/// kept.
pub async fn list_models(
    client: &Client,
    connection: &ApiConnection,
    key: &str,
) -> Result<(Vec<ModelOption>, String), ApiError> {
    let mut last_error = None;
    for variant in base_url_variants(&connection.base_url, &connection.provider) {
        let mut probe = connection.clone();
        probe.base_url = variant.clone();
        match list_models_at(client, &probe, key).await {
            Ok(models) => return Ok((models, variant)),
            Err(error) if error.kind == ApiErrorKind::Fatal && is_path_error(&error) => {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| ApiError::fatal("模型列表请求失败")))
}

/// Connection test: fetches the model list with a bounded timeout and reports
/// latency plus a sample of the discovered models. Never throws — the result
/// carries the failure instead.
pub async fn test_connection(
    client: &Client,
    connection: &ApiConnection,
    key: &str,
) -> Result<ConnectionTestResult, String> {
    let started = Instant::now();
    let mut probe = connection.clone();
    probe.timeout_seconds = probe.timeout_seconds.clamp(10, 30);
    let result = match list_models(client, &probe, key).await {
        Ok((models, effective_base)) => ConnectionTestResult {
            ok: true,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            models_fetched: models.len(),
            sample_models: models.iter().take(5).map(|m| m.id.clone()).collect(),
            error: None,
            effective_base_url: (effective_base != probe.base_url).then_some(effective_base),
        },
        Err(error) => ConnectionTestResult {
            ok: false,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            models_fetched: 0,
            sample_models: Vec::new(),
            error: Some(error.message),
            effective_base_url: None,
        },
    };
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn request_body(
    connection: &ApiConnection,
    kind: JobKind,
    model: &str,
    system_prompt: &str,
    current_caption: &str,
    image_b64: &str,
    mime: &str,
    temperature: f64,
    max_tokens: u32,
) -> (String, Value) {
    let user_text = match kind {
        JobKind::Revision => {
            format!("请对照图片复核并直接输出完整替换标签。\n当前标签：\n{current_caption}")
        }
        JobKind::AiSample => {
            "请严格依据系统消息中的用户筛选要求判断这张图片，并按指定的三行协议输出。".to_string()
        }
        JobKind::Caption => "请为这张图片生成训练标签。".to_string(),
    };
    let (url, mut body) = match connection.provider {
        ProviderKind::OpenaiCompatible => (
            endpoint(&connection.base_url, "chat/completions"),
            json!({"model":model,"stream":true,"temperature":temperature,"max_tokens":max_tokens,"messages":[{"role":"system","content":system_prompt},{"role":"user","content":[{"type":"image_url","image_url":{"url":format!("data:{mime};base64,{image_b64}"),"detail":"high"}},{"type":"text","text":user_text}]}]}),
        ),
        ProviderKind::Anthropic => {
            let base = if connection.base_url.trim_end_matches('/').ends_with("/v1") {
                connection.base_url.clone()
            } else {
                endpoint(&connection.base_url, "v1")
            };
            (
                endpoint(&base, "messages"),
                json!({"model":model,"stream":true,"system":system_prompt,"temperature":temperature,"max_tokens":max_tokens,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":mime,"data":image_b64}},{"type":"text","text":user_text}]}]}),
            )
        }
        ProviderKind::Gemini => (
            format!(
                "{}?alt=sse",
                endpoint(
                    &connection.base_url,
                    &format!("models/{model}:streamGenerateContent")
                )
            ),
            json!({"system_instruction":{"parts":[{"text":system_prompt}]},"contents":[{"role":"user","parts":[{"inline_data":{"mime_type":mime,"data":image_b64}},{"text":user_text}]}],"generationConfig":{"temperature":temperature,"maxOutputTokens":max_tokens}}),
        ),
    };
    // 服务商预设携带的推荐额外参数（思考额度等）合并进请求体；
    // 显式参数优先于预设值。
    if let Some(extra) = &connection.default_params {
        merge_json(&mut body, extra);
    }
    (url, body)
}

fn update_usage(
    usage: &mut UsageMetrics,
    input: Option<u64>,
    output: Option<u64>,
    total: Option<u64>,
) {
    if input.is_some() {
        usage.input_tokens = input;
    }
    if output.is_some() {
        usage.output_tokens = output;
    }
    if total.is_some() {
        usage.total_tokens = total;
    }
    if usage.total_tokens.is_none() {
        usage.total_tokens = usage
            .input_tokens
            .zip(usage.output_tokens)
            .map(|(a, b)| a + b);
    }
}
/// 从各类 JSON 结构中递归提取文本片段（支持 string、array、object 等各种格式）。
fn extract_text_from_value(value: Option<&Value>) -> Option<String> {
    let v = value?;
    if let Some(s) = v.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    } else if let Some(arr) = v.as_array() {
        let mut combined = String::new();
        for item in arr {
            if let Some(s) = item.as_str() {
                combined.push_str(s);
            } else if let Some(s) = item.get("text").and_then(Value::as_str) {
                combined.push_str(s);
            } else if let Some(s) = item.get("content").and_then(Value::as_str) {
                combined.push_str(s);
            }
        }
        if !combined.is_empty() {
            return Some(combined);
        }
    } else if let Some(obj) = v.as_object() {
        if let Some(s) = obj.get("text").and_then(Value::as_str) {
            return Some(s.to_string());
        } else if let Some(s) = obj.get("content").and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn parse_event(
    provider: &ProviderKind,
    value: &Value,
    text: &mut String,
    reasoning: &mut String,
    usage: &mut UsageMetrics,
    structured: &mut Option<String>,
) -> Option<String> {
    let mut delta = String::new();
    match provider {
        ProviderKind::OpenaiCompatible => {
            // 推理模型思考内容提取：覆盖字节跳动/火山方舟/豆包/Qwen/Gemma/DeepSeek 等各类字段。
            // delta 增量优先；完整 message/根级结构仅在尚未收到任何思考内容时兜底。
            let mut reasoning_taken = false;
            for path in [
                "/choices/0/delta/reasoning_content",
                "/choices/0/delta/reasoning",
                "/choices/0/delta/thought",
                "/choices/0/delta/thinking",
                "/choices/0/delta/thinking_content",
                "/choices/0/delta/reasoning_text",
            ] {
                if let Some(s) = extract_text_from_value(value.pointer(path)) {
                    reasoning.push_str(&s);
                    reasoning_taken = true;
                    break;
                }
            }
            if !reasoning_taken && reasoning.is_empty() {
                for path in [
                    "/choices/0/message/reasoning_content",
                    "/choices/0/message/reasoning",
                    "/choices/0/message/thought",
                    "/reasoning_content",
                    "/reasoning",
                    "/thought",
                ] {
                    if let Some(s) = extract_text_from_value(value.pointer(path)) {
                        reasoning.push_str(&s);
                        break;
                    }
                }
            }
            // 正文提取：支持 delta.content（字符串/数组/对象）、delta.text、message.content 等。
            // delta 增量优先；完整 message/根级结构（非流式格式或网关完成事件）仅在尚未
            // 收到任何正文时兜底，防止同一内容被重复拼接。
            let mut text_taken = false;
            for path in ["/choices/0/delta/content", "/choices/0/delta/text"] {
                if let Some(s) = extract_text_from_value(value.pointer(path)) {
                    delta.push_str(&s);
                    text_taken = true;
                    break;
                }
            }
            if !text_taken && text.is_empty() && delta.is_empty() {
                for path in [
                    "/choices/0/message/content",
                    "/choices/0/text",
                    "/content",
                    "/text",
                ] {
                    if let Some(s) = extract_text_from_value(value.pointer(path)) {
                        delta.push_str(&s);
                        break;
                    }
                }
            }
            if let Some(value) = value
                .pointer("/choices/0/delta/refusal")
                .and_then(Value::as_str)
            {
                *structured = Some(value.to_string());
                delta.push_str(value);
            }
            if value
                .pointer("/choices/0/finish_reason")
                .and_then(Value::as_str)
                == Some("content_filter")
            {
                *structured = Some("content_filter".into());
            }
            update_usage(
                usage,
                value
                    .pointer("/usage/prompt_tokens")
                    .and_then(Value::as_u64),
                value
                    .pointer("/usage/completion_tokens")
                    .and_then(Value::as_u64),
                value.pointer("/usage/total_tokens").and_then(Value::as_u64),
            );
        }
        ProviderKind::Anthropic => {
            if let Some(value) = value.pointer("/delta/text").and_then(Value::as_str) {
                delta.push_str(value);
            }
            if let Some(value) = value.pointer("/delta/thinking").and_then(Value::as_str) {
                reasoning.push_str(value);
            }
            if value
                .pointer("/delta/type")
                .and_then(Value::as_str)
                .map(|v| v.contains("refusal"))
                .unwrap_or(false)
            {
                *structured = Some("refusal_delta".into());
            }
            if value.pointer("/delta/stop_reason").and_then(Value::as_str) == Some("refusal") {
                *structured = Some("refusal".into());
            }
            update_usage(
                usage,
                value
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64),
                value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64),
                None,
            );
        }
        ProviderKind::Gemini => {
            if let Some(parts) = value
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
            {
                for part in parts {
                    if let Some(value) = part.get("text").and_then(Value::as_str) {
                        delta.push_str(value);
                    }
                }
            }
            if let Some(reason) = value
                .pointer("/candidates/0/finishReason")
                .and_then(Value::as_str)
            {
                if ["SAFETY", "BLOCKLIST", "PROHIBITED_CONTENT", "SPII"].contains(&reason) {
                    *structured = Some(reason.into());
                }
            }
            update_usage(
                usage,
                value
                    .pointer("/usageMetadata/promptTokenCount")
                    .and_then(Value::as_u64),
                value
                    .pointer("/usageMetadata/candidatesTokenCount")
                    .and_then(Value::as_u64),
                value
                    .pointer("/usageMetadata/totalTokenCount")
                    .and_then(Value::as_u64),
            );
        }
    }
    if delta.is_empty() {
        None
    } else {
        text.push_str(&delta);
        Some(delta)
    }
}

fn completion_limit_reason(provider: &ProviderKind, value: &Value) -> Option<String> {
    let reason = match provider {
        ProviderKind::OpenaiCompatible => value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .filter(|reason| matches!(*reason, "length" | "max_tokens" | "max_output_tokens")),
        ProviderKind::Anthropic => value
            .pointer("/delta/stop_reason")
            .or_else(|| value.pointer("/stop_reason"))
            .and_then(Value::as_str)
            .filter(|reason| matches!(*reason, "max_tokens" | "model_context_window_exceeded")),
        ProviderKind::Gemini => value
            .pointer("/candidates/0/finishReason")
            .and_then(Value::as_str)
            .filter(|reason| matches!(*reason, "MAX_TOKENS" | "MODEL_LENGTH")),
    }?;
    Some(reason.to_string())
}

/// Feeds one logical data line (after `data:` stripping) into the JSON
/// fragment accumulator. Handles SSE multi-line events, NDJSON lines and
/// Gemini JSON arrays split across lines or network chunks.
fn feed_line(
    provider: &ProviderKind,
    fragment: &mut String,
    data: &str,
    text: &mut String,
    reasoning: &mut String,
    usage: &mut UsageMetrics,
    structured: &mut Option<String>,
) -> Option<String> {
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    // A fresh fragment must start a JSON value; continuation lines
    // (`,{...}` inside a split array) are only accepted while a fragment
    // is open.
    if fragment.is_empty() && !data.starts_with('{') && !data.starts_with('[') {
        return None;
    }
    if !fragment.is_empty() {
        fragment.push('\n');
    }
    fragment.push_str(data);
    let mut limit_reason = None;
    match serde_json::from_str::<Value>(fragment) {
        Ok(value) => {
            fragment.clear();
            let values: Vec<&Value> = value
                .as_array()
                .map(|v| v.iter().collect())
                .unwrap_or_else(|| vec![&value]);
            for event in values {
                if limit_reason.is_none() {
                    limit_reason = completion_limit_reason(provider, event);
                }
                let _ = parse_event(provider, event, text, reasoning, usage, structured);
            }
        }
        Err(_) => {
            // Incomplete multi-line JSON: keep accumulating. Guard against
            // unbounded growth from garbage input.
            if fragment.len() > 64 * 1024 {
                fragment.clear();
            }
        }
    }
    limit_reason
}

#[allow(clippy::too_many_arguments)]
/// 发送给 API 的图片长边上限（2048 = 2K 像素量级；1080p=1920 亦低于此值）。
pub const MAX_IMAGE_SIDE: u32 = 2048;

#[allow(clippy::too_many_arguments)]
pub async fn stream_caption(
    client: &Client,
    connection: &ApiConnection,
    key: &str,
    kind: JobKind,
    model: &str,
    system_prompt: &str,
    current_caption: &str,
    image_path: &str,
    temperature: f64,
    max_tokens: u32,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<ProviderResponse, ApiError> {
    let raw = tokio::fs::read(image_path)
        .await
        .map_err(|e| ApiError::fatal(format!("读取图片失败：{e}")))?;
    let guessed = mime_guess::from_path(Path::new(image_path))
        .first_or_octet_stream()
        .to_string();
    // 发送前图片尺寸控制：长边超过 2048（2K 像素量级）时等比缩小并转 JPEG，
    // 避免超高清图导致的请求过大/超时/提示词被拒。
    let dims = image::image_dimensions(Path::new(image_path)).ok();
    let oversized = dims
        .map(|(w, h)| w.max(h) > MAX_IMAGE_SIDE)
        .unwrap_or(false);
    let (image, mime) = if oversized {
        let image = image::load_from_memory(&raw)
            .map_err(|e| ApiError::fatal(format!("转换图片失败：{e}")))?;
        let (w, h) = (image.width(), image.height());
        let (nw, nh) = if w >= h {
            (
                MAX_IMAGE_SIDE,
                (h as u64 * MAX_IMAGE_SIDE as u64 / w as u64) as u32,
            )
        } else {
            (
                (w as u64 * MAX_IMAGE_SIDE as u64 / h as u64) as u32,
                MAX_IMAGE_SIDE,
            )
        };
        let resized = image.resize(nw.max(1), nh.max(1), image::imageops::FilterType::Lanczos3);
        let mut output = Cursor::new(Vec::new());
        resized
            .write_to(&mut output, image::ImageFormat::Jpeg)
            .map_err(|e| ApiError::fatal(format!("图片压缩失败：{e}")))?;
        (output.into_inner(), "image/jpeg".to_string())
    } else if guessed == "image/bmp" || guessed == "image/tiff" {
        let image = image::load_from_memory(&raw)
            .map_err(|e| ApiError::fatal(format!("转换图片失败：{e}")))?;
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, image::ImageFormat::Jpeg)
            .map_err(|e| ApiError::fatal(format!("转换图片失败：{e}")))?;
        (output.into_inner(), "image/jpeg".to_string())
    } else {
        (raw, guessed)
    };
    let image_b64 = STANDARD.encode(image);
    // Auto-try common base URL layouts: a 404/405 on the entered URL falls
    // through to the next variant (e.g. appending /v1 or /v1beta).
    let mut variants = base_url_variants(&connection.base_url, &connection.provider).into_iter();
    let mut current_base = variants
        .next()
        .unwrap_or_else(|| connection.base_url.clone());
    let started = Instant::now();
    let response = loop {
        let mut probe = connection.clone();
        probe.base_url = current_base.clone();
        let (url, body) = request_body(
            &probe,
            kind,
            model,
            system_prompt,
            current_caption,
            &image_b64,
            &mime,
            temperature,
            max_tokens,
        );
        let request = client
            .post(url)
            .headers(request_headers(&probe)?)
            .timeout(Duration::from_secs(connection.timeout_seconds))
            .json(&body);
        let sent = tokio::select! {
            _ = cancel.cancelled() => return Err(ApiError { kind: ApiErrorKind::Cancelled, message: "任务已取消".into(), retry_after: None, status: None }),
            response = apply_auth(request, &connection.provider, key).send() => response.map_err(|e| ApiError { kind: if e.is_timeout() || e.is_connect() { ApiErrorKind::Transient } else { ApiErrorKind::Fatal }, message: e.to_string(), retry_after: None, status: None })?,
        };
        if sent.status().is_success() {
            break sent;
        }
        let error = classify_error(sent).await;
        if is_path_error(&error) {
            if let Some(next) = variants.next() {
                current_base = next;
                continue;
            }
        }
        return Err(error);
    };
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut fragment = String::new();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut usage = UsageMetrics {
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        ttft_ms: None,
        generation_ms: None,
        tokens_per_second: None,
    };
    let mut structured = None;
    let mut completion_limit = None;
    let mut first_token = None;
    // 流完成信号：OpenAI 兼容为 data: [DONE]；Anthropic 为 event: message_stop。
    // Gemini 流无完成标记（以 usageMetadata 结尾），不参与截断检测。
    let mut saw_done = false;
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(ApiError { kind: ApiErrorKind::Cancelled, message: "任务已取消".into(), retry_after: None, status: None }),
            next = stream.next() => next,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|e| ApiError {
            kind: ApiErrorKind::Transient,
            message: e.to_string(),
            retry_after: None,
            status: None,
        })?;
        buffer.extend_from_slice(&chunk);
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line_bytes: Vec<u8> = buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len().saturating_sub(1)])
                .trim()
                .trim_end_matches('\r')
                .to_string();
            if line.starts_with("event: message_stop") {
                saw_done = true;
                continue;
            }
            if line.is_empty()
                || line == "[DONE]"
                || line.starts_with(':')
                || line.starts_with("event:")
                || line.starts_with("retry:")
            {
                continue;
            }
            let data = line.strip_prefix("data:").map(str::trim).unwrap_or(&line);
            if data == "[DONE]" {
                saw_done = true;
                continue;
            }
            // 服务端在流中返回错误事件（HTTP 200 + {"error":{...}} 或 {"code":...,"message":...}，火山引擎/部分网关常见）：
            // 立即终止并把错误消息透出，避免被当作空响应。
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                let error_msg = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| value.pointer("/error_msg").and_then(Value::as_str))
                    .or_else(|| value.pointer("/err_msg").and_then(Value::as_str))
                    .or_else(|| {
                        if value.get("choices").is_none() && value.get("code").is_some() {
                            value.get("message").and_then(Value::as_str)
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        if value.get("choices").is_none() {
                            value.get("error").and_then(Value::as_str)
                        } else {
                            None
                        }
                    });
                if let Some(message) = error_msg {
                    if !message.trim().is_empty() {
                        return Err(ApiError {
                            kind: ApiErrorKind::Fatal,
                            message: format!("服务端错误：{message}"),
                            retry_after: None,
                            status: None,
                        });
                    }
                }
            }
            let before = text.len();
            if let Some(reason) = feed_line(
                &connection.provider,
                &mut fragment,
                data,
                &mut text,
                &mut reasoning,
                &mut usage,
                &mut structured,
            ) {
                completion_limit = Some(reason);
            }
            if text.len() > before && first_token.is_none() {
                first_token = Some(Instant::now());
                usage.ttft_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    // Final unterminated line.
    let trailing = String::from_utf8_lossy(&buffer).to_string();
    if !trailing.trim().is_empty() {
        let data = trailing
            .trim()
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or(trailing.trim());
        if data == "[DONE]" || data.starts_with("event: message_stop") {
            saw_done = true;
        }
        if let Some(reason) = feed_line(
            &connection.provider,
            &mut fragment,
            data,
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut structured,
        ) {
            completion_limit = Some(reason);
        }
    }
    // 推理模型：正文为空但只输出了思考内容（LM Studio/qwen/gemma/豆包思考，常见于
    // max_tokens 被思考耗尽或正文包含在未闭合思考标签内）——思考过程不得写入结果，报明确错误。
    let cleaned_text = strip_think_tags(&text);
    if cleaned_text.trim().is_empty()
        && (!reasoning.trim().is_empty() || text.contains("<think>") || text.contains("<thought>"))
    {
        return Err(ApiError {
            kind: ApiErrorKind::Fatal,
            message:
                "模型仅输出了思考内容、未给出最终答案（可能是最大输出 Token 被思考耗尽），请增大“最大输出 Token”或关闭思考模式"
                    .to_string(),
            retry_after: None,
            status: None,
        });
    }
    if let Some(first) = first_token {
        let seconds = first.elapsed().as_secs_f64();
        usage.generation_ms = Some(seconds * 1000.0);
        if seconds > 0.0 {
            usage.tokens_per_second = usage.output_tokens.map(|tokens| tokens as f64 / seconds);
        }
    }
    if let Some(reason) = completion_limit {
        return Err(ApiError {
            kind: ApiErrorKind::Fatal,
            message: format!("API 输出达到最大输出 Token 上限（{reason}），部分内容未写入文件"),
            retry_after: None,
            status: None,
        });
    }
    // 流已结束但未收到完成标记：响应被截断（连接中断/服务端异常），不返回部分内容。
    if !saw_done && !text.is_empty() && connection.provider != ProviderKind::Gemini {
        return Err(ApiError {
            kind: ApiErrorKind::Transient,
            message: "API 响应被截断：流在收到完成标记前结束，可能连接中断或服务端异常，未写入文件"
                .to_string(),
            retry_after: None,
            status: None,
        });
    }
    Ok(ProviderResponse {
        text: clean_caption(&text),
        usage,
        structured_refusal: structured,
    })
}

/// 标签分隔规范化：逗号分隔的标签统一为 "tag1, tag2, ..."（逗号 + 单个空格）。
/// 幂等：已规范的输出保持不变；"1girl,solo"、"1girl,  solo" 等自动修正。
/// 无逗号的普通文本原样返回。
pub fn normalize_tag_separators(value: &str) -> String {
    if !value.contains(',') {
        return value.to_string();
    }
    let parts: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return value.to_string();
    }
    parts.join(", ")
}

/// 剥离思考模型输出中的 `<think>...</think>` 与 `<thought>...</thought>` 块，
/// 包含未闭合思考标签（当输出被截断时）。
pub fn strip_think_tags(input: &str) -> String {
    let mut text = input.to_string();
    for (open, close) in [
        ("<think>", "</think>"),
        ("<thought>", "</thought>"),
        ("<Thought>", "</Thought>"),
        ("<THINK>", "</THINK>"),
    ] {
        while let Some(start) = text.find(open) {
            if let Some(end_offset) = text[start..].find(close) {
                let end = start + end_offset + close.len();
                text.replace_range(start..end, "");
            } else {
                text.truncate(start);
                break;
            }
        }
    }
    text.trim().to_string()
}

pub fn clean_caption(value: &str) -> String {
    let stripped = strip_think_tags(value);
    let trimmed = stripped.trim();
    let unwrapped = if trimmed.starts_with("```") && trimmed.ends_with("```") {
        trimmed
            .trim_start_matches("```")
            .trim_start_matches("text")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };
    normalize_tag_separators(unwrapped.trim_matches('"').trim())
}

pub fn detect_refusal(
    text: &str,
    custom: &[RefusalRule],
    structured: Option<&str>,
) -> Option<RefusalMatch> {
    if let Some(reason) = structured {
        return Some(RefusalMatch {
            rule: reason.into(),
            excerpt: reason.into(),
            structured: true,
        });
    }
    let normalized: String = text
        .nfkc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let high = [
        r"(?i)\bi (?:can(?:not|'t)|am unable to) (?:help|assist|comply|provide|fulfill)",
        r"(?i)\bi must (?:decline|refuse)",
        r"(?:无法|不能|不便)(?:为你|向你|对此)?(?:协助|帮助|完成|提供|执行|生成|回答)",
        r"(?:我必须|需要)(?:拒绝|婉拒)",
    ];
    for pattern in high {
        if let Ok(regex) = Regex::new(pattern) {
            if let Some(found) = regex.find(&normalized) {
                return Some(RefusalMatch {
                    rule: pattern.into(),
                    excerpt: normalized
                        .chars()
                        .skip(found.start().saturating_sub(24))
                        .take(120)
                        .collect(),
                    structured: false,
                });
            }
        }
    }
    let prefix: String = normalized.chars().take(180).collect();
    let apology = Regex::new(r"(?i)(?:抱歉|对不起|很遗憾|i(?:'m| am) sorry|apologies)").unwrap();
    let inability =
        Regex::new(r"(?i)(?:无法|不能|不便|拒绝|不可以|can't|cannot|unable|decline|refuse)")
            .unwrap();
    if apology.is_match(&prefix) && inability.is_match(&prefix) {
        return Some(RefusalMatch {
            rule: "apology+inability".into(),
            excerpt: prefix,
            structured: false,
        });
    }
    for rule in custom {
        let escaped;
        let expression = if rule.regex {
            rule.pattern.as_str()
        } else {
            escaped = regex::escape(&rule.pattern);
            &escaped
        };
        let regex = RegexBuilder::new(expression)
            .case_insensitive(!rule.case_sensitive)
            .build();
        if let Ok(regex) = regex {
            if let Some(found) = regex.find(&normalized) {
                return Some(RefusalMatch {
                    rule: rule.pattern.clone(),
                    excerpt: normalized
                        .chars()
                        .skip(found.start().saturating_sub(24))
                        .take(120)
                        .collect(),
                    structured: false,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_request_params_override_connection_thinking_defaults() {
        let connection = ApiConnection {
            id: "c".into(),
            name: "C".into(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: "https://example.test/v1".into(),
            headers: std::collections::HashMap::new(),
            key_refs: vec![],
            total_concurrency: 2,
            per_key_concurrency: 1,
            timeout_seconds: 90,
            last_model: None,
            cached_models: None,
            models_cached_at: None,
            default_params: Some(
                json!({"reasoning_effort":"medium","thinking":{"budget_tokens":2048}}),
            ),
        };
        let effective = with_request_params(
            &connection,
            Some(&json!({"reasoning_effort":"high","thinking":{"budget_tokens":8192}})),
        );
        assert_eq!(
            effective.default_params.unwrap(),
            json!({"reasoning_effort":"high","thinking":{"budget_tokens":8192}})
        );
    }
    #[test]
    fn normalizes_tag_separators() {
        // 已规范：不变
        assert_eq!(
            normalize_tag_separators("1girl, solo, smile"),
            "1girl, solo, smile"
        );
        // 无空格
        assert_eq!(
            normalize_tag_separators("1girl,solo,smile"),
            "1girl, solo, smile"
        );
        // 多余空格
        assert_eq!(
            normalize_tag_separators("1girl,  solo , smile"),
            "1girl, solo, smile"
        );
        // 空段清理
        assert_eq!(normalize_tag_separators("1girl,,solo,"), "1girl, solo");
        // 无逗号的普通文本原样返回
        assert_eq!(
            normalize_tag_separators("这是一段中文描述"),
            "这是一段中文描述"
        );
        // clean_caption 全链路
        assert_eq!(clean_caption("1girl,solo"), "1girl, solo");
        assert_eq!(clean_caption("  \"1girl,solo\"  "), "1girl, solo");
    }

    #[test]
    fn generates_base_url_variants_per_provider() {
        let openai = base_url_variants("https://api.example.com", &ProviderKind::OpenaiCompatible);
        assert_eq!(
            openai,
            vec![
                "https://api.example.com".to_string(),
                "https://api.example.com/v1".to_string()
            ]
        );
        // Entered with /v1: the root layout stays as a fallback.
        let already_v1 = base_url_variants(
            "https://api.example.com/v1/",
            &ProviderKind::OpenaiCompatible,
        );
        assert_eq!(
            already_v1,
            vec![
                "https://api.example.com/v1".to_string(),
                "https://api.example.com".to_string()
            ]
        );
        let gemini = base_url_variants(
            "https://generativelanguage.googleapis.com",
            &ProviderKind::Gemini,
        );
        assert_eq!(
            gemini,
            vec![
                "https://generativelanguage.googleapis.com".to_string(),
                "https://generativelanguage.googleapis.com/v1".to_string(),
                "https://generativelanguage.googleapis.com/v1beta".to_string()
            ]
        );
        let beta = base_url_variants(
            "https://generativelanguage.googleapis.com/v1beta",
            &ProviderKind::Gemini,
        );
        assert!(beta.contains(&"https://generativelanguage.googleapis.com/v1".to_string()));
        assert!(base_url_variants("", &ProviderKind::OpenaiCompatible).is_empty());
        // No duplicates ever.
        let many = base_url_variants("https://h/v1", &ProviderKind::OpenaiCompatible);
        assert_eq!(
            many.len(),
            many.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

    #[test]
    fn detects_english_and_chinese_refusals() {
        assert!(detect_refusal("I'm sorry, I can't assist with that.", &[], None).is_some());
        assert!(detect_refusal("抱歉，我无法协助完成这个请求。", &[], None).is_some());
        assert!(detect_refusal("1girl, solo, sunset", &[], None).is_none());
    }
    #[test]
    fn recognizes_provider_token_limit_finish_reasons() {
        assert_eq!(
            completion_limit_reason(
                &ProviderKind::OpenaiCompatible,
                &json!({"choices":[{"finish_reason":"length"}]})
            )
            .as_deref(),
            Some("length")
        );
        assert_eq!(
            completion_limit_reason(
                &ProviderKind::Anthropic,
                &json!({"delta":{"stop_reason":"max_tokens"}})
            )
            .as_deref(),
            Some("max_tokens")
        );
        assert_eq!(
            completion_limit_reason(
                &ProviderKind::Gemini,
                &json!({"candidates":[{"finishReason":"MAX_TOKENS"}]})
            )
            .as_deref(),
            Some("MAX_TOKENS")
        );
    }
    #[test]
    fn unwraps_markdown_fence() {
        assert_eq!(clean_caption("```text\n1girl, solo\n```"), "1girl, solo");
    }
    #[test]
    fn parses_openai_stream_delta_and_usage() {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut usage = UsageMetrics {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        };
        let mut refusal = None;
        let value = json!({"choices":[{"delta":{"content":"1girl, solo"},"finish_reason":null}],"usage":{"prompt_tokens":40,"completion_tokens":3,"total_tokens":43}});
        assert_eq!(
            parse_event(
                &ProviderKind::OpenaiCompatible,
                &value,
                &mut text,
                &mut reasoning,
                &mut usage,
                &mut refusal
            )
            .as_deref(),
            Some("1girl, solo")
        );
        assert_eq!(usage.total_tokens, Some(43));
    }
    #[test]
    fn parses_anthropic_stream_delta() {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut usage = UsageMetrics {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        };
        let mut refusal = None;
        let _ = parse_event(
            &ProviderKind::Anthropic,
            &json!({"delta":{"type":"text_delta","text":"blue eyes"},"usage":{"output_tokens":2}}),
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(text, "blue eyes");
        assert_eq!(usage.output_tokens, Some(2));
    }
    #[test]
    fn parses_gemini_stream_delta_and_safety() {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut usage = UsageMetrics {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        };
        let mut refusal = None;
        let _ = parse_event(
            &ProviderKind::Gemini,
            &json!({"candidates":[{"content":{"parts":[{"text":"outdoors"}]},"finishReason":"SAFETY"}],"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":1,"totalTokenCount":21}}),
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(text, "outdoors");
        assert_eq!(refusal.as_deref(), Some("SAFETY"));
        assert_eq!(usage.total_tokens, Some(21));
    }
    #[test]
    fn accumulates_multiline_and_chunked_json() {
        let provider = ProviderKind::Gemini;
        let mut fragment = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut usage = UsageMetrics {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        };
        let mut refusal = None;
        // Pretty-printed Gemini SSE event split across lines.
        feed_line(
            &provider,
            &mut fragment,
            "{",
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert!(text.is_empty());
        feed_line(
            &provider,
            &mut fragment,
            "\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"part A\"}]}}]",
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert!(text.is_empty());
        feed_line(
            &provider,
            &mut fragment,
            "}",
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(text, "part A");
        assert!(fragment.is_empty());
        // Gemini JSON array split across chunks with continuation lines.
        feed_line(
            &provider,
            &mut fragment,
            "[{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"1\"}]}}]",
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        feed_line(
            &provider,
            &mut fragment,
            "},{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"2\"}]}}]}]",
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(text, "part A12");
        assert!(fragment.is_empty());
    }

    #[test]
    fn parses_bytedance_and_volcengine_structures() {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut usage = UsageMetrics {
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            ttft_ms: None,
            generation_ms: None,
            tokens_per_second: None,
        };
        let mut refusal = None;

        // 1. 数组结构 content: [{"type": "text", "text": "1girl, "}, {"text": "solo"}]
        let array_val = json!({
            "choices": [{
                "delta": {
                    "content": [
                        {"type": "text", "text": "1girl, "},
                        {"text": "solo"}
                    ]
                }
            }]
        });
        parse_event(
            &ProviderKind::OpenaiCompatible,
            &array_val,
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(text, "1girl, solo");

        // 2. 带有 thought 字段的字节跳动思考模型
        let thought_val = json!({
            "choices": [{
                "delta": {
                    "thought": "analysing image...",
                    "content": "smile"
                }
            }]
        });
        parse_event(
            &ProviderKind::OpenaiCompatible,
            &thought_val,
            &mut text,
            &mut reasoning,
            &mut usage,
            &mut refusal,
        );
        assert_eq!(reasoning, "analysing image...");
        assert_eq!(text, "1girl, solosmile");
    }

    #[test]
    fn strips_think_and_thought_tags() {
        // 闭合思考标签
        assert_eq!(
            clean_caption("<think>思考很多很多...</think>1girl, solo"),
            "1girl, solo"
        );
        assert_eq!(
            clean_caption("<thought>Thinking process\nline 2</thought>1girl, solo"),
            "1girl, solo"
        );
        assert_eq!(
            clean_caption("```text\n<think>abc</think>1girl, solo\n```"),
            "1girl, solo"
        );
        // 未闭合思考标签（被截断）
        assert_eq!(strip_think_tags("<think>正在思考还没结束"), "");
    }

    #[test]
    fn autocompletes_volcengine_api_v3_url() {
        let urls = base_url_variants(
            "https://ark.cn-beijing.volces.com",
            &ProviderKind::OpenaiCompatible,
        );
        assert!(urls.contains(&"https://ark.cn-beijing.volces.com/api/v3".to_string()));
    }
}
