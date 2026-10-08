import { useEffect, useState } from "react";
import { KeyRound, PlugZap, Plus, Save, Trash2, Zap } from "lucide-react";
import { backend } from "../lib/backend";
import { createConnection } from "../lib/defaults";
import type { ApiConnection, ApiKeyInput, ProviderKind } from "../types";

interface Props {
  connections: ApiConnection[];
  onSave: (connection: ApiConnection) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onLog: (level: "info" | "success" | "warn" | "error", scope: "system", message: string) => void;
}

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}

interface ProviderPreset {
  name: string;
  provider: ProviderKind;
  baseUrl: string;
  /** 该服务商官方文档确认的推荐请求体参数（思考额度等）。 */
  defaultParams?: Record<string, unknown>;
}

/**
 * 知名 API 服务商预设：收在一个下拉栏里，未配置时不会平铺显示；
 * 选择后立即填充名称/供应商/Base URL 与推荐请求参数。
 * 思考参数格式均以官方文档为准（OpenAI reasoning_effort、Anthropic
 * thinking.budget_tokens、DeepSeek thinking+reasoning_effort、通义
 * enable_thinking、OpenRouter reasoning_effort 等）。
 */
const PROVIDER_PRESETS: ProviderPreset[] = [
  { name: "OpenAI", provider: "openai_compatible", baseUrl: "https://api.openai.com/v1", defaultParams: { reasoning_effort: "medium" } },
  { name: "Anthropic", provider: "anthropic", baseUrl: "https://api.anthropic.com", defaultParams: { thinking: { type: "enabled", budget_tokens: 4096 } } },
  { name: "Google Gemini", provider: "gemini", baseUrl: "https://generativelanguage.googleapis.com" },
  { name: "DeepSeek", provider: "openai_compatible", baseUrl: "https://api.deepseek.com", defaultParams: { thinking: { type: "enabled" }, reasoning_effort: "medium" } },
  { name: "Moonshot Kimi", provider: "openai_compatible", baseUrl: "https://api.moonshot.cn/v1" },
  { name: "智谱 GLM", provider: "openai_compatible", baseUrl: "https://open.bigmodel.cn/api/paas/v4" },
  { name: "阿里通义千问", provider: "openai_compatible", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", defaultParams: { enable_thinking: true } },
  { name: "百度千帆", provider: "openai_compatible", baseUrl: "https://qianfan.baidubce.com/v2" },
  { name: "腾讯混元", provider: "openai_compatible", baseUrl: "https://api.hunyuan.cloud.tencent.com/v1" },
  { name: "火山方舟", provider: "openai_compatible", baseUrl: "https://ark.cn-beijing.volces.com/api/v3" },
  { name: "硅基流动 SiliconFlow", provider: "openai_compatible", baseUrl: "https://api.siliconflow.cn/v1" },
  { name: "OpenRouter", provider: "openai_compatible", baseUrl: "https://openrouter.ai/api/v1", defaultParams: { reasoning_effort: "medium" } },
  { name: "Groq", provider: "openai_compatible", baseUrl: "https://api.groq.com/openai/v1" },
  { name: "Ollama 本地", provider: "openai_compatible", baseUrl: "http://localhost:11434/v1" },
  { name: "LM Studio 本地", provider: "openai_compatible", baseUrl: "http://localhost:1234/v1" },
  { name: "vLLM 本地", provider: "openai_compatible", baseUrl: "http://localhost:8000/v1" },
];

/**
 * API 连接库主视图（与打标任务等页面同级显示在右侧区域）：
 * 左侧连接列表 + 新建，右侧表单；支持连接测试与 Base URL 自动补全提示。
 */
export function ConnectionsView({ connections, onSave, onDelete, onLog }: Props) {
  const [selectedId, setSelectedId] = useState(connections[0]?.id ?? "new");
  const [draft, setDraft] = useState<ApiConnection>(connections[0] ?? createConnection());
  const [headersText, setHeadersText] = useState("{}");
  const [paramsText, setParamsText] = useState("{}");
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testText, setTestText] = useState<string>();
  const [testOk, setTestOk] = useState(false);
  const [presetProvider, setPresetProvider] = useState("");

  function applyProviderPreset(name?: string) {
    const preset = PROVIDER_PRESETS.find((item) => item.name === (name ?? presetProvider));
    if (!preset) return;
    const current = draft;
    const isFresh = !current.name || (current.name === "OpenAI 兼容 API" && !current.baseUrl);
    setDraft((prev) => ({
      ...prev,
      name: isFresh ? preset.name : prev.name,
      provider: preset.provider,
      baseUrl: preset.baseUrl,
      defaultParams: preset.defaultParams ?? {},
    }));
    setParamsText(JSON.stringify(preset.defaultParams ?? {}, null, 2));
    setPresetProvider("");
    setError("");
  }

  useEffect(() => {
    if (!connections.some((connection) => connection.id === selectedId)) {
      setSelectedId(connections[0]?.id ?? "new");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connections]);

  useEffect(() => {
    const next = selectedId === "new" ? createConnection() : connections.find((c) => c.id === selectedId) ?? createConnection();
    setDraft(structuredClone(next));
    setHeadersText(JSON.stringify(next.headers ?? {}, null, 2));
    setParamsText(JSON.stringify(next.defaultParams ?? {}, null, 2));
    setError("");
    setTestText(undefined);
  }, [selectedId, connections]);

  function patch<K extends keyof ApiConnection>(key: K, value: ApiConnection[K]) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  function updateKey(index: number, value: Partial<ApiKeyInput>) {
    patch("keyRefs", draft.keyRefs.map((item, i) => i === index ? { ...item, ...value } : item));
  }

  async function testConnection() {
    if (selectedId === "new" || testing) return;
    if (!isTauri()) {
      setTestOk(false);
      setTestText("演示模式下无法测试连接");
      return;
    }
    setTesting(true);
    setTestText(undefined);
    try {
      const result = await backend.testConnection(selectedId);
      setTestOk(result.ok);
      const autoNote = result.effectiveBaseUrl && result.effectiveBaseUrl !== draft.baseUrl
        ? ` · 已自动补全 Base URL：${result.effectiveBaseUrl}`
        : "";
      setTestText(result.ok
        ? `连接正常 · ${result.latencyMs ?? "—"} ms · 发现 ${result.modelsFetched} 个模型${autoNote}`
        : `连接失败：${result.error ?? "未知错误"}${autoNote}`);
      if (result.ok && result.effectiveBaseUrl && result.effectiveBaseUrl !== draft.baseUrl) {
        setDraft((current) => ({ ...current, baseUrl: result.effectiveBaseUrl! }));
      }
    } catch (reason) {
      setTestOk(false);
      setTestText(`连接测试失败：${String(reason)}`);
    } finally {
      setTesting(false);
    }
  }

  async function submit() {
    if (!draft.name.trim() || !draft.baseUrl.trim()) return setError("连接名称和 Base URL 必填");
    if (draft.keyRefs.length === 0) return setError("至少添加一个 API Key");
    let headers: Record<string, string>;
    try {
      headers = JSON.parse(headersText);
      if (!headers || Array.isArray(headers) || typeof headers !== "object") throw new Error();
    } catch {
      return setError("附加请求头必须是 JSON 对象");
    }
    let defaultParams: Record<string, unknown> | undefined;
    if (paramsText.trim()) {
      try {
        defaultParams = JSON.parse(paramsText);
        if (!defaultParams || Array.isArray(defaultParams) || typeof defaultParams !== "object") throw new Error();
      } catch {
        return setError("额外请求参数必须是 JSON 对象");
      }
    }
    setSaving(true);
    try {
      await onSave({ ...draft, headers, defaultParams, totalConcurrency: Math.min(32, Math.max(1, draft.totalConcurrency)), perKeyConcurrency: Math.min(8, Math.max(1, draft.perKeyConcurrency)) });
      setSelectedId(draft.id);
      setError("");
      setTestText(undefined);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }

  async function remove() {
    if (selectedId === "new") return;
    const connection = connections.find((item) => item.id === selectedId);
    if (!connection) return;
    if (!window.confirm(`确定删除连接“${connection.name}”？已被历史指标引用的连接无法删除。`)) return;
    try {
      await onDelete(selectedId);
      setSelectedId(connections.filter((item) => item.id !== selectedId)[0]?.id ?? "new");
      onLog("warn", "system", "API 连接已删除");
    } catch (reason) {
      onLog("error", "system", `删除连接失败：${String(reason)}`);
    }
  }

  return (
    <section className="page-card connections-page">
      <div className="page-card-header">
        <div><h2>API 连接库</h2><p>连接、模型与加密 Key 池集中管理；Base URL 会自动尝试常见格式并补全</p></div>
      </div>
      <div className="connection-layout">
        <aside className="connection-list">
          {connections.map((connection) => (
            <button key={connection.id} className={selectedId === connection.id ? "active" : ""} onClick={() => setSelectedId(connection.id)}>
              <span className={`provider-dot ${connection.provider}`} />
              <span><b>{connection.name}</b><small>{connection.keyRefs.length} 个 Key</small></span>
            </button>
          ))}
          <button className={selectedId === "new" ? "active" : ""} onClick={() => setSelectedId("new")}><Plus size={16} /> 新建连接</button>
        </aside>
        <div className="connection-form scroll-area">
          <div className="form-grid two">
            <label>连接名称<input value={draft.name} onChange={(e) => patch("name", e.target.value)} /></label>
            <label>供应商<select value={draft.provider} onChange={(e) => patch("provider", e.target.value as ProviderKind)}><option value="openai_compatible">OpenAI 兼容</option><option value="anthropic">Anthropic</option><option value="gemini">Gemini</option></select></label>
          </div>
          <div className="preset-bar">
            <span className="preset-bar-label"><Zap size={13} />从预设服务商填充</span>
            <select aria-label="预设服务商" value={presetProvider} onChange={(e) => { setPresetProvider(e.target.value); applyProviderPreset(e.target.value); }}>
              <option value="">选择知名服务商…（不会自动创建连接）</option>
              {PROVIDER_PRESETS.map((item) => <option key={item.name} value={item.name}>{item.name}</option>)}
            </select>
          </div>
          <label>Base URL<span className="field-hint">可只填主机地址；保存或测试时会自动尝试 /v1、/v1beta 等常见布局并回填有效格式</span><input value={draft.baseUrl} onChange={(e) => patch("baseUrl", e.target.value)} spellCheck={false} placeholder="https://api.example.com/v1" /></label>
          <label>附加请求头（JSON）<textarea className="code-input small" value={headersText} onChange={(e) => setHeadersText(e.target.value)} spellCheck={false} /></label>
          <label>额外请求参数（JSON，可选）<span className="field-hint">从预设服务商填充时会带上该家的推荐思考/推理参数；可自行修改或清空</span><textarea className="code-input small" value={paramsText} onChange={(e) => setParamsText(e.target.value)} spellCheck={false} placeholder={"{\n  \"reasoning_effort\": \"medium\",\n  \"thinking\": {\"type\": \"enabled\", \"budget_tokens\": 4096}\n}"} /></label>
          <div className="section-heading"><span><KeyRound size={16} /> Key 池</span><button className="text-button" onClick={() => patch("keyRefs", [...draft.keyRefs, { id: crypto.randomUUID(), label: `Key ${draft.keyRefs.length + 1}`, secret: "" }])}><Plus size={14} /> 添加</button></div>
          <div className="key-list">
            {draft.keyRefs.map((key, index) => (
              <div className="key-row" key={key.id ?? index}>
                <input aria-label="Key 备注" value={key.label} onChange={(e) => updateKey(index, { label: e.target.value })} />
                <input aria-label="API Key" type="password" value={key.secret ?? ""} placeholder={key.masked ? `已保存 ${key.masked}` : "输入 API Key"} onChange={(e) => updateKey(index, { secret: e.target.value })} />
                <button className="icon-button mini danger" aria-label="删除 Key" onClick={() => patch("keyRefs", draft.keyRefs.filter((_, i) => i !== index))}><Trash2 size={15} /></button>
              </div>
            ))}
          </div>
          <label>连接测试与请求超时（秒）<input type="number" min={10} max={600} value={draft.timeoutSeconds} onChange={(e) => patch("timeoutSeconds", Number(e.target.value))} /></label>
          {error && <div className="inline-error">{error}</div>}
          {testText && <div className={`inline-note ${testOk ? "ok" : "fail"}`}>{testText}</div>}
          <div className="connections-actions">
            <button className="button ghost" disabled={selectedId === "new" || testing} onClick={() => void testConnection()}><PlugZap size={16} />{testing ? "测试中…" : "测试连接"}</button>
            <span />
            {selectedId !== "new" && <button className="button ghost danger" onClick={() => void remove()}><Trash2 size={16} />删除连接</button>}
            <button className="button primary" disabled={saving} onClick={() => void submit()}><Save size={16} />{saving ? "正在保存…" : "保存连接"}</button>
          </div>
        </div>
      </div>
    </section>
  );
}
