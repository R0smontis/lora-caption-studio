import { describe, expect, it } from "vitest";
import type { ApiConnection } from "../types";
import { buildThinkingRequestParams, detectThinkingFamily } from "./thinking";

const connection = (provider: ApiConnection["provider"], baseUrl: string): ApiConnection => ({
  id: "c", name: "C", provider, baseUrl, headers: {}, keyRefs: [], totalConcurrency: 2, perKeyConcurrency: 1, timeoutSeconds: 90, cachedModels: [],
});

describe("thinking request adapters", () => {
  it("maps native providers to their supported thinking fields", () => {
    expect(buildThinkingRequestParams(connection("anthropic", "https://api.anthropic.com"), "claude", "medium", 0)).toEqual({ thinking: { type: "enabled", budget_tokens: 4096 }, max_tokens: 4352 });
    expect(buildThinkingRequestParams(connection("gemini", "https://generativelanguage.googleapis.com"), "gemini-2.5-flash", "off", 0)).toEqual({ generationConfig: { thinkingConfig: { thinkingBudget: 0 } } });
    expect(buildThinkingRequestParams(connection("gemini", "https://generativelanguage.googleapis.com"), "gemini-3-flash", "high", 0)).toEqual({ generationConfig: { thinkingConfig: { thinkingLevel: "high" } } });
    expect(buildThinkingRequestParams(connection("gemini", "https://generativelanguage.googleapis.com"), "gemini-3-flash", "custom", 6000)).toEqual({ generationConfig: { thinkingConfig: { thinkingLevel: "medium" } } });
  });

  it("detects OpenAI-compatible model families", () => {
    const qwen = connection("openai_compatible", "https://dashscope.aliyuncs.com/compatible-mode/v1");
    expect(detectThinkingFamily(qwen, "qwen3-vl-plus")).toBe("通义千问/QwQ");
    expect(buildThinkingRequestParams(qwen, "qwen3-vl-plus", "custom", 6000)).toEqual({ enable_thinking: true, thinking_budget: 6000 });
    expect(buildThinkingRequestParams(connection("openai_compatible", "https://api.openai.com/v1"), "o3", "xhigh", 0)).toEqual({ reasoning_effort: "xhigh" });
  });
});
