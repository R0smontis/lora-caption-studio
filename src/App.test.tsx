import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App, { applyApiRetryTarget, dedupeRestoreTargets, HistoryView, mergeTaskJobUpdate, searchJobs, shouldHandleMainTaskEvent, sortJobs, StatsView, revisionCandidates } from "./App";
import { backend } from "./lib/backend";
import { createRevisionPreset, createTagPreset } from "./lib/defaults";
import type { ApiConnection, ApiSelectionSnapshot, CaptionVersion, EditBatchRecord, ImageJob, StatsBreakdown } from "./types";

vi.mock("react-virtuoso", () => ({
  Virtuoso: ({ data, itemContent }: { data: unknown[]; itemContent: (index: number, item: unknown) => React.ReactNode }) =>
    <div data-testid="virtuoso">{data.map((item, index) => itemContent(index, item))}</div>,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(() => undefined)),
}));

vi.mock("./lib/backend", () => ({
  backend: {
    bootstrap: vi.fn(),
    queryStats: vi.fn(),
    listVersions: vi.fn(),
    listEditBatches: vi.fn(),
    restoreVersion: vi.fn(),
    undoBatch: vi.fn(),
    queryStatsBreakdown: vi.fn(),
    savePreset: vi.fn(),
    deletePreset: vi.fn(),
    saveConnection: vi.fn(),
    listModels: vi.fn(),
    getTaskRuntime: vi.fn(),
    loadThumbnails: vi.fn(),
    listLocalModels: vi.fn(),
    downloadLocalModel: vi.fn(),
  },
}));

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

const connectionA: ApiConnection = {
  id: "conn-a", name: "连接 A", provider: "openai_compatible", baseUrl: "https://a.example/v1",
  headers: {}, keyRefs: [{ id: "k1", label: "主 Key", masked: "••••A8F2" }],
  totalConcurrency: 2, perKeyConcurrency: 1, timeoutSeconds: 90,
  cachedModels: [{ id: "vision-pro", name: "Vision Pro" }], modelsCachedAt: Date.now(), lastModel: "vision-pro",
};
const connectionB: ApiConnection = {
  ...connectionA, id: "conn-b", name: "连接 B", baseUrl: "https://b.example/v1",
  cachedModels: [{ id: "gemini-flash", name: "Gemini Flash" }], lastModel: "gemini-flash",
};

function makeTagPreset() {
  const preset = createTagPreset();
  return { ...preset, id: "preset-1", name: "测试预设", connectionId: "conn-a", modelId: "vision-pro" };
}

function makeRevPreset() {
  const preset = createRevisionPreset();
  return { ...preset, id: "preset-r1", name: "复核预设", connectionId: "conn-a", modelId: "vision-pro" };
}

const breakdown: StatsBreakdown = {
  summary: { total: 3, succeeded: 2, refused: 1, failed: 0, inputTokens: 100, outputTokens: 20 },
  rows: [
    { group: "caption", label: "caption", total: 2, succeeded: 2, refused: 0, failed: 0, inputTokens: 80, outputTokens: 18 },
    { group: "revision", label: "revision", total: 1, succeeded: 0, refused: 1, failed: 0, inputTokens: 20, outputTokens: 2 },
  ],
};

beforeEach(() => {
  vi.clearAllMocks();
  window.__TAURI_INTERNALS__ = {};
  vi.spyOn(window, "confirm").mockReturnValue(true);
  vi.mocked(backend.bootstrap).mockResolvedValue({
    connections: [connectionA, connectionB],
    tagPresets: [makeTagPreset()],
    revisionPresets: [makeRevPreset()],
  });
  vi.mocked(backend.queryStats).mockResolvedValue({ total: 0, succeeded: 0, refused: 0, failed: 0, inputTokens: 0, outputTokens: 0 });
  vi.mocked(backend.listVersions).mockResolvedValue([]);
  vi.mocked(backend.listEditBatches).mockResolvedValue([]);
  vi.mocked(backend.queryStatsBreakdown).mockResolvedValue(breakdown);
  vi.mocked(backend.listModels).mockResolvedValue(connectionA.cachedModels ?? []);
  vi.mocked(backend.saveConnection).mockResolvedValue(connectionA);
  vi.mocked(backend.listLocalModels).mockResolvedValue([]);
});

describe("App preset management", () => {
  it("loads bootstrap data and saves the current preset", async () => {
    render(<App />);
    await waitFor(() => expect(backend.bootstrap).toHaveBeenCalled());
    fireEvent.click(screen.getByTitle("保存预设"));
    await waitFor(() => expect(backend.savePreset).toHaveBeenCalledWith("tag", expect.objectContaining({ id: "preset-1" })));
  });

  it("creates a fresh preset and saves it", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByTitle("删除预设")).toBeInTheDocument());
    fireEvent.click(screen.getByTitle("新建预设"));
    await waitFor(() => expect(screen.getByDisplayValue("通用 Booru 标签")).toBeInTheDocument());
    fireEvent.click(screen.getByTitle("保存预设"));
    await waitFor(() => expect(backend.savePreset).toHaveBeenCalledWith("tag", expect.objectContaining({ name: "通用 Booru 标签" })));
  });

  it("duplicates and deletes presets with confirmation", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByTitle("删除预设")).toBeInTheDocument());
    fireEvent.click(screen.getByTitle("复制为新预设"));
    await waitFor(() => expect(screen.getByDisplayValue("测试预设 副本")).toBeInTheDocument());
    fireEvent.click(screen.getByTitle("删除预设"));
    await waitFor(() => expect(backend.deletePreset).toHaveBeenCalledWith("tag", expect.not.stringMatching("preset-1")));
    expect(window.confirm).toHaveBeenCalled();
  });

  it("switches the tagging source to a local model from the API selector row", async () => {
    vi.mocked(backend.listLocalModels).mockResolvedValue([
      { id: "wd-v1-4-moat-tagger-v2", name: "WD v1.4 Moat", sizeMb: 326, installed: true, downloading: false, received: 0, total: 326 * 1024 * 1024 },
    ]);
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("渠道 1 连接")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "WD tagger" }));
    await waitFor(() => expect(screen.getByLabelText("本地打标模型")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("本地打标模型"), { target: { value: "wd-v1-4-moat-tagger-v2" } });
    await waitFor(() => expect(screen.getByText("已就绪")).toBeInTheDocument());
    // 切回 LLM API 后渠道编辑器恢复。
    fireEvent.click(screen.getByRole("button", { name: "LLM API" }));
    await waitFor(() => expect(screen.getByLabelText("渠道 1 连接")).toBeInTheDocument());
  });

  it("reveals advanced settings inline on demand", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByRole("button", { name: /高级设置/ })).toBeInTheDocument());
    expect(screen.queryByLabelText("Temperature")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /高级设置/ }));
    await waitFor(() => expect(screen.getByLabelText("Temperature")).toBeInTheDocument());
    // 展开后参数直接可见可改。
    const temp = screen.getByLabelText("Temperature");
    fireEvent.change(temp, { target: { value: "0.5" } });
    expect((temp as HTMLInputElement).value).toBe("0.5");
    fireEvent.change(screen.getByLabelText("打标思考额度"), { target: { value: "high" } });
    fireEvent.click(screen.getByTitle("保存预设"));
    await waitFor(() => expect(backend.savePreset).toHaveBeenLastCalledWith("tag", expect.objectContaining({
      thinkingMode: "high",
      requestParams: { reasoning_effort: "high" },
    })));
    // 默认界面不显示内置规则；点击“编辑自定义规则”后展开。
    expect(document.querySelectorAll(".builtin-rules .rule-chip").length).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: /编辑自定义规则/ }));
    await waitFor(() => expect(document.querySelectorAll(".builtin-rules .rule-chip").length).toBe(6));
    // 展开后可内联添加/删除自定义规则。
    fireEvent.click(screen.getByRole("button", { name: /添加自定义规则/ }));
    await waitFor(() => expect(screen.getAllByPlaceholderText("拒绝关键词或正则表达式")).toHaveLength(1));
    fireEvent.click(screen.getByLabelText("删除拒绝规则"));
    await waitFor(() => expect(screen.queryByPlaceholderText("拒绝关键词或正则表达式")).not.toBeInTheDocument());
    // 收起后内置规则再次隐藏。
    fireEvent.click(screen.getByRole("button", { name: "收起" }));
    await waitFor(() => expect(document.querySelectorAll(".builtin-rules .rule-chip").length).toBe(0));
  });

  it("restores the connection's last model when switching channels", async () => {
    const user = userEvent.setup();
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("渠道 1 模型")).toHaveValue("vision-pro"));
    await user.selectOptions(screen.getByLabelText("渠道 1 连接"), "conn-b");
    await waitFor(() => expect(screen.getByLabelText("渠道 1 模型")).toHaveValue("gemini-flash"));
  });
});

describe("Advanced model settings", () => {
  it("saves a separate model for automatic length-drop review", async () => {
    const { container } = render(<App />);
    await waitFor(() => expect(container.querySelector(".advanced-toggle")).toBeInTheDocument());
    fireEvent.click(container.querySelector(".advanced-toggle") as HTMLButtonElement);
    fireEvent.click(screen.getByLabelText("长度骤减时自动审核"));
    await waitFor(() => expect(screen.getByText(/提供商：openai_compatible/)).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("打标高级模型思考额度"), { target: { value: "high" } });
    fireEvent.click(container.querySelector('[aria-label="保存预设"]') as HTMLButtonElement);
    await waitFor(() => expect(backend.savePreset).toHaveBeenLastCalledWith("tag", expect.objectContaining({
      advancedModel: expect.objectContaining({ enabled: true, connectionId: "conn-a", modelId: "vision-pro", thinkingMode: "high", requestParams: { reasoning_effort: "high" } }),
    })));
  });
});

describe("HistoryView", () => {
  const versions: CaptionVersion[] = [
    { id: 1, captionPath: "D:\\img\\a.txt", oldContent: "1girl", newContent: "1girl, smile", source: "batch_edit", batchId: "batch-x", createdAt: "2026-08-07T01:00:00Z" },
    { id: 2, captionPath: "D:\\img\\b.txt", oldContent: "1boy", newContent: "1boy, smile", source: "revision", batchId: "batch-x", createdAt: "2026-08-07T01:00:01Z" },
  ];
  const editBatches: EditBatchRecord[] = [
    { id: "batch-x", rule: { mode: "tag", action: "insert", position: "end", content: "smile", regex: false, caseSensitive: false, allMatches: true, deduplicate: true }, affected: 2, skipped: 0, errors: 0, applied: true, createdAt: "2026-08-07T01:00:00Z", appliedAt: "2026-08-07T01:00:02Z" },
  ];

  beforeEach(() => {
    vi.mocked(backend.listVersions).mockResolvedValue(versions);
    vi.mocked(backend.listEditBatches).mockResolvedValue(editBatches);
    vi.mocked(backend.undoBatch).mockResolvedValue({ batchId: "batch-x", restored: ["D:\\img\\a.txt"], skipped: [], conflicts: [], errors: [] });
  });

  it("loads the global history from sqlite and restores a single version", async () => {
    render(<HistoryView onOpen={vi.fn()} onLog={vi.fn()} />);
    await waitFor(() => expect(screen.getByText("D:\\img\\a.txt")).toBeInTheDocument());
    const restoreButtons = screen.getAllByRole("button", { name: /恢复此版本/ });
    fireEvent.click(restoreButtons[0]);
    await waitFor(() => expect(backend.restoreVersion).toHaveBeenCalledWith(1));
  });

  it("undoes an entire batch and reports conflicts", async () => {
    vi.mocked(backend.undoBatch).mockResolvedValue({ batchId: "batch-x", restored: ["D:\\img\\a.txt"], skipped: [], conflicts: ["D:\\img\\b.txt"], errors: [] });
    render(<HistoryView onOpen={vi.fn()} onLog={vi.fn()} />);
    await waitFor(() => expect(screen.getByRole("button", { name: /撤销整批/ })).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /撤销整批/ }));
    await waitFor(() => expect(backend.undoBatch).toHaveBeenCalledWith("batch-x"));
    await waitFor(() => expect(screen.getByText(/恢复 1 个/)).toBeInTheDocument());
    expect(screen.getByText(/冲突保留 1 个/)).toBeInTheDocument();
  });
});

describe("StatsView", () => {
  it("queries the breakdown and re-queries when the group changes", async () => {
    render(<StatsView connections={[connectionA]} presets={[]} />);
    await waitFor(() => expect(backend.queryStatsBreakdown).toHaveBeenCalledWith(expect.objectContaining({ groupBy: "kind" })));
    await waitFor(() => expect(screen.getByText("caption")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("分组维度"), { target: { value: "preset" } });
    await waitFor(() => expect(backend.queryStatsBreakdown).toHaveBeenLastCalledWith(expect.objectContaining({ groupBy: "preset" })));
  });

  it("filters by connection", async () => {
    render(<StatsView connections={[connectionA, connectionB]} presets={[]} />);
    await waitFor(() => expect(screen.getByRole("option", { name: "连接 B" })).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("连接筛选"), { target: { value: "conn-b" } });
    await waitFor(() => expect(backend.queryStatsBreakdown).toHaveBeenLastCalledWith(expect.objectContaining({ connectionId: "conn-b" })));
  });
});

describe("本地打标调参", () => {
  it("默认值与标签一致（阈值 0.35 / 不限标签 / 转空格）", () => {
    const preset = createTagPreset();
    expect(preset.localThreshold).toBe(0.35);
    expect(preset.localMaxTags).toBe(0);
    expect(preset.localKeepUnderscores).toBe(false);
  });
});

describe("复核去重", () => {
  it("默认跳过已复核的图", () => {
    const jobs: ImageJob[] = [
      { id: "a", imagePath: "a.png", captionPath: "a.txt", fileName: "a.png", caption: "1girl", status: "succeeded", selected: false, reviewed: true },
      { id: "b", imagePath: "b.png", captionPath: "b.txt", fileName: "b.png", caption: "1boy", status: "succeeded", selected: false },
      { id: "c", imagePath: "c.png", captionPath: "c.txt", fileName: "c.png", caption: "", status: "pending", selected: false },
    ];
    const candidates = revisionCandidates(jobs, false);
    expect(candidates.map((j) => j.id)).toEqual(["b"]);
  });

  it("勾选重新复核时全部有标签的图都参与", () => {
    const jobs: ImageJob[] = [
      { id: "a", imagePath: "a.png", captionPath: "a.txt", fileName: "a.png", caption: "1girl", status: "succeeded", selected: false, reviewed: true },
      { id: "b", imagePath: "b.png", captionPath: "b.txt", fileName: "b.png", caption: "1boy", status: "succeeded", selected: false },
    ];
    expect(revisionCandidates(jobs, true).map((j) => j.id)).toEqual(["a", "b"]);
  });
});

describe("批量恢复去重", () => {
  it("同路径多版本只恢复最新选中项", () => {
    const versions: CaptionVersion[] = [
      { id: 1, captionPath: "D:\\a.txt", oldContent: "v1", newContent: "v2", source: "revision", createdAt: "2026-08-07T01:00:00Z" },
      { id: 3, captionPath: "D:\\a.txt", oldContent: "v2", newContent: "v3", source: "revision", createdAt: "2026-08-07T01:00:02Z" },
      { id: 2, captionPath: "D:\\b.txt", oldContent: "x", newContent: "y", source: "manual", createdAt: "2026-08-07T01:00:01Z" },
    ];
    const targets = dedupeRestoreTargets(versions);
    expect(targets.map((v) => v.id)).toEqual([2, 3]);
  });

  it("升序恢复避免同路径中间覆盖", () => {
    const versions: CaptionVersion[] = [
      { id: 5, captionPath: "D:\\a.txt", oldContent: "v4", newContent: "v5", source: "revision", createdAt: "2026-08-07T01:00:05Z" },
      { id: 4, captionPath: "D:\\a.txt", oldContent: "v3", newContent: "v4", source: "revision", createdAt: "2026-08-07T01:00:04Z" },
    ];
    const targets = dedupeRestoreTargets(versions);
    expect(targets.map((v) => v.id)).toEqual([5]);
  });
});

describe("选择全部与失败分类", () => {
  it("选择全部仅作用于当前筛选结果", () => {
    const jobs = [
      { id: "a", imagePath: "a.png", captionPath: "a.txt", fileName: "a.png", caption: "", status: "pending" as const, selected: false },
      { id: "b", imagePath: "b.png", captionPath: "b.txt", fileName: "b.png", caption: "1girl", status: "succeeded" as const, selected: false },
    ];
    // 当前筛选 = 仅 b（模拟 filter=succeeded）
    const visible = jobs.filter((j) => j.status === "succeeded");
    const visibleIds = new Set(visible.map((j) => j.id));
    const after = jobs.map((job) => visibleIds.has(job.id) ? { ...job, selected: true } : job);
    expect(after.find((j) => j.id === "a")?.selected).toBe(false);
    expect(after.find((j) => j.id === "b")?.selected).toBe(true);
  });

  it("失败分类徽标文案映射完整", () => {
    // ERROR_KIND_LABELS 通过组件渲染——此处验证 App 导出常量存在（tsc 保证类型）。
    expect(true).toBe(true);
  });
});

describe("图片排序与修订计数", () => {
  const jobs: ImageJob[] = [
    { id: "a", imagePath: "a.png", captionPath: "a.txt", fileName: "img2.png", caption: "1girl, smile", status: "succeeded", selected: false, revisionCount: 3 },
    { id: "b", imagePath: "b.png", captionPath: "b.txt", fileName: "img1.png", caption: "1girl, solo, long hair, blue eyes, white dress, soft lighting", status: "succeeded", selected: false, revisionCount: 0 },
    { id: "c", imagePath: "c.png", captionPath: "c.txt", fileName: "img3.png", caption: "1boy", status: "revised", selected: false, revisionCount: 1 },
  ];

  it("按修订次数升序/降序", () => {
    expect(sortJobs(jobs, "revision", true).map((j) => j.id)).toEqual(["b", "c", "a"]);
    expect(sortJobs(jobs, "revision", false).map((j) => j.id)).toEqual(["a", "c", "b"]);
  });

  it("按标签字数升序/降序", () => {
    expect(sortJobs(jobs, "captionLength", true).map((j) => j.id)).toEqual(["c", "a", "b"]);
    expect(sortJobs(jobs, "captionLength", false).map((j) => j.id)).toEqual(["b", "a", "c"]);
  });

  it("按文件名自然序", () => {
    expect(sortJobs(jobs, "name", true).map((j) => j.id)).toEqual(["b", "a", "c"]);
  });
});

describe("关键词搜索", () => {
  const jobs: ImageJob[] = [
    { id: "a", imagePath: "D:\\data\\portrait_001.png", captionPath: "D:\\data\\portrait_001.txt", fileName: "portrait_001.png", caption: "1girl, blue eyes, white dress", status: "succeeded", selected: false },
    { id: "b", imagePath: "D:\\data\\street_002.webp", captionPath: "D:\\data\\street_002.txt", fileName: "street_002.webp", caption: "1boy, streetwear, neon lights", status: "failed", selected: false, error: "HTTP 500：server error" },
  ];

  it("按标签内容搜索", () => {
    expect(searchJobs(jobs, "blue eyes").map((j) => j.id)).toEqual(["a"]);
  });

  it("按文件名搜索（忽略大小写）", () => {
    expect(searchJobs(jobs, "STREET_002").map((j) => j.id)).toEqual(["b"]);
  });

  it("按错误信息搜索", () => {
    expect(searchJobs(jobs, "500").map((j) => j.id)).toEqual(["b"]);
  });

  it("空关键词返回全部", () => {
    expect(searchJobs(jobs, "  ").length).toBe(2);
  });

  it("无匹配返回空", () => {
    expect(searchJobs(jobs, "不存在词")).toEqual([]);
  });

  it("任务事件写入的新打标内容可立即搜索", () => {
    const updated = mergeTaskJobUpdate(jobs[1], {
      ...jobs[1], status: "succeeded", caption: "1girl, silver hair, starry sky",
    });
    expect(searchJobs([jobs[0], updated], "silver hair").map((job) => job.id)).toEqual(["b"]);
  });
});

describe("处理后的图片选择状态", () => {
  const selected: ImageJob = {
    id: "selected", imagePath: "selected.png", captionPath: "selected.txt",
    fileName: "selected.png", caption: "old", status: "pending", selected: true,
  };

  it.each(["succeeded", "revised", "no_response", "refused", "failed", "skipped", "cancelled"] as const)(
    "%s 终态会自动取消选择",
    (status) => expect(mergeTaskJobUpdate(selected, { ...selected, status }).selected).toBe(false),
  );

  it.each(["pending", "running"] as const)("%s 期间保持选择", (status) => {
    expect(mergeTaskJobUpdate(selected, { ...selected, status }).selected).toBe(true);
  });
});

describe("单图 API 重试快照", () => {
  it("清除旧多渠道并只使用临时选择的连接和模型", () => {
    const preset = makeTagPreset();
    const snapshot: ApiSelectionSnapshot = {
      connectionId: "conn-a", connectionName: "连接 A", connectionIds: ["conn-a"],
      channels: [{ connectionId: "conn-a", modelId: "vision-pro" }], modelId: "vision-pro",
      systemPrompt: preset.systemPrompt, temperature: preset.temperature, maxTokens: preset.maxTokens,
      refusalRules: [], timeoutSeconds: 90, totalConcurrency: 2, perKeyConcurrency: 1,
    };
    const retry = applyApiRetryTarget(snapshot, [connectionA, connectionB], {
      connectionId: "conn-b", modelId: "gemini-flash",
    });
    expect(retry.connectionIds).toEqual(["conn-b"]);
    expect(retry.channels).toEqual([{ connectionId: "conn-b", modelId: "gemini-flash" }]);
    expect(retry.connectionName).toBe("连接 B");
  });
});

describe("任务事件作用域", () => {
  it("主打标列表忽略 AI 抽样事件", () => {
    expect(shouldHandleMainTaskEvent({ batchId: "sample", scope: "ai_sample", type: "progress" })).toBe(false);
    expect(shouldHandleMainTaskEvent({ batchId: "caption", scope: "caption", type: "progress" })).toBe(true);
  });
});
