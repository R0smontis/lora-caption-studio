import type { DashboardStats, ImageJob } from "../types";

export function summarizeJobs(jobs: ImageJob[]): DashboardStats {
  // 首字延迟与速度指标仅统计 API 请求：本地 WD tagger 的任务（keyLabel 以"本地模型"开头）不参与。
  const apiJobs = jobs.filter((j) => !(j.keyLabel ?? "").startsWith("本地模型"));
  const measuredTtft = apiJobs.flatMap((j) => j.metrics?.ttftMs == null ? [] : [j.metrics.ttftMs]);
  const measuredSpeed = apiJobs.flatMap((j) => j.metrics?.tokensPerSecond == null ? [] : [j.metrics.tokensPerSecond]);
  return {
    total: jobs.length,
    succeeded: jobs.filter((j) => j.status === "succeeded" || j.status === "revised").length,
    revised: jobs.filter((j) => (j.revisionCount ?? 0) > 0).length,
    refused: jobs.filter((j) => j.status === "refused").length,
    failed: jobs.filter((j) => j.status === "failed").length,
    skipped: jobs.filter((j) => j.status === "skipped").length,
    cancelled: jobs.filter((j) => j.status === "cancelled").length,
    inputTokens: jobs.reduce((sum, j) => sum + (j.metrics?.inputTokens ?? 0), 0),
    outputTokens: jobs.reduce((sum, j) => sum + (j.metrics?.outputTokens ?? 0), 0),
    averageTtftMs: measuredTtft.length ? measuredTtft.reduce((a, b) => a + b, 0) / measuredTtft.length : undefined,
    averageTokensPerSecond: measuredSpeed.length ? measuredSpeed.reduce((a, b) => a + b, 0) / measuredSpeed.length : undefined
  };
}
