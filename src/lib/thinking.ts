import type { ApiConnection } from "../types";

export type ThinkingMode = "default" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "custom";

const BUDGETS: Record<Exclude<ThinkingMode, "default" | "off" | "custom">, number> = {
  minimal: 1024,
  low: 2048,
  medium: 4096,
  high: 8192,
  xhigh: 16384,
};

export function detectThinkingFamily(connection: ApiConnection | undefined, modelId: string) {
  if (!connection) return "OpenAI 兼容";
  const target = `${connection.baseUrl} ${modelId}`.toLocaleLowerCase();
  if (connection.provider === "anthropic" || target.includes("claude")) return "Anthropic Claude";
  if (connection.provider === "gemini" || target.includes("gemini")) return target.includes("gemini-3") || target.includes("gemini-3.5") ? "Gemini 3" : "Gemini 2.5";
  if (target.includes("dashscope") || target.includes("qwen") || target.includes("qwq")) return "通义千问/QwQ";
  if (target.includes("deepseek")) return "DeepSeek";
  return "OpenAI 兼容/o 系列";
}

/** 将统一思考额度设置转换成供应商实际请求字段；返回值会覆盖连接的默认参数。 */
export function buildThinkingRequestParams(connection: ApiConnection | undefined, modelId: string, mode: ThinkingMode, customBudget: number): Record<string, unknown> | undefined {
  if (mode === "default" || !connection) return undefined;
  const family = detectThinkingFamily(connection, modelId);
  const budget = mode === "custom" ? Math.max(0, Math.floor(customBudget)) : mode === "off" ? 0 : BUDGETS[mode];

  if (family === "Anthropic Claude") {
    if (mode === "off") return { thinking: { type: "disabled" } };
    const tokens = Math.max(1024, budget);
    return { thinking: { type: "enabled", budget_tokens: tokens }, max_tokens: tokens + 256 };
  }
  if (family === "Gemini 3") {
    const customLevel = budget <= 1024 ? "minimal" : budget <= 3072 ? "low" : budget <= 8192 ? "medium" : "high";
    const level = mode === "custom" ? customLevel : mode === "off" ? "minimal" : mode === "xhigh" ? "high" : mode;
    return { generationConfig: { thinkingConfig: { thinkingLevel: level } } };
  }
  if (family === "Gemini 2.5") {
    return { generationConfig: { thinkingConfig: { thinkingBudget: mode === "off" ? 0 : budget } } };
  }
  if (family === "通义千问/QwQ") {
    return { enable_thinking: mode !== "off", ...(mode === "off" ? {} : { thinking_budget: budget }) };
  }
  if (family === "DeepSeek") {
    return { thinking: { type: mode === "off" ? "disabled" : "enabled" }, reasoning_effort: mode === "custom" ? "high" : mode === "minimal" ? "low" : mode };
  }
  return { reasoning_effort: mode === "custom" ? "high" : mode === "off" ? "none" : mode };
}
