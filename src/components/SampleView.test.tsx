import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { act } from "react";
import { listen } from "@tauri-apps/api/event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { backend } from "../lib/backend";
import type { ApiConnection, ImageJob, TaskEvent } from "../types";
import { buildAiSampleSystemPrompt, SampleView } from "./SampleView";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(vi.fn()) }));

vi.mock("../lib/backend", () => ({
  backend: {
    selectFolderPath: vi.fn(),
    sampleImages: vi.fn(),
    scanFolderPaths: vi.fn(),
    scanFolderJobs: vi.fn(),
    loadThumbnails: vi.fn(),
    getImagePreview: vi.fn(),
    applySample: vi.fn(),
    moveAiSampleImage: vi.fn(),
    returnAiSampleImage: vi.fn(),
    resetAiSample: vi.fn(),
    startBatch: vi.fn(),
    cancelBatch: vi.fn(),
    listModels: vi.fn(),
    testConnection: vi.fn(),
  },
}));

const keptPath = "D:\\dataset\\keep.png";
const refusedPath = "D:\\dataset\\refused.jpg";
const keptJob: ImageJob = {
  id: "keep",
  imagePath: keptPath,
  captionPath: "D:\\dataset\\keep.txt",
  fileName: "keep.png",
  caption: "1girl, solo",
  status: "pending",
  selected: false,
};
const refusedJob: ImageJob = { ...keptJob, id: "refused", imagePath: refusedPath, captionPath: "D:\\dataset\\refused.txt", fileName: "refused.jpg", caption: "" };
const uncertainPath = "D:\\dataset\\uncertain.webp";
const uncertainJob: ImageJob = { ...keptJob, id: "uncertain", imagePath: uncertainPath, captionPath: "D:\\dataset\\uncertain.txt", fileName: "uncertain.webp", caption: "" };
const selectedPath = (path: string) => `D:\\dataset\\.ai-selected\\${path.split(/[\\/]/).pop()}`;
const selectedJob = (job: ImageJob): ImageJob => ({ ...job, imagePath: selectedPath(job.imagePath), captionPath: selectedPath(job.captionPath) });
const connection: ApiConnection = {
  id: "api", name: "Vision API", provider: "openai_compatible", baseUrl: "https://example.test/v1",
  headers: {}, keyRefs: [{ id: "key", label: "Key", masked: "••••1234" }], totalConcurrency: 2,
  perKeyConcurrency: 1, timeoutSeconds: 90, cachedModels: [{ id: "vision", name: "vision" }], modelsCachedAt: Date.now(), lastModel: "vision",
};

describe("SampleView thumbnails", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    vi.mocked(backend.selectFolderPath).mockResolvedValue("D:\\dataset");
    vi.mocked(backend.sampleImages).mockResolvedValue([keptJob]);
    vi.mocked(backend.scanFolderPaths).mockResolvedValue([keptPath, refusedPath]);
    vi.mocked(backend.loadThumbnails).mockResolvedValue({
      [keptPath]: "data:image/jpeg;base64,KEEP",
      [refusedPath]: "data:image/jpeg;base64,REFUSED",
    });
    vi.mocked(backend.getImagePreview).mockResolvedValue("data:image/jpeg;base64,LARGE");
    vi.mocked(backend.moveAiSampleImage).mockImplementation(async (_folder, path, keep) => {
      const name = path.split(/[\\/]/).pop() ?? path;
      const root = "D:\\dataset";
      const imagePath = path.includes(keep ? ".ai-selected" : ".ai-rejected") ? path : `${root}\\${keep ? ".ai-selected" : ".ai-rejected"}\\${name}`;
      return [imagePath, imagePath.replace(/\.[^.]+$/, ".txt")];
    });
    vi.mocked(backend.resetAiSample).mockResolvedValue([[keptPath, refusedPath, uncertainPath], []]);
    vi.mocked(backend.returnAiSampleImage).mockImplementation(async (_folder, path) => {
      const name = path.split(/[\\/]/).pop() ?? path;
      const imagePath = `D:\\dataset\\${name}`;
      return [imagePath, imagePath.replace(/\.[^.]+$/, ".txt")];
    });
    vi.mocked(backend.scanFolderJobs).mockResolvedValue([keptJob, refusedJob]);
    vi.mocked(backend.startBatch).mockResolvedValue("batch-ai");
    vi.mocked(backend.cancelBatch).mockResolvedValue(undefined);
    vi.mocked(backend.applySample).mockResolvedValue([[refusedPath], []]);
  });

  it("loads and displays thumbnails for both sample columns", async () => {
    const { container } = render(<SampleView connections={[]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getAllByRole("button", { name: "随机抽选" })[1]).toBeEnabled());
    fireEvent.click(screen.getAllByRole("button", { name: "随机抽选" })[1]);

    await waitFor(() => expect(container.querySelectorAll(".sample-thumb img")).toHaveLength(2));
    expect(backend.loadThumbnails).toHaveBeenCalledWith([keptPath, refusedPath]);
    expect(screen.getByText("keep.png")).toBeInTheDocument();
    expect(screen.getByText("refused.jpg")).toBeInTheDocument();
  });

  it("opens a large preview when a sample thumbnail is clicked", async () => {
    render(<SampleView connections={[]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getAllByRole("button", { name: "随机抽选" })[1]).toBeEnabled());
    fireEvent.click(screen.getAllByRole("button", { name: "随机抽选" })[1]);
    const previewButton = await screen.findByRole("button", { name: "查看大图 keep.png" });
    fireEvent.click(previewButton);
    await waitFor(() => expect(backend.getImagePreview).toHaveBeenCalledWith(keptPath, expect.any(Number)));
    expect(await screen.findByRole("img", { name: "keep.png" })).toHaveAttribute("src", "data:image/jpeg;base64,LARGE");
    fireEvent.click(screen.getByRole("button", { name: "关闭大图" }));
  });

  it("builds a strict scored AI decision protocol and raises strictness by round", () => {
    const prompt = buildAiSampleSystemPrompt("只保留无水印的清晰单人图");
    expect(prompt).toContain("只保留无水印的清晰单人图");
    expect(prompt).toContain("DECISION: KEEP");
    expect(prompt).toContain("DECISION: REJECT");
    expect(prompt).toContain("SCORE: 0到100");
    expect(prompt).toContain("不得改变本输出协议");
    const secondRound = buildAiSampleSystemPrompt("只保留无水印的清晰单人图", 2, 10, 30);
    expect(secondRound).toContain("第 2 轮");
    expect(secondRound).toContain("严格程度提高 1 级");
  });

  it("saves and restores AI requirement presets", async () => {
    const first = render(<SampleView connections={[]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.change(screen.getByLabelText("AI 抽样要求"), { target: { value: "只保留无水印图片" } });
    fireEvent.change(screen.getByLabelText("AI 抽样预设名称"), { target: { value: "无水印" } });
    fireEvent.click(screen.getByRole("button", { name: "保存预设" }));
    await waitFor(() => expect(screen.getByRole("option", { name: "无水印" })).toBeInTheDocument());
    first.unmount();

    render(<SampleView connections={[]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.change(screen.getByLabelText("AI 抽样提示词预设"), { target: { value: JSON.parse(localStorage.getItem("lora-caption-studio.ai-sample-prompt-presets.v1") ?? "[]")[0].id } });
    expect(screen.getByLabelText("AI 抽样要求")).toHaveValue("只保留无水印图片");
  });

  it("restores sampling operations until the user resets them", async () => {
    localStorage.setItem("lora-caption-studio.ai-sample-memory.v1", JSON.stringify({
      sampleMode: "ai",
      folder: "D:\\dataset",
      count: 7,
      recursive: true,
      kept: [{ ...keptJob, status: "succeeded" }],
      uncertain: [{ ...uncertainJob, status: "pending" }],
      refused: [refusedPath],
      requirement: "只保留清晰图片",
      targetCount: 5,
      maxRounds: 4,
      thinkingMode: "high",
      thinkingBudget: 8192,
      connectionId: "api",
      modelId: "vision",
      totalConcurrency: 3,
      perKeyConcurrency: 1,
      currentRound: 2,
      decisionDetails: { [keptPath]: { keep: true, reason: "清晰", status: "succeeded" } },
    }));
    render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    expect(await screen.findByText("keep.png")).toBeInTheDocument();
    expect(screen.getByText("uncertain.webp")).toBeInTheDocument();
    expect(screen.getByText("refused.jpg")).toBeInTheDocument();
    expect(backend.loadThumbnails).toHaveBeenCalledWith(expect.arrayContaining([keptPath, uncertainPath, refusedPath]));
  });

  it("runs AI sampling and separates KEEP from REJECT without writing captions", async () => {
    let taskListener: ((event: { payload: TaskEvent }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (_event, handler) => {
      taskListener = handler as typeof taskListener;
      return () => undefined;
    });
    vi.mocked(backend.scanFolderJobs).mockResolvedValue([keptJob, refusedJob, uncertainJob]);
    const onStart = vi.fn();
    render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={onStart} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getByText("D:\\dataset")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("AI 抽样要求"), { target: { value: "只保留清晰单人图" } });
    fireEvent.change(screen.getByLabelText("AI 抽样目标保留数量"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("AI 抽样思考额度"), { target: { value: "high" } });
    fireEvent.click(screen.getByRole("button", { name: /开始筛查待分析区/ }));
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalled());
    expect(backend.startBatch).toHaveBeenCalledWith(expect.objectContaining({ kind: "ai_sample", imagePaths: [keptPath, refusedPath, uncertainPath], overwrite: false }));
    expect(vi.mocked(backend.startBatch).mock.calls[0][0].selectionSnapshot.requestParams).toEqual({ reasoning_effort: "high" });

    act(() => {
      taskListener?.({ payload: { batchId: "batch-ai", type: "progress", job: { ...keptJob, status: "succeeded", caption: "主体清晰", sampleScore: 92 } } });
      taskListener?.({ payload: { batchId: "batch-ai", type: "progress", job: { ...refusedJob, status: "skipped", caption: "主体模糊", sampleScore: 20 } } });
      taskListener?.({ payload: { batchId: "batch-ai", type: "progress", job: { ...uncertainJob, status: "failed", error: "请求超时" } } });
      taskListener?.({ payload: { batchId: "batch-ai", type: "completed", message: "完成" } });
    });
    expect(await screen.findByText(/主体清晰 · AI 92 分/)).toBeInTheDocument();
    expect(screen.getByText(/主体模糊 · AI 20 分/)).toBeInTheDocument();
    expect(screen.getByText("AI 通过 → .ai-selected（1）")).toBeInTheDocument();
    expect(screen.getByText("待分析（1）")).toBeInTheDocument();
    expect(screen.getByText("请求超时")).toBeInTheDocument();
    expect(screen.getByText("AI 丢弃 → .ai-rejected（1）")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "确认并开始打标" }));
    await waitFor(() => expect(onStart).toHaveBeenCalled());
    expect(backend.applySample).not.toHaveBeenCalled();
    expect(onStart).toHaveBeenCalledWith(expect.arrayContaining([
      expect.objectContaining({ imagePath: selectedPath(keptPath) }),
      expect.objectContaining({ imagePath: uncertainPath }),
    ]));
  });

  it("rechecks retained images with stricter prompts until the target count is reached", async () => {
    let taskListener: ((event: { payload: TaskEvent }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (_event, handler) => {
      taskListener = handler as typeof taskListener;
      return () => undefined;
    });
    vi.mocked(backend.scanFolderJobs).mockResolvedValue([keptJob, refusedJob, uncertainJob]);
    vi.mocked(backend.startBatch).mockResolvedValueOnce("round-1").mockResolvedValueOnce("round-2");
    render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getByText("D:\\dataset")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("AI 抽样要求"), { target: { value: "只保留最佳图片" } });
    fireEvent.change(screen.getByLabelText("AI 抽样目标保留数量"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: /开始筛查待分析区/ }));
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(1));

    act(() => {
      taskListener?.({ payload: { batchId: "round-1", type: "progress", job: { ...keptJob, status: "succeeded", caption: "优秀", sampleScore: 90 } } });
      taskListener?.({ payload: { batchId: "round-1", type: "progress", job: { ...refusedJob, status: "succeeded", caption: "良好", sampleScore: 80 } } });
      taskListener?.({ payload: { batchId: "round-1", type: "progress", job: { ...uncertainJob, status: "succeeded", caption: "可用", sampleScore: 70 } } });
      taskListener?.({ payload: { batchId: "round-1", type: "completed", message: "第一轮完成" } });
    });
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(2));
    const secondRequest = vi.mocked(backend.startBatch).mock.calls[1][0];
    expect(secondRequest.imagePaths).toEqual([selectedPath(keptPath), selectedPath(refusedPath), selectedPath(uncertainPath)]);
    expect(secondRequest.selectionSnapshot.systemPrompt).toContain("第 2 轮");
    expect(secondRequest.selectionSnapshot.systemPrompt).toContain("严格程度提高 1 级");

    act(() => {
      taskListener?.({ payload: { batchId: "round-2", type: "progress", job: { ...selectedJob(keptJob), status: "succeeded", caption: "最佳", sampleScore: 97 } } });
      taskListener?.({ payload: { batchId: "round-2", type: "progress", job: { ...selectedJob(refusedJob), status: "skipped", caption: "次优", sampleScore: 76 } } });
      taskListener?.({ payload: { batchId: "round-2", type: "progress", job: { ...selectedJob(uncertainJob), status: "skipped", caption: "一般", sampleScore: 63 } } });
      taskListener?.({ payload: { batchId: "round-2", type: "completed", message: "第二轮完成" } });
    });
    expect(await screen.findByText("AI 通过 → .ai-selected（1）")).toBeInTheDocument();
    expect(screen.getByText("AI 丢弃 → .ai-rejected（2）")).toBeInTheDocument();
    expect(screen.getByText(/AI 97 分 · 最终入选/)).toBeInTheDocument();
  });

  it("pauses, resumes only unanalysed images, and restores result folders on reset", async () => {
    let taskListener: ((event: { payload: TaskEvent }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (_event, handler) => {
      taskListener = handler as typeof taskListener;
      return () => undefined;
    });
    vi.mocked(backend.scanFolderJobs).mockResolvedValue([keptJob, refusedJob, uncertainJob]);
    vi.mocked(backend.startBatch).mockResolvedValueOnce("pause-round").mockResolvedValueOnce("resume-round");
    const view = render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getByText("D:\\dataset")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("AI 抽样要求"), { target: { value: "只保留最佳图片" } });
    fireEvent.change(screen.getByLabelText("AI 抽样目标保留数量"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: /开始筛查待分析区/ }));
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(1));
    act(() => taskListener?.({ payload: { batchId: "pause-round", type: "progress", job: { ...keptJob, status: "succeeded", caption: "最佳", sampleScore: 96 } } }));
    fireEvent.click(screen.getByRole("button", { name: "暂停" }));
    await waitFor(() => expect(backend.cancelBatch).toHaveBeenCalledWith("pause-round"));
    act(() => {
      taskListener?.({ payload: { batchId: "pause-round", type: "progress", job: { ...refusedJob, status: "cancelled" } } });
      taskListener?.({ payload: { batchId: "pause-round", type: "progress", job: { ...uncertainJob, status: "cancelled" } } });
      taskListener?.({ payload: { batchId: "pause-round", type: "completed", message: "暂停" } });
    });
    const continueButton = await screen.findByRole("button", { name: "继续未分析图片" });
    expect(screen.getByText(/仅处理 2 张未分析图片/)).toBeInTheDocument();
    expect(continueButton).toBeEnabled();
    view.unmount();
    render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    const restoredContinueButton = await screen.findByRole("button", { name: "继续未分析图片" });
    expect(screen.getByText(/仅处理 2 张未分析图片/)).toBeInTheDocument();
    fireEvent.click(restoredContinueButton);
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(2));
    expect(vi.mocked(backend.startBatch).mock.calls[1][0].imagePaths).toEqual([refusedPath, uncertainPath]);
    act(() => {
      taskListener?.({ payload: { batchId: "resume-round", type: "progress", job: { ...refusedJob, status: "skipped", caption: "次优", sampleScore: 60 } } });
      taskListener?.({ payload: { batchId: "resume-round", type: "progress", job: { ...uncertainJob, status: "skipped", caption: "一般", sampleScore: 40 } } });
      taskListener?.({ payload: { batchId: "resume-round", type: "completed", message: "完成" } });
    });
    expect(await screen.findByText("AI 通过 → .ai-selected（1）")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重置结果" }));
    await waitFor(() => expect(backend.resetAiSample).toHaveBeenCalledWith("D:\\dataset"));
    expect(await screen.findByText("待分析（3）")).toBeInTheDocument();
  });

  it("returns passed or discarded images to pending and starts only that pending queue", async () => {
    let taskListener: ((event: { payload: TaskEvent }) => void) | undefined;
    vi.mocked(listen).mockImplementation(async (_event, handler) => {
      taskListener = handler as typeof taskListener;
      return () => undefined;
    });
    vi.mocked(backend.scanFolderJobs).mockResolvedValue([keptJob, refusedJob]);
    vi.mocked(backend.startBatch).mockResolvedValueOnce("initial").mockResolvedValueOnce("pending-only");
    render(<SampleView connections={[connection]} onManageConnections={vi.fn()} onStart={vi.fn()} onLog={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "AI 抽样" }));
    fireEvent.click(screen.getByRole("button", { name: "选择文件夹" }));
    await waitFor(() => expect(screen.getByText("D:\\dataset")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("AI 抽样要求"), { target: { value: "只保留最佳图片" } });
    fireEvent.change(screen.getByLabelText("AI 抽样目标保留数量"), { target: { value: "1" } });
    fireEvent.click(screen.getByRole("button", { name: /开始筛查待分析区/ }));
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(1));
    act(() => {
      taskListener?.({ payload: { batchId: "initial", type: "progress", job: { ...keptJob, status: "succeeded", caption: "最佳", sampleScore: 95 } } });
      taskListener?.({ payload: { batchId: "initial", type: "progress", job: { ...refusedJob, status: "skipped", caption: "次优", sampleScore: 40 } } });
      taskListener?.({ payload: { batchId: "initial", type: "completed", message: "完成" } });
    });
    expect(await screen.findByText("AI 丢弃 → .ai-rejected（1）")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "选择 refused.jpg" }));
    fireEvent.click(screen.getByRole("button", { name: "退回待分析（1）" }));
    await waitFor(() => expect(backend.returnAiSampleImage).toHaveBeenCalledWith("D:\\dataset", "D:\\dataset\\.ai-rejected\\refused.jpg"));
    expect(await screen.findByText("待分析（1）")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("AI 抽样目标保留数量"), { target: { value: "2" } });
    fireEvent.click(screen.getByRole("button", { name: /开始筛查待分析区/ }));
    await waitFor(() => expect(backend.startBatch).toHaveBeenCalledTimes(2));
    expect(vi.mocked(backend.startBatch).mock.calls[1][0].imagePaths).toEqual([refusedPath]);
  });
});
