export type ProviderKind = "openai_compatible" | "anthropic" | "gemini";
export type JobKind = "caption" | "revision" | "ai_sample";
export type JobStatus =
  | "pending"
  | "running"
  | "succeeded"
  | "revised"
  | "no_response"
  | "refused"
  | "failed"
  | "skipped"
  | "cancelled";

export interface ApiKeyInput {
  id?: string;
  label: string;
  secret?: string;
  masked?: string;
}

/** Non-secret key reference — id/label/mask only. */
export interface ApiKeyRef {
  id: string;
  label: string;
  masked: string;
}

export interface ApiConnection {
  id: string;
  name: string;
  provider: ProviderKind;
  baseUrl: string;
  headers: Record<string, string>;
  keyRefs: ApiKeyInput[];
  totalConcurrency: number;
  perKeyConcurrency: number;
  timeoutSeconds: number;
  lastModel?: string;
  cachedModels?: ModelOption[];
  modelsCachedAt?: number;
  /** 服务商预设注入的请求体额外参数（思考额度等），随请求体发送。 */
  defaultParams?: Record<string, unknown>;
}

export interface ModelOption {
  id: string;
  name: string;
  supportsVision?: boolean;
}

export interface RefusalRule {
  id: string;
  pattern: string;
  regex: boolean;
  caseSensitive: boolean;
}

export interface ApiChannel {
  connectionId: string;
  modelId: string;
}

export interface AdvancedModelConfig {
  enabled: boolean;
  connectionId: string;
  modelId: string;
  thinkingMode?: "default" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "custom";
  thinkingBudget?: number;
  requestParams?: Record<string, unknown>;
}

export type PresetSource = "api" | "local";

export interface TagPreset {
  id: string;
  name: string;
  /** "api" = 云端模型；"local" = 本地 WD Tagger 类模型 */
  source: PresetSource;
  connectionId: string;
  /** 叠加渠道：同一模型可同时使用的额外 API 连接（主连接恒在其中） */
  connectionIds?: string[];
  /** 渠道级选择：每渠道独立连接与模型（空项表示不使用） */
  channels?: ApiChannel[];
  modelId: string;
  localModelId?: string;
  /** 本地打标置信度阈值（0-1），默认 0.35 */
  localThreshold?: number;
  /** 本地打标最多输出标签数，0 表示不限 */
  localMaxTags?: number;
  /** 本地打标是否保留下划线（false 时 "_" 转空格） */
  localKeepUnderscores?: boolean;
  systemPrompt: string;
  temperature: number;
  maxTokens: number;
  refusalRules: RefusalRule[];
  timeoutSeconds: number;
  totalConcurrency: number;
  perKeyConcurrency: number;
  /** 本任务冻结的请求体覆盖参数；用于按模型调整思考等级/额度。 */
  requestParams?: Record<string, unknown>;
  /** 统一思考额度选择；保存到打标/复核预设。 */
  thinkingMode?: "default" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "custom";
  thinkingBudget?: number;
  /** 长度骤减时用于二次审核并生成最终完整标签的高级视觉模型。 */
  advancedModel?: AdvancedModelConfig;
}

export interface RevisionPreset extends Omit<TagPreset, "systemPrompt"> {
  systemPrompt: string;
}

/** Everything a task needs, frozen when the batch starts. */
export interface ApiSelectionSnapshot {
  connectionId: string;
  connectionName: string;
  connectionIds?: string[];
  channels?: ApiChannel[];
  modelId: string;
  localModelId?: string;
  localThreshold?: number;
  localMaxTags?: number;
  localKeepUnderscores?: boolean;
  systemPrompt: string;
  temperature: number;
  maxTokens: number;
  refusalRules: RefusalRule[];
  timeoutSeconds: number;
  totalConcurrency: number;
  perKeyConcurrency: number;
  /** 本任务冻结的请求体覆盖参数；用于按模型调整思考等级/额度。 */
  requestParams?: Record<string, unknown>;
  advancedModel?: AdvancedModelConfig;
}

export interface BatchRequest {
  kind: JobKind;
  imagePaths: string[];
  presetId?: string;
  selectionSnapshot: ApiSelectionSnapshot;
  overwrite: boolean;
}

export interface UsageMetrics {
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  ttftMs?: number;
  generationMs?: number;
  tokensPerSecond?: number;
}

export interface RefusalMatch {
  rule: string;
  excerpt: string;
  structured: boolean;
}

export interface ImageJob {
  id: string;
  imagePath: string;
  captionPath: string;
  fileName: string;
  thumbnail?: string;
  caption: string;
  status: JobStatus;
  selected: boolean;
  error?: string;
  refusal?: RefusalMatch;
  metrics?: UsageMetrics;
  keyLabel?: string;
  updatedAt?: string;
  /** 该图是否已复核完成（复核去重标记） */
  reviewed?: boolean;
  /** 复核错误已被用户手动忽略 */
  errorIgnored?: boolean;
  /** 失败原因分类（rate_limit/auth/quota/transient/truncated/…） */
  errorKind?: string;
  /** 被明确成功修订的次数（复核成功写入文件 +1） */
  revisionCount?: number;
  /** AI 抽样模型给出的 0–100 质量/匹配评分，用于循环筛选最终择优收敛。 */
  sampleScore?: number;
}

export interface CaptionVersion {
  id: number;
  captionPath: string;
  oldContent: string;
  newContent: string;
  source: "manual" | "batch_edit" | "revision" | "undo";
  batchId?: string;
  createdAt: string;
}

export type CaptionEditMode = "tag" | "text";
export type CaptionEditAction = "insert" | "delete" | "replace";
export type CaptionEditPosition =
  | "start"
  | "end"
  | "index"
  | "before_anchor"
  | "after_anchor"
  | "line";

export interface CaptionEditRule {
  mode: CaptionEditMode;
  action: CaptionEditAction;
  position: CaptionEditPosition;
  content: string;
  replacement?: string;
  anchor?: string;
  index?: number;
  regex: boolean;
  caseSensitive: boolean;
  allMatches: boolean;
  deduplicate: boolean;
  /** 仅当内容不包含指定文本时才插入（缺失检测） */
  onlyIfMissing?: boolean;
}

export interface EditPreview {
  path: string;
  before: string;
  after: string;
  changed: boolean;
  error?: string;
}

export interface EditApplyResult {
  path: string;
  status: "applied" | "skipped" | "conflict" | "error";
  error?: string;
}

export interface CaptionEditBatch {
  id: string;
  rule: CaptionEditRule;
  previews: EditPreview[];
  affected: number;
  skipped: number;
  errors: number;
  createdAt: string;
  applied: boolean;
  results?: EditApplyResult[];
}

export interface EditBatchRecord {
  id: string;
  rule: CaptionEditRule;
  affected: number;
  skipped: number;
  errors: number;
  applied: boolean;
  createdAt: string;
  appliedAt?: string;
}

export interface UndoBatchResult {
  batchId: string;
  restored: string[];
  skipped: string[];
  conflicts: string[];
  errors: [string, string][];
}

export interface TaskRun {
  id: string;
  kind: string;
  connectionId: string;
  connectionName: string;
  modelId: string;
  presetId?: string;
  status: string;
  total: number;
  succeeded: number;
  refused: number;
  failed: number;
  skipped: number;
  cancelled: number;
  inputTokens: number;
  outputTokens: number;
  startedAt: string;
  finishedAt?: string;
}

export interface ApiKeyRuntimeState {
  key: ApiKeyRef;
  active: number;
  succeeded: number;
  failed: number;
  tokens: number;
  cooldownRemainingMs?: number;
  disabled: boolean;
  disableReason?: string;
}

export interface TaskRuntimeState {
  batchId: string;
  status: string;
  total: number;
  running: number;
  finished: number;
  keys: ApiKeyRuntimeState[];
}

export interface KeyRuntimeEvent {
  batchId: string;
  keys: ApiKeyRuntimeState[];
}

export interface LocalModelState {
  id: string;
  name: string;
  sizeMb: number;
  installed: boolean;
  downloading: boolean;
  received: number;
  total: number;
  storagePath?: string;
  error?: string;
}

export interface LocalModelEvent {
  modelId: string;
  status: "downloading" | "done" | "error";
  received: number;
  total: number;
  error?: string;
}

export interface ConnectionTestResult {
  ok: boolean;
  latencyMs?: number;
  modelsFetched: number;
  sampleModels: string[];
  error?: string;
  /** Base URL that actually worked, when it differs from the entered one. */
  effectiveBaseUrl?: string;
}

export interface LogEntry {
  id: string;
  timestamp: string;
  level: "info" | "success" | "warn" | "error";
  scope: "system" | JobKind | "edit";
  message: string;
}

export interface TaskEvent {
  batchId: string;
  scope?: JobKind;
  type: "started" | "progress" | "completed" | "log";
  job?: ImageJob;
  message?: string;
  level?: LogEntry["level"];
}

export interface DashboardStats {
  total: number;
  succeeded: number;
  /** 被修订过（revisionCount > 0）的图片数 */
  revised: number;
  refused: number;
  failed: number;
  skipped: number;
  cancelled: number;
  inputTokens: number;
  outputTokens: number;
  averageTtftMs?: number;
  averageTokensPerSecond?: number;
}

export interface HistoricalStats {
  total: number;
  succeeded: number;
  refused: number;
  failed: number;
  inputTokens: number;
  outputTokens: number;
  averageTtftMs?: number;
  averageTokensPerSecond?: number;
}

export type StatsGroupBy = "kind" | "batch" | "connection" | "model" | "preset" | "key";

export interface StatsQuery {
  groupBy: StatsGroupBy;
  connectionId?: string;
  batchId?: string;
  modelId?: string;
  presetId?: string;
  keyId?: string;
}

export interface StatsBreakdownRow {
  group: string;
  label: string;
  total: number;
  succeeded: number;
  refused: number;
  failed: number;
  inputTokens: number;
  outputTokens: number;
  averageTtftMs?: number;
  averageTokensPerSecond?: number;
}

export interface StatsBreakdown {
  rows: StatsBreakdownRow[];
  summary: HistoricalStats;
}

export interface LoraMetadataReport {
  path: string;
  fileName: string;
  sizeBytes: number;
  metadataPresent: boolean;
  metadataCount: number;
  metadataKeys: string[];
  tensorCount: number;
  error?: string;
}

export interface LoraSanitizeResult {
  sourcePath: string;
  outputPath?: string;
  status: "sanitized" | "already_clean" | "error";
  removedCount: number;
  tensorDataVerified: boolean;
  error?: string;
}
