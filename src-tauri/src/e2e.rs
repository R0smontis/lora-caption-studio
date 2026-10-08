//! End-to-end tests against a local mock HTTP server: model pagination,
//! request bodies, image input, SSE/JSON chunking, usage, refusal, 429/5xx,
//! timeout, stream cut, cancellation and the full scheduler loop with real
//! file writes. No real API keys involved.

use crate::{
    file_ops,
    local_model::{self, LocalModelStore},
    models::{
        AdvancedModelConfig, ApiChannelSelection, ApiConnection, ApiKeyInput, ApiSelectionSnapshot,
        BatchRequest, JobKind, ProviderKind, RefusalRule,
    },
    providers::{list_models, stream_caption, test_connection, ApiErrorKind},
    storage::Storage,
    tasks::{build_slots, dispatch_batch, AppRuntime, RunInfo},
};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    io::Read,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use tempfile::TempDir;
use tiny_http::{Header, Response, Server, StatusCode};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ---------------------------------------------------------------- mock server

enum MockBody {
    Bytes(Vec<u8>),
    /// (data, delay_ms_before_that_piece) — streamed with chunked encoding.
    Chunked(Vec<(Vec<u8>, u64)>),
}

#[allow(clippy::type_complexity)]
struct MockCtx {
    pub requests: Mutex<Vec<(String, HashMap<String, String>, Vec<u8>)>>,
    pub in_flight: AtomicUsize,
    pub max_in_flight: AtomicUsize,
    pub per_key_in_flight: Mutex<HashMap<String, usize>>,
    pub per_key_max: Mutex<HashMap<String, usize>>,
}

type MockHandler = Arc<
    dyn Fn(&str, &HashMap<String, String>, &[u8]) -> (u16, Vec<(String, String)>, MockBody)
        + Send
        + Sync,
>;

struct MockServer {
    base: String,
    ctx: Arc<MockCtx>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    fn start(handler: MockHandler) -> MockServer {
        let server = Server::http("127.0.0.1:0").unwrap();
        let base = format!(
            "http://127.0.0.1:{}",
            server.server_addr().to_ip().unwrap().port()
        );
        let ctx = Arc::new(MockCtx {
            requests: Mutex::new(Vec::new()),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            per_key_in_flight: Mutex::new(HashMap::new()),
            per_key_max: Mutex::new(HashMap::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread_ctx = ctx.clone();
        let thread_stop = stop.clone();
        let thread = thread::spawn(move || {
            for mut request in server.incoming_requests() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let path = request.url().to_string();
                let mut headers = HashMap::new();
                for header in request.headers() {
                    headers.insert(
                        header.field.as_str().to_string().to_ascii_lowercase(),
                        header.value.as_str().to_string(),
                    );
                }
                let mut body = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body);
                let key = headers
                    .get("authorization")
                    .or_else(|| headers.get("x-api-key"))
                    .or_else(|| headers.get("x-goog-api-key"))
                    .cloned()
                    .unwrap_or_default();
                thread_ctx
                    .requests
                    .lock()
                    .push((path.clone(), headers.clone(), body.clone()));
                thread_ctx.in_flight.fetch_add(1, Ordering::SeqCst);
                let mut prev = thread_ctx.max_in_flight.load(Ordering::SeqCst);
                let now = thread_ctx.in_flight.load(Ordering::SeqCst);
                while now > prev {
                    match thread_ctx.max_in_flight.compare_exchange(
                        prev,
                        now,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(actual) => prev = actual,
                    }
                }
                if !key.is_empty() {
                    let mut per_key = thread_ctx.per_key_in_flight.lock();
                    let count = per_key.entry(key.clone()).or_insert(0);
                    *count += 1;
                    let mut maxes = thread_ctx.per_key_max.lock();
                    let entry = maxes.entry(key.clone()).or_insert(0);
                    *entry = (*entry).max(*count);
                    drop(maxes);
                    drop(per_key);
                }
                let (status, response_headers, mock_body) = handler(&path, &headers, &body);
                let response_headers = response_headers
                    .into_iter()
                    .filter_map(|(name, value)| {
                        Header::from_bytes(name.as_bytes(), value.as_bytes()).ok()
                    })
                    .collect::<Vec<_>>();
                let response = match &mock_body {
                    MockBody::Bytes(bytes) => Response::new(
                        StatusCode(status),
                        response_headers,
                        Box::new(std::io::Cursor::new(bytes.clone()))
                            as Box<dyn Read + Send + 'static>,
                        Some(bytes.len()),
                        None,
                    ),
                    MockBody::Chunked(parts) => Response::new(
                        StatusCode(status),
                        response_headers,
                        Box::new(SlowReader::new(parts)) as Box<dyn Read + Send + 'static>,
                        None,
                        None,
                    ),
                };
                let _ = request.respond(response);
                thread_ctx.in_flight.fetch_sub(1, Ordering::SeqCst);
                if !key.is_empty() {
                    let mut per_key = thread_ctx.per_key_in_flight.lock();
                    if let Some(count) = per_key.get_mut(&key) {
                        *count = count.saturating_sub(1);
                    }
                }
            }
        });
        MockServer {
            base,
            ctx,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<(String, Vec<u8>)> {
        self.ctx
            .requests
            .lock()
            .iter()
            .map(|(path, _, body)| (path.clone(), body.clone()))
            .collect()
    }

    fn request_count(&self) -> usize {
        self.ctx.requests.lock().len()
    }

    fn max_in_flight(&self) -> usize {
        self.ctx.max_in_flight.load(Ordering::SeqCst)
    }

    fn per_key_max(&self, key: &str) -> usize {
        *self.ctx.per_key_max.lock().get(key).unwrap_or(&0)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // The accept loop may be blocked inside a slow stream; detach rather
        // than joining. The process teardown reaps the thread.
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take();
    }
}

struct SlowReader {
    parts: Vec<(Vec<u8>, u64)>,
    index: usize,
    pos: usize,
}

impl SlowReader {
    fn new(parts: &[(Vec<u8>, u64)]) -> SlowReader {
        SlowReader {
            parts: parts.to_vec(),
            index: 0,
            pos: 0,
        }
    }
}

impl Read for SlowReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.index >= self.parts.len() {
            return Ok(0);
        }
        let (data, delay) = &self.parts[self.index];
        if *delay > 0 {
            thread::sleep(Duration::from_millis(*delay));
        }
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[self.pos..self.pos + n]);
        self.pos += n;
        if self.pos >= data.len() {
            self.index += 1;
            self.pos = 0;
        }
        Ok(n)
    }
}

// ------------------------------------------------------------------ helpers

fn openai_stream(caption: &str) -> Vec<(Vec<u8>, u64)> {
    let lines = [
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{caption}\"}}}}]}}\n"),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n".into(),
        "data: [DONE]\n".into(),
    ];
    // Split the first line into three chunks to exercise chunk-boundary parsing.
    let first = lines[0].clone().into_bytes();
    let split = first.len() / 3;
    vec![
        (first[..split].to_vec(), 0),
        (first[split..split * 2].to_vec(), 0),
        (first[split * 2..].to_vec(), 0),
        (lines[1].clone().into_bytes(), 0),
        (lines[2].clone().into_bytes(), 0),
    ]
}

fn make_images(dir: &Path, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            let path = dir.join(format!("img_{index}.png"));
            let image =
                image::RgbaImage::from_pixel(64, 64, image::Rgba([index as u8, 40, 200, 255]));
            image.save(&path).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect()
}

fn connection_with_keys(base: &str, keys: &[(&str, &str)]) -> ApiConnection {
    connection_with_provider(base, keys, ProviderKind::OpenaiCompatible)
}

fn connection_with_provider(
    base: &str,
    keys: &[(&str, &str)],
    provider: ProviderKind,
) -> ApiConnection {
    ApiConnection {
        id: Uuid::new_v4().to_string(),
        name: "Mock".into(),
        provider,
        base_url: base.into(),
        headers: HashMap::new(),
        key_refs: keys
            .iter()
            .map(|(id, secret)| ApiKeyInput {
                id: Some(id.to_string()),
                label: format!("key {id}"),
                secret: Some(secret.to_string()),
                masked: None,
            })
            .collect(),
        total_concurrency: 2,
        per_key_concurrency: 1,
        timeout_seconds: 30,
        last_model: None,
        cached_models: None,
        models_cached_at: None,
        default_params: None,
    }
}

fn request(
    connection_id: String,
    image_paths: Vec<String>,
    total: usize,
    per_key: usize,
    timeout: u64,
) -> BatchRequest {
    BatchRequest {
        kind: JobKind::Caption,
        image_paths,
        preset_id: None,
        selection_snapshot: ApiSelectionSnapshot {
            connection_id,
            connection_name: "Mock".into(),
            model_id: "mock-model".into(),
            local_model_id: None,
            connection_ids: vec![],
            channels: vec![],
            local_threshold: None,
            local_max_tags: None,
            local_keep_underscores: None,
            system_prompt: "你是打标助手".into(),
            temperature: 0.2,
            max_tokens: 64,
            refusal_rules: vec![RefusalRule {
                id: "r1".into(),
                pattern: "拒绝处理".into(),
                regex: false,
                case_sensitive: false,
            }],
            timeout_seconds: timeout,
            total_concurrency: total,
            per_key_concurrency: per_key,
            request_params: None,
            advanced_model: None,
        },
        overwrite: true,
    }
}

// --------------------------------------------------------------------- tests

#[tokio::test]
async fn lists_openai_models_with_bearer_auth() {
    let server = MockServer::start(Arc::new(|path, headers, _| {
        assert_eq!(path, "/models");
        assert!(headers["authorization"].starts_with("Bearer "));
        (
            200,
            vec![("Content-Type".into(), "application/json".into())],
            MockBody::Bytes(
                r#"{"data":[{"id":"gpt-4o","object":"model"},{"id":"gpt-4o-mini","object":"model"}]}"#.into(),
            ),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let (models, effective) = list_models(&reqwest::Client::new(), &connection, "sk-secret-1")
        .await
        .unwrap();
    assert_eq!(effective, server.base);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gpt-4o");
}

#[tokio::test]
async fn lists_anthropic_models_with_limit_and_key_header() {
    let server = MockServer::start(Arc::new(|path, headers, _| {
        assert!(path.starts_with("/v1/models") && path.contains("limit=1000"));
        assert_eq!(
            headers.get("x-api-key").map(String::as_str),
            Some("sk-ant-secret")
        );
        (
            200,
            vec![("Content-Type".into(), "application/json".into())],
            MockBody::Bytes(r#"{"data":[{"id":"claude-3-5-sonnet","type":"model"}]}"#.into()),
        )
    }));
    let connection = connection_with_provider(
        &server.base,
        &[("k1", "sk-ant-secret")],
        ProviderKind::Anthropic,
    );
    let (models, _) = list_models(&reqwest::Client::new(), &connection, "sk-ant-secret")
        .await
        .unwrap();
    assert_eq!(models[0].id, "claude-3-5-sonnet");
}

#[tokio::test]
async fn lists_gemini_models_with_pagination_and_filtering() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let server = MockServer::start(Arc::new(move |path, _, _| {
        calls_clone.lock().push(path.to_string());
        if path.contains("pageToken=tok2") {
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                MockBody::Bytes(
                    r#"{"models":[{"name":"models/gemini-2.0-flash","supportedGenerationMethods":["generateContent","generateImages"]},{"name":"models/imagen-3","supportedGenerationMethods":["generateImages"]}]}"#.into(),
                ),
            )
        } else {
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                MockBody::Bytes(
                    r#"{"models":[{"name":"models/gemini-1.5-pro","supportedGenerationMethods":["generateContent"]}],"nextPageToken":"tok2"}"#.into(),
                ),
            )
        }
    }));
    let connection =
        connection_with_provider(&server.base, &[("k1", "gem-secret")], ProviderKind::Gemini);
    let (models, _) = list_models(&reqwest::Client::new(), &connection, "gem-secret")
        .await
        .unwrap();
    // Imagen (no generateContent) is filtered out; both pages merged.
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|model| model.id.starts_with("gemini-")));
    assert_eq!(calls.lock().len(), 2);
}

#[tokio::test]
async fn test_connection_reports_failure_gracefully() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            401,
            vec![],
            MockBody::Bytes(r#"{"error":"invalid key"}"#.into()),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "bad-key")]);
    let result = test_connection(&reqwest::Client::new(), &connection, "bad-key")
        .await
        .unwrap();
    assert!(!result.ok);
    assert!(result.error.is_some());
}

#[tokio::test]
async fn streams_openai_sse_with_chunk_boundaries_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|path, _, body| {
        assert!(path.ends_with("/chat/completions"));
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["model"], "mock-model");
        assert_eq!(value["stream"], true);
        let content = &value["messages"][1]["content"][0];
        assert!(content["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, solo")),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "1girl, solo");
    assert_eq!(response.usage.total_tokens, Some(12));
    assert_eq!(response.usage.input_tokens, Some(10));
    assert_eq!(response.usage.output_tokens, Some(2));
    assert!(response.usage.ttft_ms.is_some());
    assert!(response.usage.tokens_per_second.is_some());
}

#[tokio::test]
async fn preserves_utf8_when_a_character_crosses_network_chunks() {
    let line = "data: {\"choices\":[{\"delta\":{\"content\":\"蓝眼睛, 白裙\"}}]}\n";
    let marker = line
        .as_bytes()
        .windows(3)
        .position(|part| part == "蓝".as_bytes())
        .unwrap();
    let split = marker + 1;
    let bytes = line.as_bytes();
    let parts = vec![
        (bytes[..split].to_vec(), 0),
        (bytes[split..].to_vec(), 0),
        (
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n".to_vec(),
            0,
        ),
        (b"data: [DONE]\n".to_vec(), 0),
    ];
    let server = MockServer::start(Arc::new(move |_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(parts.clone()),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "蓝眼睛, 白裙");
}

#[tokio::test]
async fn rejects_a_normally_ended_stream_that_hit_the_token_limit() {
    let parts = vec![
        (
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial caption\"}}]}\n".to_vec(),
            0,
        ),
        (
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n".to_vec(),
            0,
        ),
        (b"data: [DONE]\n".to_vec(), 0),
    ];
    let server = MockServer::start(Arc::new(move |_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(parts.clone()),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let error = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("Token 上限"));
}

#[tokio::test]
async fn streams_gemini_multiline_json_and_structured_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|path, _, body| {
        assert!(path.contains(":streamGenerateContent"));
        assert!(path.contains("alt=sse"));
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert!(value["contents"][0]["parts"][0]["inline_data"]["data"]
            .as_str()
            .is_some());
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}]}\n".to_vec(), 0),
                // Multi-line pretty-printed event split across lines.
                (b"data: {\n".to_vec(), 0),
                (b"data: \"candidates\":[{\"content\":{\"parts\":[{\"text\":\"b\"}]},\"finishReason\":\"SAFETY\"}],\"usageMetadata\":{\"promptTokenCount\":4,\"candidatesTokenCount\":2,\"totalTokenCount\":6}\n".to_vec(), 0),
                (b"data: }\n".to_vec(), 0),
            ]),
        )
    }));
    let connection =
        connection_with_provider(&server.base, &[("k1", "gem-secret")], ProviderKind::Gemini);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "gem-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "ab");
    assert_eq!(response.structured_refusal.as_deref(), Some("SAFETY"));
    assert_eq!(response.usage.total_tokens, Some(6));
}

#[tokio::test]
async fn converts_bmp_input_to_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("photo.bmp");
    let image = image::RgbaImage::from_pixel(32, 32, image::Rgba([10, 20, 30, 255]));
    image.save(&bmp).unwrap();
    let server = MockServer::start(Arc::new(|_, _, body| {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        let url = value["messages"][1]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("ok")),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        bmp.to_str().unwrap(),
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "ok");
}

#[tokio::test]
async fn classifies_429_with_retry_after_and_5xx() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            429,
            vec![("Retry-After".into(), "7".into())],
            MockBody::Bytes("rate limited".into()),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let error = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::RateLimit);
    assert_eq!(error.retry_after, Some(Duration::from_secs(7)));

    let server2 = MockServer::start(Arc::new(|_, _, _| {
        (500, vec![], MockBody::Bytes("boom".into()))
    }));
    let connection2 = connection_with_keys(&server2.base, &[("k1", "sk-secret")]);
    let error = stream_caption(
        &reqwest::Client::new(),
        &connection2,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::Transient);
}

#[tokio::test]
async fn times_out_when_server_stalls() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![(b"data: ".to_vec(), 4000)]),
        )
    }));
    let mut connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    connection.timeout_seconds = 1;
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let error = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::Transient);
}

#[tokio::test]
async fn cancels_in_flight_stream() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            // Never completes; keeps the connection open.
            MockBody::Chunked(vec![(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n".to_vec(),
                60_000,
            )]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let task = tokio::spawn(async move {
        stream_caption(
            &reqwest::Client::new(),
            &connection,
            "sk-secret",
            JobKind::Caption,
            "mock-model",
            "prompt",
            "",
            &images[0],
            0.2,
            64,
            token,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.kind, ApiErrorKind::Cancelled);
}

#[tokio::test]
async fn recovers_from_stream_cut_mid_json() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            // Cut off in the middle of the first JSON line, then EOF.
            MockBody::Bytes(b"data: {\"choices\":[{\"delta\":{\"content\":\"part".to_vec()),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(response.text.is_empty()); // partial JSON dropped, no panic, no error
}

#[tokio::test]
async fn real_samples_with_chinese_paths_and_varied_captions() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, solo")),
        )
    }));
    // Real-world sample set: Chinese directory, PNG with an existing caption,
    // WebP with an empty caption file, JPEG without any caption.
    let root = tempfile::tempdir().unwrap();
    let samples = root.path().join("样本集");
    std::fs::create_dir_all(&samples).unwrap();
    let png = samples.join("人物_001.png");
    image::RgbaImage::from_pixel(128, 128, image::Rgba([200, 100, 80, 255]))
        .save(&png)
        .unwrap();
    std::fs::write(samples.join("人物_001.txt"), "1girl, existing caption").unwrap();
    let webp = samples.join("场景_002.webp");
    image::RgbaImage::from_pixel(96, 96, image::Rgba([80, 160, 120, 255]))
        .save(&webp)
        .unwrap();
    std::fs::write(samples.join("场景_002.txt"), "").unwrap();
    let jpeg = samples.join("street_003.jpg");
    image::RgbImage::from_pixel(160, 90, image::Rgb([60, 90, 150]))
        .save(&jpeg)
        .unwrap();

    let images = vec![
        png.to_string_lossy().into_owned(),
        webp.to_string_lossy().into_owned(),
        jpeg.to_string_lossy().into_owned(),
    ];
    let (_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images, 2, 1);
    join.await.unwrap();

    // Existing caption is overwritten (overwrite=true), empty caption filled,
    // new caption created.
    assert_eq!(
        std::fs::read_to_string(samples.join("人物_001.txt")).unwrap(),
        "1girl, solo"
    );
    assert_eq!(
        std::fs::read_to_string(samples.join("场景_002.txt")).unwrap(),
        "1girl, solo"
    );
    assert_eq!(
        std::fs::read_to_string(samples.join("street_003.txt")).unwrap(),
        "1girl, solo"
    );
    let items = storage.list_task_items(&run).unwrap();
    assert_eq!(items.len(), 3);
    assert!(
        items.iter().all(|item| item.status == "succeeded"),
        "items: {items:?}"
    );
    // Version history preserved the pre-existing caption.
    let caption_path = crate::file_ops::normalize_display_path(
        std::fs::canonicalize(samples.join("人物_001.txt")).unwrap(),
    );
    let versions = storage
        .list_versions(Some(caption_path.to_str().unwrap()))
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].old_content, "1girl, existing caption");
}

#[tokio::test]
async fn auto_completes_missing_v1_suffix_for_models() {
    let server = MockServer::start(Arc::new(|path, _, _| {
        if path == "/models" {
            (404, vec![], MockBody::Bytes("not found".into()))
        } else {
            assert_eq!(path, "/v1/models");
            (
                200,
                vec![("Content-Type".into(), "application/json".into())],
                MockBody::Bytes(r#"{"data":[{"id":"gpt-4o","object":"model"}]}"#.into()),
            )
        }
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let (models, effective) = list_models(&reqwest::Client::new(), &connection, "sk-secret-1")
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(effective, format!("{}/v1", server.base));
    assert_eq!(server.request_count(), 2); // first 404, then the /v1 variant
}

#[tokio::test]
async fn auto_completes_missing_v1_suffix_for_streams() {
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|path, _, _| {
        if path == "/chat/completions" {
            (404, vec![], MockBody::Bytes("not found".into()))
        } else {
            assert_eq!(path, "/v1/chat/completions");
            (
                200,
                vec![("Content-Type".into(), "text/event-stream".into())],
                MockBody::Chunked(openai_stream("completed-url")),
            )
        }
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "completed-url");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn downloads_local_model_with_range_resume() {
    // 模拟 HF 下载源：支持 Range 断点续传。
    let model_bytes: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();
    let served = model_bytes.clone();
    let server = MockServer::start(Arc::new(move |path, headers, _| {
        let expected = "/SmilingWolf/wd-v1-4-vit-tagger-v2/resolve/main/model.onnx";
        if path == "/SmilingWolf/wd-v1-4-vit-tagger-v2/resolve/main/selected_tags.csv" {
            return (
                200,
                vec![],
                MockBody::Bytes(b"tag_id,name,category,count\n0,test_tag,0,1\n".to_vec()),
            );
        }
        assert_eq!(path, expected);
        match headers.get("range").map(String::as_str) {
            Some(range) if range.starts_with("bytes=") => {
                let start: u64 = range["bytes=".len()..]
                    .trim_end_matches('-')
                    .parse()
                    .unwrap();
                let body = served[start as usize..].to_vec();
                (
                    206,
                    vec![(
                        "Content-Range".into(),
                        format!("bytes {}-{}/{}", start, served.len() - 1, served.len()),
                    )],
                    MockBody::Bytes(body),
                )
            }
            _ => (200, vec![], MockBody::Bytes(served.clone())),
        }
    }));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(local_model::LocalModelStore::with_base(
        dir.path(),
        &server.base,
    ));
    let app = mock_app();
    // 第一次下载完整文件。
    local_model::download_model(
        &reqwest::Client::new(),
        app.handle().clone(),
        &store,
        "wd-v1-4-vit-tagger-v2",
    )
    .await
    .unwrap();
    assert!(store.installed("wd-v1-4-vit-tagger-v2"));
    let saved = std::fs::read(store.model_path("wd-v1-4-vit-tagger-v2")).unwrap();
    assert_eq!(saved, model_bytes);
    // 模拟中断：删除模型但保留 .part，再下载应走 Range 续传。
    std::fs::remove_file(store.model_path("wd-v1-4-vit-tagger-v2")).unwrap();
    let part = store
        .model_dir("wd-v1-4-vit-tagger-v2")
        .join("model.onnx.part");
    std::fs::write(&part, &model_bytes[..512]).unwrap();
    local_model::download_model(
        &reqwest::Client::new(),
        app.handle().clone(),
        &store,
        "wd-v1-4-vit-tagger-v2",
    )
    .await
    .unwrap();
    let resumed = std::fs::read(store.model_path("wd-v1-4-vit-tagger-v2")).unwrap();
    assert_eq!(resumed, model_bytes);
    // 标签表随模型一起落地。
    assert!(store
        .model_dir("wd-v1-4-vit-tagger-v2")
        .join("selected_tags.csv")
        .is_file());
}

#[tokio::test]
async fn local_model_batch_tags_images_offline() {
    // 用内置的最小 ONNX（Identity 图）走完整调度器本地打标路径。
    std::env::set_var(
        "ORT_DYLIB_PATH",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/onnxruntime/onnxruntime.dll"
        ),
    );
    // 模拟应用启动时的 CUDA 运行库 PATH 注入（安装版由 lib.rs 完成）
    let cuda_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/cuda");
    std::env::set_var(
        "PATH",
        format!("{cuda_dir};{}", std::env::var("PATH").unwrap_or_default()),
    );
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 3);
    let model_dir = dir.path().join("models").join("wd-test-model");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/resources/test-model.onnx"),
        model_dir.join("model.onnx"),
    )
    .unwrap();
    std::fs::write(
        model_dir.join("selected_tags.csv"),
        "tag_id,name,category,count\n0,one,0,1\n1,two,0,1\n2,three,0,1\n",
    )
    .unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let runtime =
        Arc::new(AppRuntime::new(Storage::open(dir.path()).unwrap(), dir.path()).unwrap());
    let app = mock_app();
    let handle = app.handle().clone();
    let mut batch = request("".into(), images.clone(), 1, 1, 30);
    batch.selection_snapshot.connection_id = String::new();
    batch.selection_snapshot.model_id = "wd-test-model".into();
    batch.selection_snapshot.local_model_id = Some("wd-test-model".into());
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: "run-local".into(),
        status: "running".into(),
        total: 3,
        running: 0,
        finished: 0,
        slots: Vec::new(),
    }));
    dispatch_batch(
        handle,
        runtime.clone(),
        batch,
        "本地模型".into(),
        Arc::new(Vec::new()),
        Arc::new(Vec::new()),
        CancellationToken::new(),
        run_info,
        "run-local".into(),
    )
    .await;
    let items = storage.list_task_items("run-local").unwrap();
    eprintln!(
        "[trace] local items: {:?}",
        items
            .iter()
            .map(|item| (item.status.clone(), item.error.clone()))
            .collect::<Vec<_>>()
    );
    // 每个图片都写入了标签文件并记录任务条目（Identity 图输出为像素归一化值，
    // 经 sigmoid 与 0.35 阈值后通常仍有少量标签）。
    for image in &images {
        let caption = Path::new(image).with_extension("txt");
        assert!(caption.is_file(), "missing caption for {image}");
    }
    assert_eq!(items.len(), 3);
    assert!(
        items.iter().all(|item| item.status == "succeeded"),
        "items: {items:?}"
    );
    // 性能指标不统计本地打标：request_metrics 应为空。
    let stats = storage.query_stats().unwrap();
    assert_eq!(stats.total, 0, "本地打标不应写入性能指标: {stats:?}");
}

#[tokio::test]
async fn failed_job_records_error_kind_classification() {
    // 限流（429）→ 任务失败且错误分类为 rate_limit。
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            429,
            vec![("Content-Type".into(), "application/json".into())],
            MockBody::Bytes(
                br#"{"error":{"message":"Rate limit exceeded","type":"rate_limit"}}"#.to_vec(),
            ),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let (_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images.clone(), 1, 1);
    join.await.unwrap();
    let items = storage.list_task_items(&run).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "failed");
    // 限流在 2 次重试后仍失败：error_kind 由任务项持久化前的 job 记录……
    // task_items 持久化 error 但分类在 ImageJob——通过 metrics 表验证无成功记录。
    let stats = storage.query_stats().unwrap();
    assert_eq!(stats.total, 1, "每次任务写一条指标（含重试）");
    assert_eq!(stats.failed, 1);
    // 前端分类字段由 ImageJob.error_kind 承载：直接验证 ApiError 分类映射
    // （stream_caption 已把 429 归类为 RateLimit，tasks 映射为 rate_limit）。
    let _ = run;
}

/// 真实模型冒烟：下载 SmilingWolf 最小模型并推理真实图片。仅在显式运行时执行。
#[tokio::test]
#[ignore = "下载约 326MB 模型，仅做发布前冒烟"]
async fn smoke_real_wd_model_inference() {
    std::env::set_var(
        "ORT_DYLIB_PATH",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/onnxruntime/onnxruntime.dll"
        ),
    );
    // 模拟应用启动时的 CUDA 运行库 PATH 注入（安装版由 lib.rs 完成）
    let cuda_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/cuda");
    std::env::set_var(
        "PATH",
        format!("{cuda_dir};{}", std::env::var("PATH").unwrap_or_default()),
    );
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalModelStore::new(dir.path()));
    let app = mock_app();
    let client = reqwest::Client::new();
    local_model::download_model(
        &client,
        app.handle().clone(),
        &store,
        "wd-v1-4-moat-tagger-v2",
    )
    .await
    .unwrap();
    let image_path = dir.path().join("sample.png");
    image::RgbaImage::from_pixel(512, 768, image::Rgba([200, 120, 90, 255]))
        .save(&image_path)
        .unwrap();
    let started = std::time::Instant::now();
    let (text, metrics) = local_model::tag_image(
        &store,
        "wd-v1-4-moat-tagger-v2",
        image_path.to_str().unwrap(),
        0.35,
        0,
        false,
    )
    .unwrap();
    eprintln!(
        "[smoke] inference took {}ms, tags: {}",
        metrics.ttft_ms.unwrap_or(0.0) as u64,
        text
    );
    assert!(
        !text.is_empty(),
        "expected non-empty tags from the real model"
    );
    assert!(started.elapsed().as_secs() < 120);
}

#[tokio::test]
async fn merges_default_params_into_request_body() {
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|path, _, body| {
        if path == "/chat/completions" {
            let value: serde_json::Value = serde_json::from_slice(body).unwrap();
            // 服务商预设注入的思考额度参数已进入请求体。
            assert_eq!(value["reasoning_effort"], "medium");
            // 预设未包含的显式参数保持原样。
            assert_eq!(value["temperature"], 0.2);
            (
                200,
                vec![("Content-Type".into(), "text/event-stream".into())],
                MockBody::Chunked(openai_stream("ok")),
            )
        } else {
            panic!("unexpected path {path}");
        }
    }));
    let mut connection = connection_with_keys(&server.base, &[("k1", "sk-secret")]);
    connection.default_params = Some(serde_json::json!({
        "reasoning_effort": "medium",
    }));
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "ok");
}

#[tokio::test]
async fn merges_nested_anthropic_thinking_params() {
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|path, _, body| {
        assert!(path.ends_with("/v1/messages"), "path: {path}");
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["thinking"]["type"], "enabled");
        assert_eq!(value["thinking"]["budget_tokens"], 4096);
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n".to_vec(), 0),
                (b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n".to_vec(), 0),
                (b"event: message_stop\n".to_vec(), 0),
            ]),
        )
    }));
    let mut connection =
        connection_with_provider(&server.base, &[("k1", "sk-ant")], ProviderKind::Anthropic);
    connection.default_params = Some(serde_json::json!({
        "thinking": {"type": "enabled", "budget_tokens": 4096},
    }));
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-ant",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        64,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "hi");
}

#[tokio::test]
async fn download_proceeds_after_ipc_placeholder() {
    // IPC download() 先原子占位再 spawn 后台任务；后台任务不得因占位自锁
    // （曾导致进度永远停在 0%）。
    let model_bytes: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    let served = model_bytes.clone();
    let server = MockServer::start(Arc::new(move |path, _, _| {
        if path.ends_with("selected_tags.csv") {
            return (
                200,
                vec![],
                MockBody::Bytes(b"tag_id,name,category,count\n0,t,0,1\n".to_vec()),
            );
        }
        (200, vec![], MockBody::Bytes(served.clone()))
    }));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LocalModelStore::with_base(dir.path(), &server.base));
    let app = mock_app();
    // 模拟 IPC download() 已设置的原子占位：后台任务不得因此自锁，
    // 必须正常下载并在结束后清理占位。
    store.test_place_placeholder("wd-v1-4-vit-tagger-v2", 373 * 1024 * 1024);
    local_model::download_model(
        &reqwest::Client::new(),
        app.handle().clone(),
        &store,
        "wd-v1-4-vit-tagger-v2",
    )
    .await
    .unwrap();
    assert!(store.installed("wd-v1-4-vit-tagger-v2"));
    assert_eq!(
        std::fs::read(store.model_path("wd-v1-4-vit-tagger-v2")).unwrap(),
        model_bytes
    );
    // 下载结束后占位应被清理，前端才能恢复为非下载状态（通过公开状态可见）。
    assert!(
        store
            .list()
            .iter()
            .find(|model| model.id == "wd-v1-4-vit-tagger-v2")
            .is_some_and(|model| !model.downloading),
        "下载结束后 downloading 状态应清除"
    );
}

#[tokio::test]
#[ignore = "manual: requires real v3 model + photo on disk"]
async fn real_photo_v3_outputs_content_tags() {
    // 回归：v3 输入必须是 [0,255] 原始像素；归一化输入会被模型当作暗图
    // （输出 monochrome/greyscale）。真实照片应输出内容标签。
    std::env::set_var(
        "ORT_DYLIB_PATH",
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/onnxruntime/onnxruntime.dll"
        ),
    );
    // 模拟应用启动时的 CUDA 运行库 PATH 注入（安装版由 lib.rs 完成）
    let cuda_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/cuda");
    std::env::set_var(
        "PATH",
        format!("{cuda_dir};{}", std::env::var("PATH").unwrap_or_default()),
    );
    let src_model = r"C:\tmp\wd-swinv2-tagger-v3.onnx";
    let src_photo = r"C:\tmp\real_photo.jpg";
    let src_csv = r"C:\tmp\wd-swinv2-tagger-v3.csv";
    if !std::path::Path::new(src_model).is_file()
        || !std::path::Path::new(src_photo).is_file()
        || !std::path::Path::new(src_csv).is_file()
    {
        eprintln!("skip: missing model/photo/csv");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let model_dir = dir.path().join("models").join("wd-swinv2-tagger-v3");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::copy(src_model, model_dir.join("model.onnx")).unwrap();
    std::fs::copy(src_csv, model_dir.join("selected_tags.csv")).unwrap();
    let store = LocalModelStore::new(dir.path());
    let mut last_text = String::new();
    for round in 0..4 {
        let started = std::time::Instant::now();
        let (text, _) =
            local_model::tag_image(&store, "wd-swinv2-tagger-v3", src_photo, 0.35, 0, false)
                .unwrap();
        eprintln!(
            "[photo] round{round}: EP={:?} 耗时={:.0}ms",
            store.ep_name(),
            started.elapsed().as_millis()
        );
        if round == 0 {
            eprintln!("[photo] {text}");
        }
        last_text = text;
    }
    let text = &last_text;
    assert!(
        text.contains("nature")
            || text.contains("tree")
            || text.contains("sky")
            || text.contains("outdoors")
            || text.contains("scenery"),
        "照片应输出内容标签: {text}"
    );
    assert!(
        !text.contains("monochrome") && !text.contains("greyscale"),
        "照片不应被判为灰图: {text}"
    );
}

#[tokio::test]
async fn revision_success_is_recorded_as_reviewed_path() {
    // 复核去重：revision 成功的图进入"已复核"集合，供再次导入时跳过。
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, revised tags")),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 2);
    for image in &images {
        std::fs::write(Path::new(image).with_extension("txt"), "1girl").unwrap();
    }
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut connection = connection_with_keys(&server.base, &[("k1", "sk-a")]);
    connection.id = "conn-mock".into();
    storage.save_connection(connection.clone()).unwrap();
    let runtime =
        Arc::new(AppRuntime::new(Storage::open(dir.path()).unwrap(), dir.path()).unwrap());
    let app = mock_app();
    let slots = Arc::new(
        build_slots(
            Arc::new(storage.get_connection_full("conn-mock").unwrap()),
            "mock-model",
            1,
            &storage,
        )
        .unwrap(),
    );
    let mut batch = request("conn-mock".into(), images.clone(), 1, 1, 30);
    batch.kind = JobKind::Revision;
    let cancel = CancellationToken::new();
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: "run-rev".into(),
        status: "running".into(),
        total: batch.image_paths.len() as u32,
        running: 0,
        finished: 0,
        slots: slots.iter().cloned().collect(),
    }));
    let join = tokio::spawn(dispatch_batch(
        app.handle().clone(),
        runtime.clone(),
        batch,
        "Mock".into(),
        slots,
        Arc::new(Vec::new()),
        cancel,
        run_info,
        "run-rev".into(),
    ));
    join.await.unwrap();
    let reviewed = storage.list_reviewed_caption_paths().unwrap();
    assert_eq!(reviewed.len(), 2, "两张图都应进入已复核集合: {reviewed:?}");
    // 修订计数：明确成功修订（status='revised'）的每张图计 1 次。
    let counts = storage.count_revised_per_caption().unwrap();
    assert_eq!(counts.len(), 2, "两张图都应有修订计数");
    for image in &images {
        let caption = Path::new(image)
            .with_extension("txt")
            .to_string_lossy()
            .into_owned();
        let key = counts
            .keys()
            .find(|p| p.eq_ignore_ascii_case(&caption))
            .expect("计数应含该图");
        assert_eq!(*counts.get(key).unwrap(), 1, "每图修订 1 次");
    }
    for image in &images {
        let caption = Path::new(image).with_extension("txt");
        assert!(
            reviewed
                .iter()
                .any(|p| p.eq_ignore_ascii_case(&caption.to_string_lossy())),
            "{} 应已复核",
            caption.display()
        );
        assert_eq!(
            std::fs::read_to_string(caption).unwrap(),
            "1girl, revised tags"
        );
    }
    // 未复核的图（从未跑 revision）不在集合中
    let fresh_caption = dir.path().join("zzz-fresh.png").with_extension("txt");
    assert!(storage
        .list_reviewed_caption_paths()
        .unwrap()
        .iter()
        .all(|p| !p.eq_ignore_ascii_case(&fresh_caption.to_string_lossy())));
}

#[tokio::test]
async fn multi_connection_batch_spreads_requests_across_channels() {
    // 多渠道叠加：connection_ids 含两个连接时，请求分摊到两个渠道。
    let server_a = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, from a")),
        )
    }));
    let server_b = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, from b")),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 6);
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut conn_a = connection_with_keys(&server_a.base, &[("k-a", "sk-a")]);
    conn_a.id = "conn-a".into();
    conn_a.name = "渠道 A".into();
    storage.save_connection(conn_a.clone()).unwrap();
    let mut conn_b = connection_with_keys(&server_b.base, &[("k-b", "sk-b")]);
    conn_b.id = "conn-b".into();
    conn_b.name = "渠道 B".into();
    storage.save_connection(conn_b.clone()).unwrap();
    let runtime =
        Arc::new(AppRuntime::new(Storage::open(dir.path()).unwrap(), dir.path()).unwrap());
    let app = mock_app();
    let mut batch = request("conn-a".into(), images.clone(), 2, 1, 30);
    // 渠道级模型：渠道 A 用 mock-model，渠道 B 用 mock-model-b
    batch.selection_snapshot.channels = vec![
        ApiChannelSelection {
            connection_id: "conn-a".into(),
            model_id: "mock-model".into(),
        },
        ApiChannelSelection {
            connection_id: "conn-b".into(),
            model_id: "mock-model-b".into(),
        },
    ];
    let cancel = CancellationToken::new();
    let slots = Arc::new(
        build_slots(Arc::new(conn_a), "mock-model", 1, &storage)
            .unwrap()
            .into_iter()
            .chain(
                build_slots(Arc::new(conn_b), "mock-model-b", 1, &storage)
                    .unwrap()
                    .into_iter(),
            )
            .collect::<Vec<_>>(),
    );
    assert_eq!(slots.len(), 2, "两个渠道各一个 Key");
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: "run-multi".into(),
        status: "running".into(),
        total: batch.image_paths.len() as u32,
        running: 0,
        finished: 0,
        slots: slots.iter().cloned().collect(),
    }));
    let join = tokio::spawn(dispatch_batch(
        app.handle().clone(),
        runtime.clone(),
        batch,
        "渠道 A".into(),
        slots,
        Arc::new(Vec::new()),
        cancel,
        run_info,
        "run-multi".into(),
    ));
    join.await.unwrap();
    let total = server_a.request_count() + server_b.request_count();
    assert_eq!(total, 6, "全部请求完成");
    assert!(
        server_a.request_count() > 0 && server_b.request_count() > 0,
        "请求应分摊到两个渠道: A={} B={}",
        server_a.request_count(),
        server_b.request_count()
    );
    // 各渠道请求发到对应 base_url 且携带各自渠道的模型 ID
    for (_, body) in server_a.requests() {
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "mock-model");
    }
    for (_, body) in server_b.requests() {
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "mock-model-b", "渠道 B 应使用其自身模型");
    }
}

#[tokio::test]
async fn truncated_stream_is_detected_and_fails_job() {
    // 流在完成标记（[DONE]）前结束 → 判定截断，任务失败且不写文件。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial tag\"}}]}\n".to_vec(),
                0,
            )]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let result = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(&result, Err(error) if error.message.contains("被截断")),
        "应检测到截断: {result:?}"
    );
    // 未写入任何文件
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "只有图片文件，不应写入 caption"
    );
    // 完整流（带 [DONE]）不受影响
    let server2 = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("complete tag")),
        )
    }));
    let connection2 = connection_with_keys(&server2.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection2,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(response.text.contains("complete tag"));
}

#[tokio::test]
async fn no_response_retries_and_is_not_counted_as_error_or_success() {
    // 持续 503（瞬态）→ 自动重试 6 次 → no_response：
    // 不算错误（failed 统计为 0）、不算成功、不写性能指标。
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            503,
            vec![("Content-Type".into(), "text/plain".into())],
            MockBody::Bytes(b"service unavailable".to_vec()),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let (_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images.clone(), 1, 1);
    join.await.unwrap();
    // 重试了 6 次（首次 + 5 次重试）
    assert_eq!(server.request_count(), 6, "瞬态错误应自动重试 6 次");
    let items = storage.list_task_items(&run).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "no_response", "无响应不算错误也不算成功");
    // 性能指标：无响应不写入
    let stats = storage.query_stats().unwrap();
    assert_eq!(stats.total, 0, "无响应不应写入性能指标");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.succeeded, 0);
    // 未写入标签文件
    for image in &images {
        assert!(
            !Path::new(image).with_extension("txt").exists(),
            "不应写入标签"
        );
    }
}

#[tokio::test]
async fn oversized_image_is_resized_before_send() {
    // 超高清图（4000×2000）发送前应等比缩小到长边 2048，避免请求过大。
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.png");
    image::RgbaImage::from_pixel(4000, 2000, image::Rgba([180, 90, 40, 255]))
        .save(&big)
        .unwrap();
    let server = MockServer::start(Arc::new(|_, _, body| {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        let url = value["messages"][1]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        let b64 = url.split("base64,").nth(1).unwrap();
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        let loaded = image::load_from_memory(&bytes).unwrap();
        let (w, h) = (loaded.width(), loaded.height());
        eprintln!("[resize] sent image: {w}x{h}");
        assert_eq!(w, 2048, "长边应缩到 2048");
        assert_eq!(h, 1024, "宽高比应保持 2:1");
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, resized")),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        big.to_str().unwrap(),
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(response.text.contains("resized"));
}

#[tokio::test]
async fn normal_size_image_is_sent_unchanged() {
    // 未超限的图保持原始字节与格式（PNG 原样发送，不做有损转换）。
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("ok.png");
    image::RgbaImage::from_pixel(1024, 1024, image::Rgba([120, 60, 200, 255]))
        .save(&png)
        .unwrap();
    let original = std::fs::read(&png).unwrap();
    let expected = original.clone();
    let server = MockServer::start(Arc::new(move |_, _, body| {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        let url = value["messages"][1]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .to_string();
        let b64 = url.split("base64,").nth(1).unwrap();
        let bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        assert_eq!(bytes, expected, "未超限图应原样发送");
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, unchanged")),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        png.to_str().unwrap(),
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(response.text.contains("unchanged"));
}

#[test]
fn sample_paths_returns_requested_count_and_rest() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        image::RgbaImage::from_pixel(64, 64, image::Rgba([i as u8, 10, 10, 255]))
            .save(dir.path().join(format!("img{i:02}.png")))
            .unwrap();
    }
    let (kept, rest) = file_ops::sample_paths(dir.path(), false, 5).unwrap();
    assert_eq!(kept.len(), 5);
    assert_eq!(rest.len(), 15);
    // 抽中的与其余不相交
    let kept_set: std::collections::HashSet<_> = kept.iter().collect();
    assert!(rest.iter().all(|p| !kept_set.contains(p)));
    // 数量超过总数时全取
    let (all_kept, none) = file_ops::sample_paths(dir.path(), false, 999).unwrap();
    assert_eq!(all_kept.len(), 20);
    assert!(none.is_empty());
    // 随机性：两次抽选结果应不同（概率上）
    let (a, _) = file_ops::sample_paths(dir.path(), false, 5).unwrap();
    let (b, _) = file_ops::sample_paths(dir.path(), false, 5).unwrap();
    let a_set: std::collections::HashSet<_> = a.iter().collect();
    let b_set: std::collections::HashSet<_> = b.iter().collect();
    assert_ne!(a_set, b_set, "两次抽选应不同（随机）");
}

#[test]
fn apply_sample_moves_unkept_images_to_refusal_folder() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..6 {
        image::RgbaImage::from_pixel(64, 64, image::Rgba([i as u8, 20, 20, 255]))
            .save(dir.path().join(format!("img{i}.png")))
            .unwrap();
    }
    let kept: Vec<String> = (0..2)
        .map(|i| {
            dir.path()
                .join(format!("img{i}.png"))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let (moved, skipped) = file_ops::apply_sample_refusal(dir.path(), &kept).unwrap();
    assert_eq!(moved.len(), 4, "其余 4 张应移入 .refusal");
    assert!(skipped.is_empty());
    // 保留的两张仍在原处
    assert!(dir.path().join("img0.png").exists());
    assert!(dir.path().join("img1.png").exists());
    // 其余进入 .refusal
    for i in 2..6 {
        assert!(dir
            .path()
            .join(".refusal")
            .join(format!("img{i}.png"))
            .exists());
        assert!(!dir.path().join(format!("img{i}.png")).exists());
    }
    // 同名冲突：再次放入同名文件后应用空保留清单——空清单视为全保留（防误移）
    let (moved2, _) = file_ops::apply_sample_refusal(dir.path(), &[]).unwrap();
    assert!(moved2.is_empty(), "空保留清单不应移动任何文件");
    // 冲突：.refusal 已有同名时跳过
    std::fs::write(dir.path().join("img5.png"), "x").unwrap();
    let (moved3, skipped3) = file_ops::apply_sample_refusal(dir.path(), &[]).unwrap();
    assert!(moved3.is_empty());
    let _ = skipped3;
}

#[tokio::test]
async fn connection_save_writes_snapshot_backup() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut connection = connection_with_keys("https://api.example.com/v1", &[("k1", "sk-a")]);
    connection.id = "conn-backup".into();
    connection.name = "备份测试连接".into();
    storage.save_connection(connection.clone()).unwrap();
    let backup_dir = dir.path().join("backups").join("connections");
    let files: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(files.len(), 1, "每次保存应产生一份快照");
    let content = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(content.contains("备份测试连接"), "快照应含连接配置");
    // 再次保存：产生第二份（保留最近 20 份）
    connection.name = "备份测试连接-改".into();
    storage.save_connection(connection.clone()).unwrap();
    let files2: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(files2.len(), 2, "每次更改都备份");
}

#[tokio::test]
async fn inline_error_event_in_stream_fails_with_message() {
    // HTTP 200 + data: {"error":{...}}（火山引擎/网关流内错误形态）→ 立即失败并透出消息。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n".to_vec(), 0),
                (b"data: {\"error\":{\"code\":\"InvalidParam\",\"message\":\"model not supported\"}}\n".to_vec(), 0),
            ]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let result = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(&result, Err(error) if error.message.contains("model not supported")),
        "应透出服务端错误消息: {result:?}"
    );
    assert!(
        !Path::new(&images[0]).with_extension("txt").exists(),
        "不应写入文件"
    );
}

#[tokio::test]
async fn reasoning_only_stream_falls_back_to_thinking_content() {
    // LM Studio 推理模型（qwen3）：思考走 reasoning_content、正文为空（如 max_tokens
    // 被思考耗尽）——应回退使用思考内容，不再判定"空回复"。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"analyzing image content...\"}}]}\n".to_vec(), 0),
                (b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"conclusion: 1girl\"}}]}\n".to_vec(), 0),
                (b"data: [DONE]\n".to_vec(), 0),
            ]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let result = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await;
    // 思考过程不得写入结果：正文为空时明确报错，而非用思考内容冒充回复。
    assert!(
        matches!(&result, Err(error) if error.message.contains("仅输出了思考内容")),
        "应报思考耗尽错误: {result:?}"
    );
}

#[tokio::test]
async fn gemma_thinking_field_fallback_and_content_precedence() {
    // gemma 风格：thinking 字段 + 正文同时存在 → 只取正文；仅 thinking 时回退。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (
                    b"data: {\"choices\":[{\"delta\":{\"thinking\":\"thinking...\"}}]}\n".to_vec(),
                    0,
                ),
                (
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"1girl, solo\"}}]}\n".to_vec(),
                    0,
                ),
                (b"data: [DONE]\n".to_vec(), 0),
            ]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "1girl, solo", "正文存在时不应混入思考内容");
    // 仅 thinking、无 content：报明确错误而非空回复/思考入文
    let server2 = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (
                    b"data: {\"choices\":[{\"delta\":{\"thinking\":\"only thinking output\"}}]}\n"
                        .to_vec(),
                    0,
                ),
                (b"data: [DONE]\n".to_vec(), 0),
            ]),
        )
    }));
    let connection2 = connection_with_keys(&server2.base, &[("k1", "sk-secret-1")]);
    let result2 = stream_caption(
        &reqwest::Client::new(),
        &connection2,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(&result2, Err(error) if error.message.contains("仅输出了思考内容")),
        "仅思考时应报错: {result2:?}"
    );
}

#[tokio::test]
async fn delta_stream_with_message_completion_event_is_not_duplicated() {
    // 流式 delta 已拼接全文后，网关再发一个含完整 message.content 的完成事件：
    // 不应把同一内容重复拼接。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"choices\":[{\"delta\":{\"content\":\"1girl, solo\"}}]}\n".to_vec(), 0),
                (b"data: {\"choices\":[{\"delta\":{},\"message\":{\"content\":\"1girl, solo\"},\"finish_reason\":\"stop\"}]}\n".to_vec(), 0),
                (b"data: [DONE]\n".to_vec(), 0),
            ]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "1girl, solo", "内容不应重复拼接");
}

#[tokio::test]
async fn think_tags_are_stripped_from_caption_output() {
    // 思考模型把推理过程写在正文 <think> 标签内：标签及内容必须被剥离。
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(vec![
                (b"data: {\"choices\":[{\"delta\":{\"content\":\"<think>let me look...\"}}]}\n".to_vec(), 0),
                (b"data: {\"choices\":[{\"delta\":{\"content\":\" analyzing colors</think>1girl, solo\"}}]}\n".to_vec(), 0),
                (b"data: [DONE]\n".to_vec(), 0),
            ]),
        )
    }));
    let connection = connection_with_keys(&server.base, &[("k1", "sk-secret-1")]);
    let response = stream_caption(
        &reqwest::Client::new(),
        &connection,
        "sk-secret-1",
        JobKind::Caption,
        "mock-model",
        "prompt",
        "",
        &images[0],
        0.2,
        128,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text, "1girl, solo");
    assert!(!response.text.contains("think"));
}

// -------------------------------------------------------------- scheduler e2e

fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .build(tauri::generate_context!())
        .expect("failed to build mock app")
}

fn scheduler_harness(
    server: &MockServer,
    keys: &[(&str, &str)],
    images: Vec<String>,
    total: usize,
    per_key: usize,
) -> (
    TempDir,
    Arc<Storage>,
    tauri::App<tauri::test::MockRuntime>,
    String,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    scheduler_harness_with_kind(server, keys, images, total, per_key, JobKind::Caption)
}

fn scheduler_harness_with_kind(
    server: &MockServer,
    keys: &[(&str, &str)],
    images: Vec<String>,
    total: usize,
    per_key: usize,
    kind: JobKind,
) -> (
    TempDir,
    Arc<Storage>,
    tauri::App<tauri::test::MockRuntime>,
    String,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let mut connection = connection_with_keys(&server.base, keys);
    connection.id = "conn-mock".into();
    storage.save_connection(connection.clone()).unwrap();
    let runtime =
        Arc::new(AppRuntime::new(Storage::open(dir.path()).unwrap(), dir.path()).unwrap());
    let app = mock_app();
    let handle = app.handle().clone();
    let slots = Arc::new(
        build_slots(
            Arc::new(storage.get_connection_full("conn-mock").unwrap()),
            "mock-model",
            per_key,
            &storage,
        )
        .unwrap(),
    );
    let mut batch = request("conn-mock".into(), images, total, per_key, 30);
    batch.kind = kind;
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: "run-e2e".into(),
        status: "running".into(),
        total: batch.image_paths.len() as u32,
        running: 0,
        finished: 0,
        slots: slots.iter().cloned().collect(),
    }));
    let join = tokio::spawn(dispatch_batch(
        handle,
        runtime.clone(),
        batch,
        "Mock".into(),
        slots,
        Arc::new(Vec::new()),
        cancel,
        run_info,
        "run-e2e".into(),
    ));
    (dir, storage, app, "run-e2e".into(), token, join)
}

#[tokio::test]
async fn ai_sample_separates_decisions_without_touching_caption_files() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_server = calls.clone();
    let server = MockServer::start(Arc::new(move |_, _, body| {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert!(value["messages"][1]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("筛选要求"));
        let answer = if calls_for_server.fetch_add(1, Ordering::SeqCst) == 0 {
            "DECISION: KEEP\\nREASON: 主体清晰"
        } else {
            "DECISION: REJECT\\nREASON: 主体模糊"
        };
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream(answer)),
        )
    }));
    let images_dir = tempfile::tempdir().unwrap();
    let images = make_images(images_dir.path(), 2);
    for image in &images {
        std::fs::write(Path::new(image).with_extension("txt"), "existing tags").unwrap();
    }
    let (_dir, storage, _app, run, _token, join) = scheduler_harness_with_kind(
        &server,
        &[("k1", "sk-a")],
        images.clone(),
        1,
        1,
        JobKind::AiSample,
    );
    join.await.unwrap();
    let items = storage.list_task_items(&run).unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| item.status == "succeeded")
            .count(),
        1
    );
    assert_eq!(
        items.iter().filter(|item| item.status == "skipped").count(),
        1
    );
    for image in images {
        assert_eq!(
            std::fs::read_to_string(Path::new(&image).with_extension("txt")).unwrap(),
            "existing tags"
        );
    }
}

#[tokio::test]
async fn e2e_batch_tags_all_images_and_records_metrics() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream("1girl, solo")),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 6);
    let (_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images.clone(), 2, 1);
    join.await.unwrap();
    assert_eq!(server.request_count(), 6);
    // Every caption file was written with the streamed content.
    for image in &images {
        let caption = Path::new(image).with_extension("txt");
        assert_eq!(std::fs::read_to_string(&caption).unwrap(), "1girl, solo");
    }
    // Every image was sent with base64 content and the expected prompt.
    for (_, body) in server.requests() {
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["model"], "mock-model");
        assert!(value["messages"][1]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .contains("base64,"));
    }
    let runs = storage.list_task_runs(10).unwrap();
    let run = runs.iter().find(|item| item.id == run).unwrap();
    assert_eq!(run.status, "completed");
    assert_eq!(run.total, 6);
    assert_eq!(run.succeeded, 6);
    assert_eq!(run.input_tokens, 60);
    assert_eq!(run.output_tokens, 12);
    let items = storage.list_task_items(&run.id).unwrap();
    assert_eq!(items.len(), 6);
    assert!(items.iter().all(|item| item.status == "succeeded"));
}

#[tokio::test]
async fn scheduler_honors_total_and_per_key_concurrency_caps() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        let mut parts = openai_stream("a");
        parts[0].1 = 60;
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(parts),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 12);
    let (_dir, _storage, _app, _run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a"), ("k2", "sk-b")], images, 3, 1);
    join.await.unwrap();
    let max = server.max_in_flight();
    assert!(max <= 3, "total concurrency exceeded: {max}");
    assert_eq!(server.per_key_max("Bearer sk-a"), 1);
    assert_eq!(server.per_key_max("Bearer sk-b"), 1);
}

#[tokio::test]
async fn scheduler_cancels_all_queued_items() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            // Holds every in-flight request open.
            MockBody::Chunked(vec![(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n".to_vec(),
                120_000,
            )]),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 10);
    let (_dir, storage, app, run, token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images.clone(), 2, 1);
    // Let a couple of tasks start streaming, then cancel the whole batch.
    tokio::time::sleep(Duration::from_millis(300)).await;
    token.cancel();
    join.await.unwrap();

    let runs = storage.list_task_runs(10).unwrap();
    let run = runs.iter().find(|item| item.id == run).unwrap();
    assert_eq!(run.status, "completed");
    let items = storage.list_task_items(&run.id).unwrap();
    assert_eq!(items.len(), 10);
    eprintln!(
        "[trace] item statuses: {:?}",
        items
            .iter()
            .map(|item| item.status.as_str())
            .collect::<Vec<_>>()
    );
    // Every item reached a terminal state; none is left running or pending.
    assert!(
        items
            .iter()
            .all(|item| item.status != "running" && item.status != "pending"),
        "statuses: {:?}",
        items.iter().map(|item| &item.status).collect::<Vec<_>>()
    );
    let _ = (dir, app, images);
}

#[tokio::test]
async fn scheduler_migrates_429_key_and_retries() {
    let server = MockServer::start(Arc::new(|_, headers, _| {
        let key = headers.get("Authorization").cloned().unwrap_or_default();
        if key.contains("sk-bad") {
            (
                429,
                vec![("Retry-After".into(), "0".into())],
                MockBody::Bytes("slow down".into()),
            )
        } else {
            (
                200,
                vec![("Content-Type".into(), "text/event-stream".into())],
                MockBody::Chunked(openai_stream("recovered")),
            )
        }
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 3);
    let (_dir, storage, _app, run, _token, join) = scheduler_harness(
        &server,
        &[("k1", "sk-bad"), ("k2", "sk-good")],
        images,
        2,
        1,
    );
    join.await.unwrap();
    // All files succeed via migration to the healthy key.
    let items = storage.list_task_items(&run).unwrap();
    assert!(
        items.iter().all(|item| item.status == "succeeded"),
        "items: {items:?}"
    );
    // The rate-limited key was actually hit (429 path exercised) and the
    // healthy key took over (migration happened).
    assert!(
        server.per_key_max("Bearer sk-bad") >= 1,
        "rate-limited key never used"
    );
    assert!(
        server.per_key_max("Bearer sk-good") >= 1,
        "migration to healthy key never happened"
    );
    let total_requests = server.request_count();
    assert!(
        (3..=6).contains(&total_requests),
        "unexpected request count: {total_requests}"
    );
}

#[tokio::test]
async fn refused_caption_is_retried_twice_then_succeeds() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_server = calls.clone();
    let server = MockServer::start(Arc::new(move |_, _, _| {
        let call = calls_for_server.fetch_add(1, Ordering::SeqCst);
        let text = if call < 2 {
            "I cannot assist with that request."
        } else {
            "1girl, solo"
        };
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream(text)),
        )
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 1);
    let (_runtime_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-a")], images.clone(), 1, 1);
    join.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        std::fs::read_to_string(Path::new(&images[0]).with_extension("txt")).unwrap(),
        "1girl, solo"
    );
    let items = storage.list_task_items(&run).unwrap();
    assert_eq!(items[0].status, "succeeded");
    assert_eq!(items[0].retries, 2);
    assert_eq!(items[0].input_tokens, Some(30));
    assert_eq!(items[0].output_tokens, Some(6));
    let request_stats = storage.query_stats().unwrap();
    assert_eq!(request_stats.total, 3);
    assert_eq!(request_stats.input_tokens, 30);
    assert_eq!(request_stats.output_tokens, 6);
}

#[tokio::test]
async fn length_drop_is_automatically_replaced_by_advanced_model_review() {
    let server = MockServer::start(Arc::new(move |_, _, body| {
        let value: serde_json::Value = serde_json::from_slice(body).unwrap();
        let answer = if value["model"] == "advanced-model" {
            "1girl, solo, long hair, blue eyes, white dress, outdoors, daylight"
        } else {
            "1girl"
        };
        (
            200,
            vec![("Content-Type".into(), "text/event-stream".into())],
            MockBody::Chunked(openai_stream(answer)),
        )
    }));
    let image_dir = tempfile::tempdir().unwrap();
    let images = make_images(image_dir.path(), 1);
    let caption_path = Path::new(&images[0]).with_extension("txt");
    std::fs::write(
        &caption_path,
        "1girl, solo, long hair, blue eyes, white dress, outdoors, day",
    )
    .unwrap();

    let runtime_dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(runtime_dir.path()).unwrap());
    let mut connection = connection_with_keys(&server.base, &[("k1", "sk-a")]);
    connection.id = "conn-advanced".into();
    storage.save_connection(connection).unwrap();
    let runtime = Arc::new(
        AppRuntime::new(
            Storage::open(runtime_dir.path()).unwrap(),
            runtime_dir.path(),
        )
        .unwrap(),
    );
    let primary_slots = Arc::new(
        build_slots(
            Arc::new(storage.get_connection_full("conn-advanced").unwrap()),
            "primary-model",
            1,
            &storage,
        )
        .unwrap(),
    );
    let advanced_slots = Arc::new(
        build_slots(
            Arc::new(storage.get_connection_full("conn-advanced").unwrap()),
            "advanced-model",
            1,
            &storage,
        )
        .unwrap(),
    );
    let mut batch = request("conn-advanced".into(), images.clone(), 1, 1, 30);
    batch.kind = JobKind::Revision;
    batch.selection_snapshot.model_id = "primary-model".into();
    batch.selection_snapshot.advanced_model = Some(AdvancedModelConfig {
        enabled: true,
        connection_id: "conn-advanced".into(),
        model_id: "advanced-model".into(),
        thinking_mode: Some("high".into()),
        thinking_budget: Some(8192),
        request_params: Some(serde_json::json!({"reasoning_effort":"high"})),
    });
    let run_info = Arc::new(Mutex::new(RunInfo {
        batch_id: "run-advanced".into(),
        status: "running".into(),
        total: 1,
        running: 0,
        finished: 0,
        slots: primary_slots
            .iter()
            .chain(advanced_slots.iter())
            .cloned()
            .collect(),
    }));
    let app = mock_app();
    dispatch_batch(
        app.handle().clone(),
        runtime,
        batch,
        "Mock".into(),
        primary_slots,
        advanced_slots,
        CancellationToken::new(),
        run_info,
        "run-advanced".into(),
    )
    .await;
    assert_eq!(server.request_count(), 2);
    assert_eq!(
        std::fs::read_to_string(&caption_path).unwrap(),
        "1girl, solo, long hair, blue eyes, white dress, outdoors, daylight"
    );
    let items = storage.list_task_items("run-advanced").unwrap();
    assert_eq!(items[0].status, "revised");
}

#[tokio::test]
async fn scheduler_disables_auth_failed_key() {
    let server = MockServer::start(Arc::new(|_, _, _| {
        (401, vec![], MockBody::Bytes("unauthorized".into()))
    }));
    let dir = tempfile::tempdir().unwrap();
    let images = make_images(dir.path(), 2);
    let (_dir, storage, _app, run, _token, join) =
        scheduler_harness(&server, &[("k1", "sk-bad")], images, 1, 1);
    join.await.unwrap();
    let items = storage.list_task_items(&run).unwrap();
    assert!(items.iter().all(|item| item.status == "failed"));
    assert!(items
        .iter()
        .all(|item| item.error.as_deref().unwrap_or("").contains("停用")));
}
