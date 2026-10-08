import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConnectionsView } from "./ConnectionsView";
import { backend } from "../lib/backend";
import type { ApiConnection } from "../types";

vi.mock("../lib/backend", () => ({
  backend: {
    testConnection: vi.fn(),
  },
}));

const connectionA: ApiConnection = {
  id: "conn-a", name: "连接 A", provider: "openai_compatible", baseUrl: "https://a.example",
  headers: {}, keyRefs: [{ id: "k1", label: "主 Key", masked: "••••A8F2" }],
  totalConcurrency: 2, perKeyConcurrency: 1, timeoutSeconds: 90,
  cachedModels: [{ id: "vision-pro", name: "Vision Pro" }], modelsCachedAt: Date.now(),
};
const connectionB: ApiConnection = {
  ...connectionA, id: "conn-b", name: "连接 B", baseUrl: "https://b.example/v1",
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

function renderView(overrides: Partial<Parameters<typeof ConnectionsView>[0]> = {}) {
  return render(
    <ConnectionsView
      connections={[connectionA, connectionB]}
      onSave={vi.fn()}
      onDelete={vi.fn()}
      onLog={vi.fn()}
      {...overrides}
    />
  );
}

describe("ConnectionsView", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    window.__TAURI_INTERNALS__ = {};
    vi.mocked(backend.testConnection).mockResolvedValue({
      ok: true, latencyMs: 33, modelsFetched: 2, sampleModels: ["vision-pro"], error: undefined,
      effectiveBaseUrl: "https://a.example/v1",
    });
  });

  it("lists connections and selects one", () => {
    renderView();
    expect(screen.getByText("连接 A")).toBeInTheDocument();
    expect(screen.getByText("连接 B")).toBeInTheDocument();
    fireEvent.click(screen.getByText("连接 B"));
    expect((screen.getByDisplayValue("连接 B"))).toBeInTheDocument();
    expect(screen.getByDisplayValue("https://b.example/v1")).toBeInTheDocument();
  });

  it("keeps concurrency controls out of connection settings", () => {
    renderView();
    expect(screen.queryByText("总并发")).not.toBeInTheDocument();
    expect(screen.queryByText("每 Key 并发")).not.toBeInTheDocument();
    expect(screen.getByText("连接测试与请求超时（秒）")).toBeInTheDocument();
  });

  it("saves the edited connection", async () => {
    const onSave = vi.fn().mockResolvedValue(connectionA);
    renderView({ onSave });
    const nameInput = screen.getByDisplayValue("连接 A");
    fireEvent.change(nameInput, { target: { value: "连接 A 改名" } });
    fireEvent.click(screen.getByRole("button", { name: /保存连接/ }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ name: "连接 A 改名" })));
  });

  it("runs a connection test and surfaces the auto-completed base url", async () => {
    renderView();
    fireEvent.click(screen.getByRole("button", { name: /测试连接/ }));
    await waitFor(() => expect(backend.testConnection).toHaveBeenCalledWith("conn-a"));
    await waitFor(() => expect(screen.getByText(/连接正常 · 33 ms · 发现 2 个模型/)).toBeInTheDocument());
    await waitFor(() => expect(screen.getByDisplayValue("https://a.example/v1")).toBeInTheDocument());
  });

  it("reports test failures", async () => {
    vi.mocked(backend.testConnection).mockResolvedValue({
      ok: false, latencyMs: 10, modelsFetched: 0, sampleModels: [], error: "HTTP 401：unauthorized",
    });
    renderView();
    fireEvent.click(screen.getByRole("button", { name: /测试连接/ }));
    await waitFor(() => expect(screen.getByText(/连接失败：HTTP 401：unauthorized/)).toBeInTheDocument());
  });

  it("deletes the selected connection with confirmation", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const onDelete = vi.fn().mockResolvedValue(undefined);
    renderView({ onDelete });
    fireEvent.click(screen.getByRole("button", { name: /删除连接/ }));
    await waitFor(() => expect(confirm).toHaveBeenCalled());
    await waitFor(() => expect(onDelete).toHaveBeenCalledWith("conn-a"));
  });

  it("creates a new connection draft", () => {
    renderView();
    fireEvent.click(screen.getByRole("button", { name: /新建连接/ }));
    expect(screen.getByDisplayValue("OpenAI 兼容 API")).toBeInTheDocument();
  });

  it("fills base url and provider immediately when a preset is selected", () => {
    renderView();
    fireEvent.click(screen.getByRole("button", { name: /新建连接/ }));
    fireEvent.change(screen.getByLabelText("预设服务商"), { target: { value: "Google Gemini" } });
    // 选择即填充，无需再点击空白处。
    expect(screen.getByDisplayValue("https://generativelanguage.googleapis.com")).toBeInTheDocument();
    expect((screen.getByLabelText("供应商") as HTMLSelectElement).value).toBe("gemini");
    expect(screen.getByLabelText("预设服务商")).toHaveValue("");
  });

  it("applies provider-specific default params (reasoning) on fill", () => {
    renderView();
    fireEvent.click(screen.getByRole("button", { name: /新建连接/ }));
    fireEvent.change(screen.getByLabelText("预设服务商"), { target: { value: "OpenAI" } });
    const params = JSON.parse((screen.getByLabelText(/额外请求参数/) as HTMLTextAreaElement).value);
    expect(params).toEqual({ reasoning_effort: "medium" });
    // DeepSeek 同时注入 thinking 与 reasoning_effort。
    fireEvent.change(screen.getByLabelText("预设服务商"), { target: { value: "DeepSeek" } });
    const deepseek = JSON.parse((screen.getByLabelText(/额外请求参数/) as HTMLTextAreaElement).value);
    expect(deepseek.thinking).toEqual({ type: "enabled" });
    expect(deepseek.reasoning_effort).toBe("medium");
    // 通义千问注入 enable_thinking。
    fireEvent.change(screen.getByLabelText("预设服务商"), { target: { value: "阿里通义千问" } });
    const qwen = JSON.parse((screen.getByLabelText(/额外请求参数/) as HTMLTextAreaElement).value);
    expect(qwen.enable_thinking).toBe(true);
  });

  it("saves custom default params with the connection", async () => {
    const onSave = vi.fn().mockResolvedValue(connectionA);
    renderView({ onSave });
    fireEvent.change(screen.getByLabelText(/额外请求参数/), { target: { value: JSON.stringify({ reasoning_effort: "high" }) } });
    fireEvent.click(screen.getByRole("button", { name: /保存连接/ }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ defaultParams: { reasoning_effort: "high" } })));
  });

  it("rejects invalid default params json", async () => {
    const onSave = vi.fn().mockResolvedValue(connectionA);
    renderView({ onSave });
    fireEvent.change(screen.getByLabelText(/额外请求参数/), { target: { value: "{ not json" } });
    fireEvent.click(screen.getByRole("button", { name: /保存连接/ }));
    await waitFor(() => expect(screen.getByText("额外请求参数必须是 JSON 对象")).toBeInTheDocument());
    expect(onSave).not.toHaveBeenCalled();
  });

  it("keeps presets collapsed and never auto-creates connections", () => {
    renderView();
    const select = screen.getByLabelText("预设服务商");
    expect((select as HTMLSelectElement).selectedOptions[0].textContent).toContain("选择知名服务商");
    // 选项只显示服务商名称，不带说明注释。
    const options = Array.from((select as HTMLSelectElement).options).map((o) => o.textContent ?? "");
    expect(options).toContain("OpenAI");
    expect(options.every((o) => !o.includes("—"))).toBe(true);
    // 连接列表只包含用户自己创建的连接，没有预设服务商条目。
    expect(screen.getAllByRole("button").filter((b) => b.textContent?.includes("个 Key"))).toHaveLength(2);
  });
});
