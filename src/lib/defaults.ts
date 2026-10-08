import type { ApiConnection, CaptionEditRule, RevisionPreset, TagPreset } from "../types";

export const DEFAULT_TAG_PROMPT = `你是专业的图像数据集打标助手。请只输出适合 LoRA 训练的英文逗号分隔标签，不要解释。准确描述主体、数量、外观、服饰、姿态、构图、场景和光照，避免臆测不可见信息。`;

export const DEFAULT_REVISION_PROMPT = `你是 LoRA 数据集标签复核助手。请对照图片检查当前标签中的重复、错标、漏标、格式问题和无关内容。只输出完整的替换标签，不要解释，不要使用 Markdown。`;

export const createConnection = (): ApiConnection => ({
  id: crypto.randomUUID(),
  name: "OpenAI 兼容 API",
  provider: "openai_compatible",
  baseUrl: "https://api.openai.com/v1",
  headers: {},
  keyRefs: [{ id: crypto.randomUUID(), label: "默认 Key", secret: "" }],
  totalConcurrency: 2,
  perKeyConcurrency: 1,
  timeoutSeconds: 90
});

export const createTagPreset = (): TagPreset => ({
  id: crypto.randomUUID(),
  name: "通用 Booru 标签",
  source: "api",
  connectionId: "",
  connectionIds: [],
  modelId: "",
  localThreshold: 0.35,
  localMaxTags: 0,
  localKeepUnderscores: false,
  systemPrompt: DEFAULT_TAG_PROMPT,
  temperature: 0.2,
  maxTokens: 512,
  refusalRules: [],
  timeoutSeconds: 90,
  totalConcurrency: 2,
  perKeyConcurrency: 1,
  thinkingMode: "default",
  thinkingBudget: 4096,
  advancedModel: {
    enabled: false,
    connectionId: "",
    modelId: "",
    thinkingMode: "high",
    thinkingBudget: 8192,
  }
});

export const createRevisionPreset = (): RevisionPreset => ({
  ...createTagPreset(),
  name: "图片对照复核",
  systemPrompt: DEFAULT_REVISION_PROMPT
});

export const DEFAULT_EDIT_RULE: CaptionEditRule = {
  mode: "tag",
  action: "insert",
  position: "end",
  content: "",
  anchor: "",
  index: 1,
  replacement: "",
  regex: false,
  caseSensitive: false,
  allMatches: true,
  deduplicate: true,
  onlyIfMissing: false
};
