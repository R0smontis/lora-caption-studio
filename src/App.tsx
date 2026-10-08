import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Virtuoso } from "react-virtuoso";
import {
  Activity, ArrowDown, ArrowUp, BarChart3, Calculator, CheckCircle2, ChevronRight, CircleAlert, Clock3, Copy, Edit3,
  FileImage, FilePlus2, FolderOpen, History, ImagePlus, KeyRound, Layers3, Moon, Play,
  Plus, RefreshCcw, RotateCcw, Save, Settings2, Shuffle, SlidersHorizontal, Sparkles, Square, Sun, Tags,
  Trash2, WandSparkles, Zap, ShieldCheck
} from "lucide-react";
import { ApiModelSelector } from "./components/ApiModelSelector";
import { AnimaTrainingDesigner } from "./components/AnimaTrainingDesigner";
import { BatchEditDialog } from "./components/BatchEditDialog";
import { ConnectionsView } from "./components/ConnectionsView";
import { DetailDrawer } from "./components/DetailDrawer";
import { ImageLightbox } from "./components/ImageLightbox";
import { LocalModelPicker } from "./components/LocalModelPicker";
import { ChannelsEditor } from "./components/ChannelsEditor";
import { SampleView } from "./components/SampleView";
import { LogPanel } from "./components/LogPanel";
import { PostprocessView } from "./components/PostprocessView";
import { TagFrequencyPanel } from "./components/TagFrequencyPanel";
import { backend } from "./lib/backend";
import { createRevisionPreset, createTagPreset } from "./lib/defaults";
import { summarizeJobs } from "./lib/metrics";
import { BUILTIN_RULE_INFO } from "./lib/refusal";
import { buildThinkingRequestParams, detectThinkingFamily, type ThinkingMode } from "./lib/thinking";
import type {
  ApiChannel, ApiConnection, ApiKeyRuntimeState, ApiSelectionSnapshot, CaptionEditBatch,
  CaptionVersion, EditBatchRecord, HistoricalStats, ImageJob, JobKind, KeyRuntimeEvent,
  LocalModelEvent, LocalModelState, LogEntry, RefusalRule, RevisionPreset, StatsBreakdown,
  StatsGroupBy, TagPreset, TaskEvent, UndoBatchResult,
} from "./types";

type Theme = "system" | "light" | "dark";
type View = "tasks" | "sample" | "connections" | "history" | "stats" | "postprocess" | "anima";

const statusLabel: Record<ImageJob["status"], string> = {
  pending: "等待", running: "处理中", succeeded: "已完成", revised: "已修订",
  no_response: "无响应", refused: "疑似拒绝", failed: "失败", skipped: "已跳过", cancelled: "已取消"
};

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}

function nowTime() {
  return new Date().toLocaleTimeString("zh-CN", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3 });
}

function buildSnapshot(preset: TagPreset | RevisionPreset, connections: ApiConnection[]): ApiSelectionSnapshot {
  const connection = connections.find((item) => item.id === preset.connectionId);
  return {
    connectionId: preset.connectionId,
    connectionName: connection?.name ?? "",
    connectionIds: [preset.connectionId, ...(preset.connectionIds ?? [])].filter((id, index, all) => id && all.indexOf(id) === index),
    channels: effectiveChannels(preset).filter((c) => c.connectionId.trim() && c.modelId.trim()),
    modelId: preset.modelId,
    localThreshold: preset.localThreshold,
    localMaxTags: preset.localMaxTags,
    localKeepUnderscores: preset.localKeepUnderscores,
    systemPrompt: preset.systemPrompt,
    temperature: preset.temperature,
    maxTokens: preset.maxTokens,
    refusalRules: preset.refusalRules,
    timeoutSeconds: preset.timeoutSeconds,
    totalConcurrency: preset.totalConcurrency,
    perKeyConcurrency: preset.perKeyConcurrency,
    requestParams: preset.requestParams,
    advancedModel: preset.advancedModel,
  };
}

export function applyApiRetryTarget(
  snapshot: ApiSelectionSnapshot,
  connections: ApiConnection[],
  target: { connectionId?: string; modelId?: string },
): ApiSelectionSnapshot {
  const connectionId = target.connectionId ?? snapshot.connectionId;
  const modelId = target.modelId ?? snapshot.modelId;
  return {
    ...snapshot,
    connectionId,
    connectionName: connections.find((item) => item.id === connectionId)?.name ?? snapshot.connectionName,
    connectionIds: [connectionId],
    channels: [{ connectionId, modelId }],
    modelId,
    localModelId: undefined,
  };
}

export function shouldHandleMainTaskEvent(event: TaskEvent): boolean {
  return event.scope !== "ai_sample";
}

/** 关键词搜索：匹配文件名、标签内容、路径与错误信息（大小写不敏感）。 */
export function searchJobs(jobs: ImageJob[], query: string): ImageJob[] {
  const q = query.trim().toLocaleLowerCase();
  if (!q) return jobs;
  return jobs.filter((job) =>
    job.fileName.toLocaleLowerCase().includes(q) ||
    job.caption.toLocaleLowerCase().includes(q) ||
    job.imagePath.toLocaleLowerCase().includes(q) ||
    (job.error ?? "").toLocaleLowerCase().includes(q)
  );
}

const TERMINAL_JOB_STATUSES = new Set<ImageJob["status"]>([
  "succeeded", "revised", "no_response", "refused", "failed", "skipped", "cancelled",
]);

/**
 * 合并后端任务事件。条目完成一次处理（无论成功、拒绝、失败、跳过或取消）后
 * 自动取消选择；pending/running 期间保持用户当前选择，避免任务刚启动就失去范围提示。
 */
export function mergeTaskJobUpdate(current: ImageJob, incoming: ImageJob): ImageJob {
  const revised = incoming.status === "revised";
  return {
    ...current,
    ...incoming,
    selected: TERMINAL_JOB_STATUSES.has(incoming.status) ? false : current.selected,
    reviewed: revised ? true : current.reviewed,
    revisionCount: revised ? (current.revisionCount ?? 0) + 1 : current.revisionCount,
  };
}

/** 按 文件名 / 修订次数 / 标签字数 排序；同值回退文件名自然序。 */
export function sortJobs(jobs: ImageJob[], key: "name" | "revision" | "captionLength", asc: boolean): ImageJob[] {
  const list = [...jobs];
  const dir = asc ? 1 : -1;
  list.sort((a, b) => {
    if (key === "revision") return (((a.revisionCount ?? 0) - (b.revisionCount ?? 0)) * dir) || a.fileName.localeCompare(b.fileName, undefined, { numeric: true });
    if (key === "captionLength") return ((a.caption.length - b.caption.length) * dir) || a.fileName.localeCompare(b.fileName, undefined, { numeric: true });
    return a.fileName.localeCompare(b.fileName, undefined, { numeric: true }) * dir;
  });
  return list;
}

/** 批量恢复目标：同路径多版本只保留选中版本中最新（id 最大）的一个，按 id 升序恢复。 */
export function dedupeRestoreTargets(picked: CaptionVersion[]): CaptionVersion[] {
  const byPath = new Map<string, CaptionVersion>();
  for (const version of picked) {
    const current = byPath.get(version.captionPath);
    if (!current || version.id > current.id) byPath.set(version.captionPath, version);
  }
  return [...byPath.values()].sort((a, b) => a.id - b.id);
}

/** 渠道列表：显式配置优先；未配置时从主连接派生（兼容旧预设）。 */
export function effectiveChannels(preset: TagPreset | RevisionPreset): ApiChannel[] {
  if (preset.channels && preset.channels.length > 0) return preset.channels;
  if (preset.connectionId) return [{ connectionId: preset.connectionId, modelId: preset.modelId }];
  return [];
}

/** 复核候选：默认跳过已复核；勾选"重新复核已复核"时全部参与。 */
export function revisionCandidates(jobs: ImageJob[], reviewAgain: boolean): ImageJob[] {
  return jobs.filter((job) => job.caption.trim() && (reviewAgain || !job.reviewed));
}

function App() {
  const [connections, setConnections] = useState<ApiConnection[]>([]);
  const [tagPresets, setTagPresets] = useState<TagPreset[]>([]);
  const [revisionPresets, setRevisionPresets] = useState<RevisionPreset[]>([]);
  const [tagPreset, setTagPreset] = useState<TagPreset>(createTagPreset);
  const [revisionPreset, setRevisionPreset] = useState<RevisionPreset>(createRevisionPreset);
  const [mode, setMode] = useState<JobKind>("caption");
  const [view, setView] = useState<View>("tasks");
  const [jobs, setJobs] = useState<ImageJob[]>([]);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [filter, setFilter] = useState<"all" | "succeeded" | "revised" | "refused" | "failed">("all");
  const [sortKey, setSortKey] = useState<"name" | "revision" | "captionLength">("name");
  const [sortAsc, setSortAsc] = useState(true);
  const [query, setQuery] = useState("");
  const [reviewFilter, setReviewFilter] = useState<"all" | "reviewed" | "unreviewed">("all");
  const [recursive, setRecursive] = useState(false);
  const [overwrite, setOverwrite] = useState(false);
  const [reviewAgain, setReviewAgain] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [batchId, setBatchId] = useState("");
  const [sampleRunning, setSampleRunning] = useState(false);
  const [frozen, setFrozen] = useState<ApiSelectionSnapshot>();
  const [keyRuntime, setKeyRuntime] = useState<ApiKeyRuntimeState[]>([]);
  const [editOpen, setEditOpen] = useState(false);
  const [rulesOpen, setRulesOpen] = useState(false);
  const [detailId, setDetailId] = useState<string>();
  const [previewJob, setPreviewJob] = useState<ImageJob>();
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem("theme") as Theme) || "system");
  const [bootError, setBootError] = useState("");
  const [loading, setLoading] = useState(true);
  const [bootAttempt, setBootAttempt] = useState(0);
  const [historicalStats, setHistoricalStats] = useState<HistoricalStats>({ total: 0, succeeded: 0, refused: 0, failed: 0, inputTokens: 0, outputTokens: 0 });
  const [localModels, setLocalModels] = useState<LocalModelState[]>([]);

  // WebView2 TSF 已知 bug（tauri#15436）：含文本的输入框首次聚焦会冻结中文
  // 输入法（Windows 10/11）。挂载时用隐藏空 textarea 预初始化 TSF，规避冻结。
  useEffect(() => {
    if (!isTauri()) return;
    const probe = document.createElement("textarea");
    probe.style.position = "fixed";
    probe.style.opacity = "0";
    probe.style.pointerEvents = "none";
    probe.style.width = "1px";
    probe.style.height = "1px";
    probe.setAttribute("aria-hidden", "true");
    document.body.appendChild(probe);
    probe.focus();
    requestAnimationFrame(() => {
      probe.blur();
      probe.remove();
    });
  }, []);

  const preset = mode === "caption" ? tagPreset : revisionPreset;
  const presetList = mode === "caption" ? tagPresets : revisionPresets;
  const selectedJobs = jobs.filter((job) => job.selected);
  const targetJobs = selectedJobs.length ? selectedJobs : jobs;
  const searchedJobs = useMemo(() => searchJobs(jobs, query), [jobs, query]);
  const filteredJobs = useMemo(() => {
    let list = filter === "all" ? searchedJobs : filter === "revised" ? searchedJobs.filter((job) => (job.revisionCount ?? 0) > 0) : searchedJobs.filter((job) => job.status === filter);
    if (reviewFilter === "reviewed") list = list.filter((job) => job.reviewed);
    if (reviewFilter === "unreviewed") list = list.filter((job) => !job.reviewed);
    return list;
  }, [searchedJobs, filter, reviewFilter]);
  const visibleJobs = useMemo(() => sortJobs(filteredJobs, sortKey, sortAsc), [filteredJobs, sortKey, sortAsc]);
  const stats = useMemo(() => summarizeJobs(jobs), [jobs]);
  const running = Boolean(batchId);
  const detailJob = jobs.find((job) => job.id === detailId);

  function addLog(level: LogEntry["level"], scope: LogEntry["scope"], message: string) {
    setLogs((current) => [...current.slice(-999), { id: crypto.randomUUID(), timestamp: nowTime(), level, scope, message }]);
  }

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("theme", theme);
  }, [theme]);

  useEffect(() => {
    if (!isTauri() && import.meta.env.DEV) {
      const demoConnection: ApiConnection = {
        id: "demo-api", name: "视觉模型 API", provider: "openai_compatible", baseUrl: "https://api.example.com/v1",
        headers: {}, keyRefs: [{ id: "demo-key", label: "主 Key", masked: "••••A8F2" }, { id: "demo-key-2", label: "备用 Key", masked: "••••C19D" }],
        totalConcurrency: 4, perKeyConcurrency: 2, timeoutSeconds: 90,
        cachedModels: [{ id: "vision-model-pro", name: "Vision Model Pro" }, { id: "vision-model-fast", name: "Vision Model Fast" }], modelsCachedAt: Date.now()
      };
      const demoTag = { ...createTagPreset(), connectionId: demoConnection.id, modelId: "vision-model-pro" };
      const demoRevision = { ...createRevisionPreset(), connectionId: demoConnection.id, modelId: "vision-model-pro" };
      setConnections([demoConnection]); setTagPresets([demoTag]); setRevisionPresets([demoRevision]); setTagPreset(demoTag); setRevisionPreset(demoRevision);
      setJobs([
        { id: "demo-1", imagePath: "D:\\dataset\\portrait_001.png", captionPath: "D:\\dataset\\portrait_001.txt", fileName: "portrait_001.png", caption: "1girl, solo, long black hair, blue eyes, white dress, soft lighting, upper body", status: "succeeded", selected: false, metrics: { inputTokens: 864, outputTokens: 28, ttftMs: 642, tokensPerSecond: 31.4 } },
        { id: "demo-2", imagePath: "D:\\dataset\\street_002.webp", captionPath: "D:\\dataset\\street_002.txt", fileName: "street_002.webp", caption: "1boy, streetwear, city street, neon lights, night, looking at viewer", status: "running", selected: false },
        { id: "demo-3", imagePath: "D:\\dataset\\scene_003.jpg", captionPath: "D:\\dataset\\scene_003.txt", fileName: "scene_003.jpg", caption: "", status: "pending", selected: false }
      ]);
      setLogs([
        { id: "demo-log-1", timestamp: nowTime(), level: "success", scope: "system", message: "本地数据库和加密凭据库已加载" },
        { id: "demo-log-2", timestamp: nowTime(), level: "info", scope: "caption", message: "portrait_001.png 使用主 Key 完成请求" },
        { id: "demo-log-3", timestamp: nowTime(), level: "info", scope: "caption", message: "street_002.webp 正在接收流式响应" }
      ]);
      setLoading(false);
      return;
    }
    let active = true;
    setLoading(true);
    setBootError("");
    let timeoutId = 0;
    const timeout = new Promise<never>((_, reject) => {
      timeoutId = window.setTimeout(() => reject(new Error("初始化超过 15 秒，请点击重试；数据库维护已在后台运行")), 15_000);
    });
    Promise.race([backend.bootstrap(), timeout]).then((data) => {
      if (!active) return;
      setConnections(data.connections);
      const tags = data.tagPresets.length ? data.tagPresets : [createTagPreset()];
      const revisions = data.revisionPresets.length ? data.revisionPresets : [createRevisionPreset()];
      setTagPresets(tags);
      setRevisionPresets(revisions);
      setTagPreset({ ...tags[0], connectionId: tags[0].connectionId || data.connections[0]?.id || "" });
      setRevisionPreset({ ...revisions[0], connectionId: revisions[0].connectionId || data.connections[0]?.id || "" });
      addLog("success", "system", "本地数据库和加密凭据库已加载");
      void backend.queryStats().then(setHistoricalStats);
      void backend.listLocalModels().then(setLocalModels).catch(() => undefined);
    }).catch((reason) => {
      if (!active) return;
      setBootError(String(reason));
      addLog("error", "system", `初始化失败：${String(reason)}`);
    }).finally(() => {
      window.clearTimeout(timeoutId);
      if (active) setLoading(false);
    });
    return () => {
      active = false;
      window.clearTimeout(timeoutId);
    };
  }, [bootAttempt]);

  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    listen<TaskEvent>("task-event", ({ payload }) => {
      if (!shouldHandleMainTaskEvent(payload)) return;
      if (payload.type === "log" && payload.message) addLog(payload.level ?? "info", mode, payload.message);
      if (payload.job) {
        setJobs((current) => current.map((job) =>
          job.imagePath === payload.job!.imagePath ? mergeTaskJobUpdate(job, payload.job!) : job
        ));
      }
      if (payload.type === "completed") {
        setBatchId("");
        setFrozen(undefined);
        setKeyRuntime([]);
        if (payload.message) addLog(payload.message.includes("停止") ? "warn" : "success", mode, payload.message);
        void backend.queryStats().then(setHistoricalStats).catch(() => undefined);
      }
    }).then((unlisten) => { dispose = unlisten; });
    return () => dispose?.();
  }, [mode]);

  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    listen<LocalModelEvent>("local-model-event", ({ payload }) => {
      setLocalModels((current) => current.map((model) => model.id === payload.modelId
        ? { ...model, downloading: payload.status === "downloading", received: payload.received, total: payload.total || model.total, error: payload.error, installed: payload.status === "done" }
        : model));
      if (payload.status === "error") addLog("error", "system", `本地模型下载失败：${payload.error ?? "未知错误"}`);
      if (payload.status === "done") addLog("success", "system", `本地模型 ${payload.modelId} 下载完成`);
    }).then((unlisten) => { dispose = unlisten; });
    return () => dispose?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    listen<KeyRuntimeEvent>("key-runtime-event", ({ payload }) => {
      if (payload.batchId === batchId || running) setKeyRuntime(payload.keys);
    }).then((unlisten) => { dispose = unlisten; });
    return () => dispose?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [batchId]);

  function updateRule(index: number, patch: Partial<RefusalRule>) {
    patchPreset({ refusalRules: preset.refusalRules.map((rule, i) => i === index ? { ...rule, ...patch } : rule) });
  }

  function addRule() {
    patchPreset({ refusalRules: [...preset.refusalRules, { id: crypto.randomUUID(), pattern: "", regex: false, caseSensitive: false }] });
  }

  function removeRule(index: number) {
    patchPreset({ refusalRules: preset.refusalRules.filter((_, i) => i !== index) });
  }

  function patchPreset(patch: Partial<TagPreset | RevisionPreset>) {
    if (mode === "caption") setTagPreset((current) => ({ ...current, ...patch }));
    else setRevisionPreset((current) => ({ ...current, ...patch } as RevisionPreset));
  }

  function patchThinking(modeValue: ThinkingMode, budgetValue = preset.thinkingBudget ?? 4096) {
    const connection = connections.find((item) => item.id === preset.connectionId);
    patchPreset({
      thinkingMode: modeValue,
      thinkingBudget: budgetValue,
      requestParams: buildThinkingRequestParams(connection, preset.modelId, modeValue, budgetValue),
    });
  }

  function patchAdvancedModel(patch: Partial<NonNullable<TagPreset["advancedModel"]>>) {
    patchPreset({
      advancedModel: {
        enabled: false,
        connectionId: "",
        modelId: "",
        thinkingMode: "high",
        thinkingBudget: 8192,
        ...(preset.advancedModel ?? {}),
        ...patch,
      },
    });
  }

  function patchAdvancedThinking(modeValue: ThinkingMode, budgetValue = preset.advancedModel?.thinkingBudget ?? 8192) {
    const advanced = preset.advancedModel;
    const connection = connections.find((item) => item.id === advanced?.connectionId);
    patchAdvancedModel({
      thinkingMode: modeValue,
      thinkingBudget: budgetValue,
      requestParams: buildThinkingRequestParams(connection, advanced?.modelId ?? "", modeValue, budgetValue),
    });
  }

  async function loadThumbnailsChunked(items: ImageJob[]) {
    const pending = items.filter((job) => !job.thumbnail).map((job) => job.imagePath);
    for (let index = 0; index < pending.length; index += 40) {
      const chunk = pending.slice(index, index + 40);
      const thumbs = await backend.loadThumbnails(chunk).catch(() => ({} as Record<string, string>));
      setJobs((current) => current.map((job) => thumbs[job.imagePath] ? { ...job, thumbnail: thumbs[job.imagePath] } : job));
    }
  }

  async function importImages(kind: "single" | "multiple" | "folder") {
    try {
      const added = kind === "folder" ? await backend.selectFolder(recursive) : await backend.selectImages(kind === "multiple");
      const known = new Set(jobs.map((job) => job.imagePath.toLocaleLowerCase()));
      const unique = added.filter((job) => !known.has(job.imagePath.toLocaleLowerCase()));
      setJobs((current) => [...current, ...unique]);
      addLog("info", "system", `已加入 ${unique.length} 张图片${added.length !== unique.length ? `，忽略 ${added.length - unique.length} 个重复项` : ""}`);
      if (unique.length && isTauri()) void loadThumbnailsChunked(unique);
    } catch (reason) {
      if (!String(reason).toLocaleLowerCase().includes("cancel")) addLog("error", "system", `导入失败：${String(reason)}`);
    }
  }

  async function ignoreJobError(job: ImageJob) {
    try {
      await backend.ignoreReviewError(job.captionPath);
      setJobs((current) => current.map((item) => item.id === job.id ? { ...item, reviewed: true, errorIgnored: true, status: "failed" as const } : item));
      addLog("success", "system", `已忽略 ${job.fileName} 的复核错误，下次复核将跳过`);
    } catch (reason) {
      addLog("error", "system", `忽略失败：${String(reason)}`);
    }
  }

  function toggleExtraChannel(id: string) {
    const list = preset.connectionIds ?? [];
    const next = list.includes(id) ? list.filter((item) => item !== id) : [...list, id];
    patchPreset({ connectionIds: next });
  }

  async function saveCurrentPreset() {
    try {
      await backend.savePreset(mode === "caption" ? "tag" : "revision", preset);
      const connection = connections.find((item) => item.id === preset.connectionId);
      if (connection && preset.modelId && connection.lastModel !== preset.modelId) {
        const savedConnection = await backend.saveConnection({ ...connection, lastModel: preset.modelId });
        setConnections((current) => current.map((item) => item.id === savedConnection.id ? savedConnection : item));
      }
      if (mode === "caption") setTagPresets((list) => [...list.filter((p) => p.id !== tagPreset.id), tagPreset]);
      else setRevisionPresets((list) => [...list.filter((p) => p.id !== revisionPreset.id), revisionPreset]);
      addLog("success", "system", `预设“${preset.name}”已保存`);
    } catch (reason) { addLog("error", "system", `保存预设失败：${String(reason)}`); }
  }

  function duplicatePreset() {
    if (mode === "caption") setTagPreset({ ...tagPreset, id: crypto.randomUUID(), name: `${tagPreset.name} 副本` });
    else setRevisionPreset({ ...revisionPreset, id: crypto.randomUUID(), name: `${revisionPreset.name} 副本` });
  }

  function newPreset() {
    if (mode === "caption") setTagPreset({ ...createTagPreset(), connectionId: preset.connectionId, modelId: preset.modelId, timeoutSeconds: preset.timeoutSeconds, totalConcurrency: preset.totalConcurrency, perKeyConcurrency: preset.perKeyConcurrency });
    else setRevisionPreset({ ...createRevisionPreset(), connectionId: preset.connectionId, modelId: preset.modelId, timeoutSeconds: preset.timeoutSeconds, totalConcurrency: preset.totalConcurrency, perKeyConcurrency: preset.perKeyConcurrency });
  }

  async function deleteCurrentPreset() {
    if (!window.confirm(`确定删除预设“${preset.name}”？历史指标仍会保留。`)) return;
    try {
      await backend.deletePreset(mode === "caption" ? "tag" : "revision", preset.id);
      if (mode === "caption") {
        const remaining = tagPresets.filter((item) => item.id !== preset.id);
        setTagPresets(remaining);
        setTagPreset(remaining[0] ?? createTagPreset());
      } else {
        const remaining = revisionPresets.filter((item) => item.id !== preset.id);
        setRevisionPresets(remaining);
        setRevisionPreset(remaining[0] ?? createRevisionPreset());
      }
      addLog("warn", "system", `预设“${preset.name}”已删除`);
    } catch (reason) {
      addLog("error", "system", `删除预设失败：${String(reason)}`);
    }
  }

  async function start(override?: ImageJob[], retryOverride?: { connectionId?: string; modelId?: string; localModelId?: string }) {
    const candidates =
        override ?? (mode === "caption" ? targetJobs : revisionCandidates(targetJobs, reviewAgain));
    if (candidates.length === 0) {
        addLog("info", mode, "没有待复核的图片（已复核的默认跳过，可勾选“重新复核已复核”）");
        return;
    }
    const isLocal = retryOverride?.localModelId ? true : preset.source === "local";
    if (isLocal) {
      if (!(retryOverride?.localModelId ?? preset.localModelId)) return addLog("error", mode, "请先选择本地打标模型");
    } else {
      if (retryOverride) {
        const retryConnectionId = retryOverride.connectionId ?? preset.connectionId;
        const retryModelId = retryOverride.modelId ?? preset.modelId;
        if (!retryConnectionId.trim() || !retryModelId.trim()) return addLog("error", mode, "请为单图重试选择 API 连接和模型");
      } else {
        const active = effectiveChannels(preset).filter((c) => c.connectionId.trim());
        if (active.length === 0) return addLog("error", mode, "请至少启用一个渠道并选择模型");
        if (active.some((c) => !c.modelId.trim())) return addLog("error", mode, "请为每个启用的渠道选择模型");
      }
    }
    if (preset.advancedModel?.enabled && (!preset.advancedModel.connectionId.trim() || !preset.advancedModel.modelId.trim())) {
      return addLog("error", mode, "高级模型已启用，请选择 API 提供商连接和模型");
    }
    if (!candidates.length) return addLog("warn", mode, mode === "revision" ? "没有可复核的现有标签" : "请先添加图片");
    try {
      let snapshot = {
        ...buildSnapshot(preset, connections),
        ...(retryOverride ?? {}),
      };
      if (isLocal) {
        snapshot.connectionId = "";
        snapshot.connectionName = "本地模型";
        snapshot.modelId = retryOverride?.localModelId ?? preset.localModelId ?? "";
        snapshot.localModelId = retryOverride?.localModelId ?? preset.localModelId;
      } else if (retryOverride) {
        snapshot = applyApiRetryTarget(snapshot, connections, retryOverride);
      } else {
        snapshot.localModelId = undefined;
      }
      setJobs((current) => current.map((job) => candidates.some((c) => c.id === job.id) ? { ...job, status: "pending", error: undefined, refusal: undefined, keyLabel: undefined } : job));
      const id = await backend.startBatch({
        kind: mode,
        imagePaths: candidates.map((job) => job.imagePath),
        presetId: preset.id,
        selectionSnapshot: snapshot,
        overwrite: mode === "revision" || overwrite
      });
      setBatchId(id);
      setFrozen(snapshot);
      setKeyRuntime([]);
      void backend.getTaskRuntime(id).then((state) => state && setKeyRuntime(state.keys)).catch(() => undefined);
      addLog("info", mode, `批次已开始，共 ${candidates.length} 张图片`);
    } catch (reason) { addLog("error", mode, `启动失败：${String(reason)}`); }
  }

  function retryJobFromDrawer(job: ImageJob, target: { connectionId?: string; modelId?: string; localModelId?: string }) {
    void start([job], target);
  }

  async function stop() {
    if (!batchId) return;
    try { await backend.cancelBatch(batchId); addLog("warn", mode, "已请求停止当前批次"); }
    catch (reason) { addLog("error", mode, `停止失败：${String(reason)}`); }
  }

  async function saveConnection(connection: ApiConnection) {
    const saved = await backend.saveConnection(connection);
    setConnections((current) => [...current.filter((item) => item.id !== saved.id), saved]);
    if (!preset.connectionId) patchPreset({ connectionId: saved.id });
    addLog("success", "system", `API 连接“${saved.name}”已保存`);
  }

  async function deleteConnection(id: string) {
    try {
      await backend.deleteConnection(id);
      setConnections((current) => current.filter((item) => item.id !== id));
      if (preset.connectionId === id) patchPreset({ connectionId: "", modelId: "" });
    } catch (reason) {
      addLog("error", "system", `删除连接失败：${String(reason)}`);
    }
  }

  const applyEditResults = useCallback((batch: CaptionEditBatch) => {
    const byPath = new Map((batch.results ?? []).filter((item) => item.status === "applied").map((item) => [item.path, item]));
    if (byPath.size === 0 && !batch.previews.some((item) => item.changed)) {
      addLog("warn", "edit", "批量编辑：没有文件被修改");
      return;
    }
    setJobs((current) => current.map((job) => byPath.has(job.captionPath)
      ? { ...job, caption: batch.previews.find((p) => p.path === job.captionPath)?.after ?? job.caption, updatedAt: new Date().toISOString() }
      : job));
    const conflicts = batch.results?.filter((item) => item.status === "conflict").length ?? 0;
    addLog("success", "edit", `批量编辑完成，应用 ${byPath.size} 个文件${conflicts ? `，${conflicts} 个冲突已跳过` : ""}`);
  }, []);

  function metricValue(value: number | undefined, suffix = "") { return value == null ? "—" : `${value.toFixed(value >= 100 ? 0 : 1)}${suffix}`; }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand"><div className="brand-mark"><Tags size={21} /></div><div><b>Caption Studio</b><small>LoRA 数据集工作台</small></div></div>
        <nav>
          <button className={view === "sample" ? "active" : ""} disabled={running} title={running ? "请先停止当前打标任务" : undefined} onClick={() => setView("sample")}><Shuffle size={18} />抽选</button>
          <button className={view === "tasks" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("tasks")}><Layers3 size={18} />打标任务<span>{jobs.length}</span></button>
          <button className={view === "connections" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("connections")}><Settings2 size={18} />API 连接<span>{connections.length}</span></button>
          <button className={view === "history" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("history")}><History size={18} />版本历史</button>
          <button className={view === "postprocess" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("postprocess")}><ShieldCheck size={18} />模型后处理</button>
          <button className={view === "anima" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("anima")}><Calculator size={18} />Anima 参数</button>
          <button className={view === "stats" ? "active" : ""} disabled={sampleRunning} onClick={() => setView("stats")}><BarChart3 size={18} />使用统计</button>
        </nav>
        <div className="sidebar-bottom">
          <div className="theme-switcher"><button className={theme === "light" ? "active" : ""} onClick={() => setTheme("light")} aria-label="浅色主题"><Sun size={15} /></button><button className={theme === "system" ? "active" : ""} onClick={() => setTheme("system")} aria-label="跟随系统"><Activity size={15} /></button><button className={theme === "dark" ? "active" : ""} onClick={() => setTheme("dark")} aria-label="深色主题"><Moon size={15} /></button></div>
          <div className="privacy-note"><CheckCircle2 size={15} /><span>凭据在本机加密</span></div>
        </div>
      </aside>

      <main className="main-shell">
        <header className="topbar">
          <div><h1>{view === "tasks" ? "图像打标" : view === "sample" ? "图片抽选" : view === "connections" ? "API 连接" : view === "history" ? "版本历史" : view === "postprocess" ? "模型后处理" : view === "anima" ? "Anima 训练参数设计" : "使用统计"}</h1><p>{view === "tasks" ? "批量生成、修订并管理训练标签" : view === "sample" ? "随机抽取或交由视觉模型按要求逐图筛选" : view === "connections" ? "连接库与加密 Key 池，Base URL 自动补全" : view === "history" ? "所有覆盖操作均可恢复" : view === "postprocess" ? "彻底清除 LoRA 模型中的训练参数元数据" : view === "anima" ? "依据图片数量、LoRA 类型和指定 Batch Size 计算训练规模" : "按任务类型、批次、API、模型、预设与 Key 分组"}</p></div>
          {view === "tasks" && <div className="toolbar-actions">
            <button className="button ghost" onClick={() => void importImages("single")}><FilePlus2 size={16} />选择单张</button>
            <button className="button ghost" onClick={() => void importImages("multiple")}><ImagePlus size={16} />批量选择</button>
            <button className="button ghost" onClick={() => void importImages("folder")}><FolderOpen size={16} />选择文件夹</button>
          </div>}
        </header>

        {loading ? <div className="center-state"><Sparkles className="pulse" /><h2>正在准备工作区</h2></div> : bootError ? <div className="center-state error-state"><CircleAlert /><h2>初始化失败</h2><p>{bootError}</p><button className="button primary" onClick={() => setBootAttempt((value) => value + 1)}><RefreshCcw size={16} />重试初始化</button></div> : view === "sample" ? <SampleView connections={connections} onManageConnections={() => setView("connections")} onAiRunningChange={setSampleRunning} onStart={(jobs) => { setJobs((current) => [...current, ...jobs.filter((j) => !current.some((c) => c.imagePath.toLocaleLowerCase() === j.imagePath.toLocaleLowerCase()))]); setView("tasks"); }} onLog={(level, scope, message) => addLog(level, scope, message)} /> : view === "tasks" ? <div className="task-page">
          <section className="control-card">
            <div className="mode-row">
              <div className="segmented large"><button className={mode === "caption" ? "active" : ""} onClick={() => setMode("caption")}><WandSparkles size={16} />生成标签</button><button className={mode === "revision" ? "active" : ""} onClick={() => setMode("revision")}><RefreshCcw size={16} />模型复核</button></div>
              <div className="preset-control"><span>预设</span><select value={preset.id} onChange={(e) => { const found = presetList.find((p) => p.id === e.target.value); if (found) mode === "caption" ? setTagPreset(found as TagPreset) : setRevisionPreset(found as RevisionPreset); }}>{presetList.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select><input className="preset-name" aria-label="预设名称" value={preset.name} onChange={(e) => patchPreset({ name: e.target.value })} /><button className="icon-button mini" onClick={newPreset} title="新建预设" aria-label="新建预设"><Plus size={15} /></button><button className="icon-button mini" onClick={duplicatePreset} title="复制为新预设" aria-label="复制预设"><Copy size={15} /></button><button className="icon-button mini" onClick={() => void saveCurrentPreset()} title="保存预设" aria-label="保存预设"><Save size={15} /></button><button className="icon-button mini danger" onClick={() => void deleteCurrentPreset()} title="删除预设" aria-label="删除预设"><Trash2 size={15} /></button><button className={`button small advanced-toggle ${advancedOpen ? "active" : ""}`} onClick={() => setAdvancedOpen((open) => !open)} aria-expanded={advancedOpen}><SlidersHorizontal size={14} />高级设置</button></div>
            </div>
            <div className="api-row">
              <div className="source-cell">
                {mode === "caption" && (
                  <div className="segmented source-switch">
                    <button className={preset.source !== "local" ? "active" : ""} onClick={() => patchPreset({ source: "api" })} title="使用云端 LLM 视觉 API 打标">LLM API</button>
                    <button className={preset.source === "local" ? "active" : ""} onClick={() => patchPreset({ source: "local" })} title="使用本地 WD tagger 模型离线打标">WD tagger</button>
                  </div>
                )}
                {preset.source === "local" && mode === "caption" ? (
                  <LocalModelPicker models={localModels} selectedId={preset.localModelId ?? ""} disabled={running} onSelect={(id) => patchPreset({ localModelId: id, modelId: id })} onDownload={(id) => void backend.downloadLocalModel(id).catch((reason) => addLog("error", "system", `下载失败：${String(reason)}`))} onRefresh={() => void backend.listLocalModels().then(setLocalModels).catch(() => undefined)} />
                ) : (
                  <ChannelsEditor
                    connections={connections}
                    channels={effectiveChannels(preset)}
                    disabled={running}
                    onModelsRefreshed={(connectionId, models) => {
                      const connection = connections.find((c) => c.id === connectionId);
                      if (!connection) return;
                      const saved = { ...connection, cachedModels: models, modelsCachedAt: Date.now() };
                      void backend.saveConnection(saved).then((updated) => setConnections((cur) => cur.map((c) => c.id === updated.id ? updated : c))).catch(() => setConnections((cur) => cur.map((c) => c.id === saved.id ? saved : c)));
                    }}
                    onChange={(channels) => {
                      const first = channels.find((c) => c.connectionId.trim());
                      const nextConnection = connections.find((item) => item.id === first?.connectionId);
                      const nextModel = first?.modelId ?? "";
                      const thinkingMode = preset.thinkingMode ?? "default";
                      patchPreset({
                        channels,
                        connectionId: first?.connectionId ?? "",
                        modelId: nextModel,
                        requestParams: buildThinkingRequestParams(nextConnection, nextModel, thinkingMode, preset.thinkingBudget ?? 4096),
                      });
                    }}
                    onManage={() => setView("connections")}
                  />
                )}
              </div>
              <label className="prompt-field"><span>{mode === "caption" ? "系统提示词" : "复核提示词"}</span><textarea value={preset.systemPrompt} onChange={(e) => patchPreset({ systemPrompt: e.target.value })} /></label>
              <div className="run-controls">{mode === "revision" ? <label className="check compact"><input type="checkbox" checked={reviewAgain} onChange={(e) => setReviewAgain(e.target.checked)} />重新复核已复核</label> : <label className="check compact"><input type="checkbox" checked={overwrite} onChange={(e) => setOverwrite(e.target.checked)} />覆盖已有标签</label>}{running ? <button className="button stop" onClick={() => void stop()}><Square size={15} fill="currentColor" />停止</button> : <button className="button primary run" onClick={() => void start()}><Play size={16} fill="currentColor" />{mode === "caption" ? "开始打标" : "开始复核"}</button>}</div>
            </div>
            {advancedOpen && <div className="advanced-settings">
              <div className="adv-row params">
                <label>Temperature<input type="number" min={0} max={2} step={0.1} value={preset.temperature} onChange={(e) => patchPreset({ temperature: Number(e.target.value) })} /></label>
                <label>最大输出 Token<input type="number" min={16} max={32768} value={preset.maxTokens} onChange={(e) => patchPreset({ maxTokens: Number(e.target.value) })} /></label>
                <label>请求超时（秒）<input type="number" min={10} max={600} value={preset.timeoutSeconds} onChange={(e) => patchPreset({ timeoutSeconds: Number(e.target.value) })} /></label>
                <label>每 Key 并发<input type="number" min={1} max={8} value={preset.perKeyConcurrency} onChange={(e) => patchPreset({ perKeyConcurrency: Number(e.target.value) })} /></label>
              </div>
              {preset.source !== "local" && <div className="adv-row thinking-params">
                <span className="adv-label">模型思考</span>
                <label>思考额度<select aria-label={`${mode === "caption" ? "打标" : "复核"}思考额度`} value={preset.thinkingMode ?? "default"} onChange={(e) => patchThinking(e.target.value as ThinkingMode)}><option value="default">跟随连接默认</option><option value="off">关闭/最小</option><option value="minimal">极低</option><option value="low">低</option><option value="medium">中</option><option value="high">高</option><option value="xhigh">极高</option><option value="custom">自定义 Token</option></select></label>
                {(preset.thinkingMode ?? "default") === "custom" && <label>思考 Token<input aria-label={`${mode === "caption" ? "打标" : "复核"}自定义思考 Token`} type="number" min={0} max={131072} value={preset.thinkingBudget ?? 4096} onChange={(e) => { const budget = Math.min(131072, Math.max(0, Math.floor(Number(e.target.value)))); patchThinking("custom", budget); }} /></label>}
                <small>自动适配：{detectThinkingFamily(connections.find((item) => item.id === preset.connectionId), preset.modelId)}</small>
              </div>}
              <div className="adv-row advanced-model-settings">
                <span className="adv-label">高级模型</span>
                <label className="check compact advanced-model-enable"><input type="checkbox" checked={preset.advancedModel?.enabled ?? false} onChange={(e) => {
                  const connectionId = preset.advancedModel?.connectionId || preset.connectionId || connections[0]?.id || "";
                  const connection = connections.find((item) => item.id === connectionId);
                  const modelId = preset.advancedModel?.modelId || connection?.lastModel || connection?.cachedModels?.[0]?.id || "";
                  const thinkingMode = preset.advancedModel?.thinkingMode ?? "high";
                  const thinkingBudget = preset.advancedModel?.thinkingBudget ?? 8192;
                  patchAdvancedModel({ enabled: e.target.checked, connectionId, modelId, requestParams: buildThinkingRequestParams(connection, modelId, thinkingMode, thinkingBudget) });
                }} />长度骤减时自动审核</label>
                {(preset.advancedModel?.enabled ?? false) && <div className="advanced-model-body">
                  <ApiModelSelector
                    connections={connections}
                    connectionId={preset.advancedModel?.connectionId ?? ""}
                    modelId={preset.advancedModel?.modelId ?? ""}
                    disabled={running}
                    onConnectionChange={(connectionId) => {
                      const connection = connections.find((item) => item.id === connectionId);
                      const modelId = connection?.lastModel ?? connection?.cachedModels?.[0]?.id ?? "";
                      const thinkingMode = preset.advancedModel?.thinkingMode ?? "high";
                      const thinkingBudget = preset.advancedModel?.thinkingBudget ?? 8192;
                      patchAdvancedModel({ connectionId, modelId, requestParams: buildThinkingRequestParams(connection, modelId, thinkingMode, thinkingBudget) });
                    }}
                    onModelChange={(modelId) => {
                      const connection = connections.find((item) => item.id === preset.advancedModel?.connectionId);
                      const thinkingMode = preset.advancedModel?.thinkingMode ?? "high";
                      const thinkingBudget = preset.advancedModel?.thinkingBudget ?? 8192;
                      patchAdvancedModel({ modelId, requestParams: buildThinkingRequestParams(connection, modelId, thinkingMode, thinkingBudget) });
                    }}
                    onManage={() => setView("connections")}
                  />
                  <span className="advanced-provider">提供商：{connections.find((item) => item.id === preset.advancedModel?.connectionId)?.provider ?? "未选择"}</span>
                  <label>思考额度<select aria-label={`${mode === "caption" ? "打标" : "复核"}高级模型思考额度`} value={preset.advancedModel?.thinkingMode ?? "high"} onChange={(e) => patchAdvancedThinking(e.target.value as ThinkingMode)}><option value="default">跟随连接默认</option><option value="off">关闭/最小</option><option value="minimal">极低</option><option value="low">低</option><option value="medium">中</option><option value="high">高</option><option value="xhigh">极高</option><option value="custom">自定义 Token</option></select></label>
                  {(preset.advancedModel?.thinkingMode ?? "high") === "custom" && <label>思考 Token<input aria-label={`${mode === "caption" ? "打标" : "复核"}高级模型自定义思考 Token`} type="number" min={0} max={131072} value={preset.advancedModel?.thinkingBudget ?? 8192} onChange={(e) => patchAdvancedThinking("custom", Math.min(131072, Math.max(0, Math.floor(Number(e.target.value)))))} /></label>}
                  <small>仅在候选标签少于原标签一半时调用；审核失败时保留原文。</small>
                </div>}
              </div>
              {preset.source === "local" && (
                <div className="adv-row local-params">
                  <span className="adv-label">本地模型参数</span>
                  <label title="置信度阈值（0-1），低于此分数的标签被丢弃">阈值<input type="number" min={0} max={1} step={0.05} value={preset.localThreshold ?? 0.35} onChange={(e) => patchPreset({ localThreshold: Math.min(1, Math.max(0, Number(e.target.value))) })} /></label>
                  <label title="0 表示不限">最多标签<input type="number" min={0} max={500} value={preset.localMaxTags ?? 0} onChange={(e) => patchPreset({ localMaxTags: Math.max(0, Math.floor(Number(e.target.value))) })} /></label>
                  <label className="check compact"><input type="checkbox" checked={preset.localKeepUnderscores ?? false} onChange={(e) => patchPreset({ localKeepUnderscores: e.target.checked })} />保留下划线</label>
                </div>
              )}
              <div className="adv-row rules">
                <span className="adv-label">拒绝规则</span>
                {rulesOpen ? (
                  <div className="rules-panel">
                    <div className="builtin-rules">
                      <small>内置（始终生效，不可编辑）：</small>
                      {BUILTIN_RULE_INFO.map((rule) => (
                        <span className="rule-chip" key={rule.source} title={rule.source}>{rule.label}</span>
                      ))}
                    </div>
                    <div className="rules-list">
                      {preset.refusalRules.map((rule, index) => (
                        <div className="rule-row" key={rule.id}>
                          <input value={rule.pattern} onChange={(e) => updateRule(index, { pattern: e.target.value })} placeholder="拒绝关键词或正则表达式" />
                          <label className="check compact"><input type="checkbox" checked={rule.regex} onChange={(e) => updateRule(index, { regex: e.target.checked })} />正则</label>
                          <label className="check compact"><input type="checkbox" checked={rule.caseSensitive} onChange={(e) => updateRule(index, { caseSensitive: e.target.checked })} />大小写</label>
                          <button className="icon-button mini danger" onClick={() => removeRule(index)} aria-label="删除拒绝规则"><Trash2 size={14} /></button>
                        </div>
                      ))}
                      {preset.refusalRules.length === 0 && <span className="muted">暂无自定义规则；可补充特定模型或代理返回的固定文本</span>}
                    </div>
                    <button className="text-button" onClick={addRule}><Plus size={14} />添加自定义规则</button>
                  </div>
                ) : (
                  <span className="rules-summary">内置中英文拒绝检测 · 已配置 {preset.refusalRules.length} 条自定义规则</span>
                )}
                <button className="button small" onClick={() => setRulesOpen((open) => !open)}>{rulesOpen ? "收起" : "编辑自定义规则"}</button>
              </div>
            </div>}
          </section>

          {running && frozen && (
            <section className="frozen-banner">
              <div className="frozen-title"><Clock3 size={15} /><b>任务进行中 — 以下快照已冻结</b></div>
              <div className="frozen-chips">
                <span>连接：{frozen.connectionName || frozen.connectionId}{(frozen.connectionIds?.length ?? 0) > 1 ? `（${frozen.connectionIds!.length} 个渠道叠加）` : ""}</span>
                <span>模型：{frozen.modelId}</span>
                {frozen.advancedModel?.enabled && <span>高级审核：{frozen.advancedModel.modelId}</span>}
                <span>超时：{frozen.timeoutSeconds}s</span>
                <span>每 Key 并发：{frozen.perKeyConcurrency}</span>
                <span>覆盖：{overwrite || mode === "revision" ? "允许" : "保留"}</span>
              </div>
              <p>继续编辑配置只会影响下一批次，当前请求不受影响。</p>
            </section>
          )}

          {running && keyRuntime.length > 0 && (
            <section className="key-runtime">
              <div className="key-runtime-title"><KeyRound size={14} />Key 实时状态</div>
              <div className="key-runtime-list">
                {keyRuntime.map((key) => (
                  <article key={key.key.id} className={key.disabled ? "disabled" : ""}>
                    <b>{key.key.label}</b>
                    <span>{key.key.masked}</span>
                    <em>{key.active > 0 ? `${key.active} 活动` : "空闲"}</em>
                    <small>成功 {key.succeeded} · 失败 {key.failed}</small>
                    <small>Token {key.tokens.toLocaleString()}</small>
                    {key.cooldownRemainingMs != null && key.cooldownRemainingMs > 0 && <small className="cooldown">冷却 {(key.cooldownRemainingMs / 1000).toFixed(0)}s</small>}
                    {key.disabled && <small className="disabled-reason">{key.disableReason ?? "已停用"}</small>}
                  </article>
                ))}
              </div>
            </section>
          )}

          <section className="metric-grid task-metrics">
            <MetricCard icon={<FileImage />} label="图片进度" value={`${stats.succeeded + stats.refused + stats.failed + stats.skipped}/${stats.total}`} sub={`${stats.succeeded} 成功 · ${stats.refused} 拒绝`} tone="blue" />
            <MetricCard icon={<Zap />} label="Token 消耗" value={(stats.inputTokens + stats.outputTokens).toLocaleString()} sub={`${stats.inputTokens.toLocaleString()} 输入 · ${stats.outputTokens.toLocaleString()} 输出`} tone="neutral" />
            <MetricCard icon={<Activity />} label="平均速度" value={metricValue(stats.averageTokensPerSecond, " t/s")} sub="按已报告输出 Token" tone="green" />
            <MetricCard icon={<Clock3 />} label="平均首字延迟" value={metricValue(stats.averageTtftMs, " ms")} sub="首个可见文本片段" tone="orange" />
          </section>

          <TagFrequencyPanel jobs={jobs} />

          <section className="workspace-card">
            <header className="workspace-toolbar">
              <div className="workspace-toolbar-primary">
                <div className="filter-tabs"><button className={filter === "all" ? "active" : ""} onClick={() => setFilter("all")}>全部 {jobs.length}</button><button className={filter === "succeeded" ? "active" : ""} onClick={() => setFilter("succeeded")}>完成 {stats.succeeded}</button><button className={filter === "revised" ? "active" : ""} onClick={() => setFilter("revised")}>已修订 {stats.revised}</button><button className={filter === "refused" ? "active" : ""} onClick={() => setFilter("refused")}>拒绝 {stats.refused}</button><button className={filter === "failed" ? "active" : ""} onClick={() => setFilter("failed")}>错误 {stats.failed}</button></div>
                <div className="list-controls"><input className="search-box" aria-label="搜索图片" placeholder="搜索文件名、路径或打标内容" value={query} onChange={(e) => setQuery(e.target.value)} /><select className="sort-select" aria-label="复核筛选" value={reviewFilter} onChange={(e) => setReviewFilter(e.target.value as typeof reviewFilter)}><option value="all">全部复核状态</option><option value="reviewed">仅已复核</option><option value="unreviewed">仅未复核</option></select><select className="sort-select" aria-label="排序方式" value={sortKey} onChange={(e) => setSortKey(e.target.value as typeof sortKey)}><option value="name">文件名</option><option value="revision">修订次数</option><option value="captionLength">标签长度</option></select><button className="icon-button mini" title={sortAsc ? "升序" : "降序"} aria-label={sortAsc ? "升序" : "降序"} onClick={() => setSortAsc((v) => !v)}>{sortAsc ? <ArrowUp size={14} /> : <ArrowDown size={14} />}</button></div>
              </div>
              <div className="workspace-actions"><label className="check compact"><input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />包含子文件夹</label><span className="action-divider" /><button className="button ghost small" disabled={running || !selectedJobs.length} onClick={() => void start(selectedJobs)}><RotateCcw size={15} />重试选中</button><button className="button ghost small" disabled={running || !visibleJobs.length} onClick={() => void start(visibleJobs)}><RotateCcw size={15} />重试筛选</button><button className="button ghost small" disabled={running || !jobs.some((job) => job.status === "failed" || job.status === "refused")} onClick={() => void start(jobs.filter((job) => job.status === "failed" || job.status === "refused"))}><RotateCcw size={15} />重试异常</button><button className="button secondary small" disabled={!targetJobs.length} onClick={() => setEditOpen(true)}><Edit3 size={15} />批量编辑</button><button className="icon-button mini" disabled={!jobs.length || running} onClick={() => setJobs([])} title="清空列表" aria-label="清空列表"><Trash2 size={16} /></button></div>
            </header>
            <div className="selection-bar"><label className="check"><input type="checkbox" checked={visibleJobs.length > 0 && visibleJobs.every((job) => job.selected)} onChange={(e) => { const visibleIds = new Set(visibleJobs.map((job) => job.id)); setJobs((current) => current.map((job) => visibleIds.has(job.id) ? { ...job, selected: e.target.checked } : job)); }} />选择全部（当前筛选 {visibleJobs.length} 张）</label><span>{selectedJobs.length ? `已选 ${selectedJobs.length} 项，操作将仅作用于所选项` : "未选择条目，操作将作用于全部图片"}</span></div>
            <div className="job-list">
              {visibleJobs.length === 0 ? <div className="empty-state"><div className="empty-icon"><ImagePlus size={28} /></div><h2>{jobs.length ? "当前筛选条件下没有图片" : "添加图片开始打标"}</h2><p>支持单张、批量选择，或快速导入整个文件夹</p>{!jobs.length && <button className="button primary" onClick={() => void importImages("folder")}><FolderOpen size={16} />选择图片文件夹</button>}</div> : <Virtuoso data={visibleJobs} itemContent={(_, job) => <JobRow job={job} onSelect={(selected) => setJobs((current) => current.map((item) => item.id === job.id ? { ...item, selected } : item))} onPreview={() => setPreviewJob(job)} onOpen={() => setDetailId(job.id)} onIgnoreError={ignoreJobError} />} />}
            </div>
          </section>
          <LogPanel logs={logs} onClear={() => setLogs([])} />
        </div> : view === "connections" ? <ConnectionsView connections={connections} onSave={saveConnection} onDelete={deleteConnection} onLog={addLog} /> : view === "history" ? <HistoryView onOpen={(id) => setDetailId(id)} onLog={addLog} /> : view === "postprocess" ? <PostprocessView /> : view === "anima" ? <AnimaTrainingDesigner currentDatasetCount={jobs.length} /> : <StatsView connections={connections} presets={tagPresets} />}
      </main>

      <BatchEditDialog
        open={editOpen}
        selectedPaths={selectedJobs.map((job) => job.captionPath)}
        filteredPaths={visibleJobs.map((job) => job.captionPath)}
        allPaths={jobs.map((job) => job.captionPath)}
        counts={{ selected: selectedJobs.length, filtered: visibleJobs.length, all: jobs.length }}
        onOpenChange={setEditOpen}
        onApplied={applyEditResults}
      />
      <DetailDrawer job={detailJob} connections={connections} defaultConnectionId={preset.connectionId} defaultModelId={preset.modelId} localMode={preset.source === "local"} localModelId={preset.localModelId} onClose={() => setDetailId(undefined)} onSaved={(caption) => setJobs((current) => current.map((job) => job.id === detailId ? { ...job, caption } : job))} onRetry={retryJobFromDrawer} />
      <ImageLightbox path={previewJob?.imagePath} name={previewJob?.fileName} thumbnail={previewJob?.thumbnail} onClose={() => setPreviewJob(undefined)} />
    </div>
  );
}

function MetricCard({ icon, label, value, sub, tone }: { icon: React.ReactNode; label: string; value: string; sub: string; tone: string }) {
  return <article className={`metric-card ${tone}`}><div className="metric-icon">{icon}</div><div><span>{label}</span><b>{value}</b><small>{sub}</small></div></article>;
}

function JobRow({ job, onSelect, onPreview, onOpen, onIgnoreError }: { job: ImageJob; onSelect: (selected: boolean) => void; onPreview: () => void; onOpen: () => void; onIgnoreError: (job: ImageJob) => void }) {
  return <article className={`job-row ${job.selected ? "selected" : ""}`}><label className="row-check"><input type="checkbox" checked={job.selected} onChange={(e) => onSelect(e.target.checked)} aria-label={`选择 ${job.fileName}`} /></label><button className="thumb-button" onClick={onPreview} aria-label={`查看大图 ${job.fileName}`} title="点击查看大图">{job.thumbnail ? <img src={job.thumbnail} alt="" /> : <div className="thumb-placeholder"><FileImage size={20} /></div>}</button><button className="job-main" onClick={onOpen}><span className="job-title"><b>{job.fileName}</b>{job.reviewed ? <em className="reviewed-pill">{job.errorIgnored ? "已忽略" : "已复核"}</em> : null}{(job.revisionCount ?? 0) > 0 ? <em className="rev-count-pill">修订×{job.revisionCount}</em> : null}{job.status === "failed" && (job.error?.includes("大幅减少") || job.error?.includes("提示词被服务端拒绝")) ? <button className="button mini" onClick={(e) => { e.stopPropagation(); onIgnoreError(job); }}>{job.errorIgnored ? "已忽略" : "忽略错误"}</button> : null}<em className={`status-pill ${job.status}`}>{statusLabel[job.status]}</em></span><span className="caption-preview">{job.error && job.errorKind ? <em className={`error-kind ${job.errorKind}`}>{ERROR_KIND_LABELS[job.errorKind] ?? job.errorKind}</em> : null}{job.caption || job.error || (job.status === "pending" ? "等待处理…" : "尚未生成标签")}</span><small>{job.imagePath}</small></button><div className="job-metrics"><span>{job.metrics?.outputTokens == null ? "—" : `${job.metrics.outputTokens} tokens`}</span><span>{job.metrics?.tokensPerSecond == null ? "—" : `${job.metrics.tokensPerSecond.toFixed(1)} t/s`}</span></div><button className="icon-button" onClick={onOpen} aria-label="查看详情"><ChevronRight size={17} /></button></article>;
}

const ERROR_KIND_LABELS: Record<string, string> = {
  rate_limit: "限流",
  auth: "认证失败",
  quota: "额度用尽",
  transient: "网络错误",
  prompt_rejected: "提示词被拒",
  truncated: "响应截断",
  length_reduced: "长度骤减",
  empty_reply: "空响应",
  write_error: "写入失败",
  no_key: "无可用 Key",
  download_failed: "模型下载失败",
  fatal: "请求失败",
  cancelled: "已取消",
};

const SOURCE_LABELS: Record<string, string> = {
  manual: "手动编辑",
  batch_edit: "批量编辑",
  revision: "模型修订",
  undo: "撤销恢复",
};

export function HistoryView({ onOpen, onLog }: { onOpen: (id: string) => void; onLog: (level: LogEntry["level"], scope: LogEntry["scope"], message: string) => void }) {
  const [versions, setVersions] = useState<CaptionVersion[]>([]);
  const [editBatches, setEditBatches] = useState<EditBatchRecord[]>([]);
  const [busy, setBusy] = useState(false);
  const [undoResult, setUndoResult] = useState<UndoBatchResult>();
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());

  const reload = useCallback(() => {
    void backend.listVersions().then(setVersions).catch(() => setVersions([]));
    void backend.listEditBatches().then(setEditBatches).catch(() => setEditBatches([]));
  }, []);

  useEffect(() => { if (isTauri()) reload(); }, [reload]);

  async function restore(version: CaptionVersion) {
    if (!window.confirm(`恢复 ${version.captionPath} 到该版本的内容？当前内容将被覆盖并记录为新的历史版本。`)) return;
    setBusy(true);
    try {
      await backend.restoreVersion(version.id);
      onLog("success", "edit", `已恢复版本：${version.captionPath}`);
      reload();
    } catch (reason) {
      onLog("error", "edit", `恢复失败：${String(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  async function restoreMany(picked: CaptionVersion[]) {
    const targets = dedupeRestoreTargets(picked);
    if (!window.confirm(`恢复所选 ${targets.length} 个文件的版本？当前内容将被覆盖并记录为新的历史版本。`)) return;
    setBusy(true);
    let ok = 0;
    let failed = 0;
    try {
      for (const version of targets) {
        try {
          await backend.restoreVersion(version.id);
          ok += 1;
        } catch (reason) {
          failed += 1;
          onLog("error", "edit", `恢复失败：${version.captionPath}：${String(reason)}`);
        }
      }
      if (ok > 0) onLog("success", "edit", `已批量恢复 ${ok} 个版本${failed ? `，失败 ${failed} 个` : ""}`);
      setSelectedIds(new Set());
      reload();
    } finally {
      setBusy(false);
    }
  }

  async function undoBatch(batchId: string) {
    if (!window.confirm(`撤销批次 ${batchId.slice(0, 8)}… 的全部修改？仅恢复仍等于该批次新内容的文件，外部修改过的文件将保留并列出。`)) return;
    setBusy(true);
    try {
      const result = await backend.undoBatch(batchId);
      setUndoResult(result);
      onLog("success", "edit", `批次撤销完成：恢复 ${result.restored.length} 个，冲突 ${result.conflicts.length} 个`);
      reload();
    } catch (reason) {
      onLog("error", "edit", `撤销失败：${String(reason)}`);
    } finally {
      setBusy(false);
    }
  }

  if (!isTauri()) {
    return <section className="page-card"><div className="page-card-header"><History /><div><h2>最近修改</h2><p>演示模式下无历史数据</p></div></div></section>;
  }

  const batchIds = Array.from(new Set(versions.map((v) => v.batchId).filter(Boolean) as string[]))
    .filter((id) => !editBatches.some((batch) => batch.id === id));

  return (
    <div className="stats-page">
      <section className="page-card">
        <div className="page-card-header"><History /><div><h2>全局版本历史</h2><p>从数据库加载，不依赖当前图片列表；可恢复单文件任意版本或撤销整批</p></div><button className="button ghost small" onClick={reload}><RefreshCcw size={14} />刷新</button></div>
        {editBatches.length > 0 && <div className="batch-list">{editBatches.map((batch) => (
          <article key={batch.id} className="batch-record">
            <div><b>批量编辑批次</b><small>{new Date(batch.createdAt).toLocaleString()} · 影响 {batch.affected} · 跳过 {batch.skipped} · 错误 {batch.errors} · {batch.applied ? "已应用" : "未应用"}</small></div>
            <span className="rule-chip">{batch.rule.mode} / {batch.rule.action}{batch.rule.content ? ` / ${batch.rule.content.slice(0, 24)}` : ""}</span>
            {batch.applied && <button className="button ghost small danger" disabled={busy} onClick={() => void undoBatch(batch.id)}><RotateCcw size={14} />撤销整批</button>}
          </article>
        ))}</div>}
        {undoResult && <div className="undo-summary"><b>撤销结果</b><span>恢复 {undoResult.restored.length} 个</span>{undoResult.skipped.length > 0 && <span>跳过 {undoResult.skipped.length}</span>}{undoResult.conflicts.length > 0 && <span className="conflict">冲突保留 {undoResult.conflicts.length} 个（已被外部修改）</span>}{undoResult.errors.length > 0 && <span className="conflict">错误 {undoResult.errors.length}</span>}</div>}
        <div className="history-toolbar">
          <label className="check compact"><input type="checkbox" checked={versions.length > 0 && selectedIds.size === versions.length} onChange={(e) => setSelectedIds(e.target.checked ? new Set(versions.map((v) => v.id)) : new Set())} />选择全部</label>
          <button className="button small" disabled={busy || selectedIds.size === 0} onClick={() => void restoreMany(versions.filter((v) => selectedIds.has(v.id)))}><RotateCcw size={14} />恢复所选 ({selectedIds.size})</button>
        </div>
        <div className="history-list">
          {versions.length === 0 ? <div className="empty-state compact"><History size={28} /><h2>暂无修改历史</h2></div> : versions.map((version) => (
            <article key={version.id} className={`history-entry ${selectedIds.has(version.id) ? "selected" : ""}`}>
              <label className="row-check"><input type="checkbox" checked={selectedIds.has(version.id)} onChange={(e) => setSelectedIds((current) => { const next = new Set(current); if (e.target.checked) next.add(version.id); else next.delete(version.id); return next; })} aria-label={`选择版本 ${version.id}`} /></label>
              <div className="history-main">
                <b>{version.captionPath}</b>
                <small>{new Date(version.createdAt).toLocaleString()} · {SOURCE_LABELS[version.source] ?? version.source}{version.batchId ? ` · 批次 ${version.batchId.slice(0, 8)}…` : ""}</small>
                <p><em>{version.oldContent || "（空）"}</em> <ChevronRight size={13} /> <em className="after">{version.newContent || "（空）"}</em></p>
              </div>
              <button className="button ghost small" disabled={busy} onClick={() => void restore(version)}><RotateCcw size={14} />恢复此版本</button>
            </article>
          ))}
        </div>
        {versions.length >= 500 && <p className="muted note">仅显示最近 500 条记录</p>}
      </section>
      {batchIds.length > 0 && (
        <section className="page-card">
          <div className="page-card-header"><Layers3 /><div><h2>按批次撤销</h2><p>任务批次产生的版本按批次组织，可整批撤销</p></div></div>
          <div className="batch-list">{batchIds.map((id) => (
            <article key={id} className="batch-record">
              <div><b>批次 {id.slice(0, 8)}…</b><small>{versions.filter((v) => v.batchId === id).length} 条版本记录</small></div>
              <button className="button ghost small danger" disabled={busy} onClick={() => void undoBatch(id)}><RotateCcw size={14} />撤销整批</button>
            </article>
          ))}</div>
        </section>
      )}
    </div>
  );
}

const GROUP_LABELS: Record<StatsGroupBy, string> = {
  kind: "任务类型",
  batch: "批次",
  connection: "API 连接",
  model: "模型",
  preset: "预设",
  key: "API Key",
};

export function StatsView({ connections, presets }: { connections: ApiConnection[]; presets: TagPreset[] }) {
  const [groupBy, setGroupBy] = useState<StatsGroupBy>("kind");
  const [breakdown, setBreakdown] = useState<StatsBreakdown>();
  const [error, setError] = useState("");
  const [connectionFilter, setConnectionFilter] = useState("");

  useEffect(() => {
    if (!isTauri()) return;
    backend.queryStatsBreakdown({ groupBy, connectionId: connectionFilter || undefined })
      .then(setBreakdown)
      .catch((reason) => setError(String(reason)));
  }, [groupBy, connectionFilter]);

  return (
    <div className="stats-page">
      <section className="page-card stats-summary">
        <div className="page-card-header"><BarChart3 /><div><h2>历史汇总</h2><p>当前设备上所有已完成 API 请求</p></div></div>
        <section className="metric-grid">
          <MetricCard icon={<CheckCircle2 />} label="历史成功" value={String(breakdown?.summary.succeeded ?? "—")} sub={`共 ${breakdown?.summary.total ?? "—"} 个请求`} tone="green" />
          <MetricCard icon={<CircleAlert />} label="拒绝与失败" value={String((breakdown?.summary.refused ?? 0) + (breakdown?.summary.failed ?? 0))} sub={`${breakdown?.summary.refused ?? 0} 拒绝 · ${breakdown?.summary.failed ?? 0} 失败`} tone="orange" />
          <MetricCard icon={<Zap />} label="历史 Token" value={((breakdown?.summary.inputTokens ?? 0) + (breakdown?.summary.outputTokens ?? 0)).toLocaleString()} sub={`${breakdown?.summary.inputTokens ?? 0} 输入 · ${breakdown?.summary.outputTokens ?? 0} 输出`} tone="purple" />
          <MetricCard icon={<Clock3 />} label="历史平均 TTFT" value={breakdown?.summary.averageTtftMs == null ? "—" : `${breakdown.summary.averageTtftMs.toFixed(0)} ms`} sub={breakdown?.summary.averageTokensPerSecond == null ? "Token 速度未提供" : `${breakdown.summary.averageTokensPerSecond.toFixed(1)} t/s`} tone="blue" />
        </section>
      </section>
      <section className="page-card">
        <div className="page-card-header"><Activity /><div><h2>分组统计</h2><p>按任务类型、批次、API、模型、预设与 Key 汇总指标</p></div><div className="stats-filters">
          <select aria-label="分组维度" value={groupBy} onChange={(e) => setGroupBy(e.target.value as StatsGroupBy)}>{Object.entries(GROUP_LABELS).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select>
          <select aria-label="连接筛选" value={connectionFilter} onChange={(e) => setConnectionFilter(e.target.value)}><option value="">全部连接</option>{connections.map((connection) => <option key={connection.id} value={connection.id}>{connection.name}</option>)}</select>
        </div></div>
        {error && <div className="inline-error">{error}</div>}
        {!breakdown ? <div className="empty-state compact"><BarChart3 size={28} /><h2>暂无指标数据</h2></div> : breakdown.rows.length === 0 ? <div className="empty-state compact"><BarChart3 size={28} /><h2>该分组下没有记录</h2></div> : (
          <div className="breakdown-table">
            <div className="breakdown-row header"><span>分组</span><span>请求</span><span>成功</span><span>拒绝</span><span>失败</span><span>输入 Token</span><span>输出 Token</span><span>平均 TTFT</span><span>平均速度</span></div>
            {breakdown.rows.map((row) => (
              <div className="breakdown-row" key={`${row.group}-${row.label}`}>
                <span><b>{row.label || row.group}</b>{row.label !== row.group && <small>{row.group}</small>}</span>
                <span>{row.total}</span><span className="ok">{row.succeeded}</span><span className="warn">{row.refused}</span><span className="bad">{row.failed}</span>
                <span>{row.inputTokens.toLocaleString()}</span><span>{row.outputTokens.toLocaleString()}</span>
                <span>{row.averageTtftMs == null ? "未提供" : `${row.averageTtftMs.toFixed(0)} ms`}</span>
                <span>{row.averageTokensPerSecond == null ? "未提供" : `${row.averageTokensPerSecond.toFixed(1)} t/s`}</span>
              </div>
            ))}
          </div>
        )}
      </section>
      <section className="page-card">
        <div className="page-card-header"><Settings2 /><div><h2>API 连接概览</h2><p>Key 池与服务地址</p></div></div>
        <div className="connection-stats">{connections.map((connection) => <article key={connection.id}><span className={`provider-dot ${connection.provider}`} /><div><b>{connection.name}</b><small>{connection.baseUrl}</small></div><em>{connection.keyRefs.length} Keys</em><span>{connection.timeoutSeconds}s 超时</span></article>)}</div>
      </section>
    </div>
  );
}

export default App;
