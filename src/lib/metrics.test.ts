import { describe, expect, it } from "vitest";
import { summarizeJobs } from "./metrics";
import type { ImageJob } from "../types";

const job = (status: ImageJob["status"], ttftMs?: number): ImageJob => ({
  id: crypto.randomUUID(), imagePath: "x.png", captionPath: "x.txt", fileName: "x.png", caption: "", selected: false, status,
  metrics: { inputTokens: 10, outputTokens: 5, ttftMs, tokensPerSecond: ttftMs ? 12 : undefined }
});

describe("summarizeJobs", () => {
  it("aggregates status, usage and measured averages", () => {
    const stats = summarizeJobs([job("succeeded", 100), job("refused", 300), job("failed")]);
    expect(stats.succeeded).toBe(1);
    expect(stats.refused).toBe(1);
    expect(stats.inputTokens).toBe(30);
    expect(stats.averageTtftMs).toBe(200);
  });
});

describe("本地打标不参与首字延迟统计", () => {
  it("本地模型任务（keyLabel 本地模型）的 TTFT 不计入均值", () => {
    const jobs: ImageJob[] = [
      { id: "a", imagePath: "a.png", captionPath: "a.txt", fileName: "a.png", caption: "1girl", status: "succeeded", selected: false, keyLabel: "本地模型", metrics: { inputTokens: undefined, outputTokens: undefined, ttftMs: 322, tokensPerSecond: undefined } },
      { id: "b", imagePath: "b.png", captionPath: "b.txt", fileName: "b.png", caption: "1boy", status: "succeeded", selected: false, keyLabel: "渠道A/k1", metrics: { inputTokens: 10, outputTokens: 2, ttftMs: 100, tokensPerSecond: 20 } },
    ];
    const stats = summarizeJobs(jobs);
    expect(stats.averageTtftMs).toBe(100);
    expect(stats.averageTokensPerSecond).toBe(20);
  });
});
