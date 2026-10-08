import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ApiModelSelector } from "./ApiModelSelector";
import { backend } from "../lib/backend";
import type { ApiConnection, ModelOption } from "../types";

vi.mock("../lib/backend", () => ({
  backend: {
    listModels: vi.fn(),
    testConnection: vi.fn(),
  },
}));

const modelsA: ModelOption[] = [
  { id: "vision-pro", name: "Vision Pro" },
  { id: "vision-fast", name: "Vision Fast" },
];
const modelsB: ModelOption[] = [{ id: "gemini-flash", name: "Gemini Flash" }];

function connection(id: string, name: string, models?: ModelOption[]): ApiConnection {
  return {
    id, name, provider: "openai_compatible", baseUrl: "https://example.invalid/v1",
    headers: {}, keyRefs: [{ id: "k1", label: "主 Key", masked: "••••A8F2" }],
    totalConcurrency: 2, perKeyConcurrency: 1, timeoutSeconds: 90,
    cachedModels: models, modelsCachedAt: Date.now(), lastModel: models?.[0]?.id,
  };
}

function renderSelector(connections: ApiConnection[], connectionId: string, modelId: string, onChange = vi.fn()) {
  return render(
    <ApiModelSelector
      connections={connections}
      connectionId={connectionId}
      modelId={modelId}
      onConnectionChange={onChange}
      onModelChange={vi.fn()}
      onManage={vi.fn()}
    />
  );
}

describe("ApiModelSelector", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(backend.listModels).mockResolvedValue(modelsA);
    vi.mocked(backend.testConnection).mockResolvedValue({ ok: true, latencyMs: 42, modelsFetched: 2, sampleModels: ["vision-pro"], error: undefined });
  });

  it("shows the current connection name", () => {
    renderSelector([connection("a", "连接 A")], "a", "");
    expect(screen.getByText("连接 A")).toBeInTheDocument();
  });

  it("lists connections in the menu and fires onConnectionChange", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    renderSelector([connection("a", "连接 A"), connection("b", "连接 B")], "a", "", onChange);
    await user.click(screen.getByRole("button", { name: "选择 API 连接" }));
    await waitFor(() => expect(screen.getByText("连接 B")).toBeInTheDocument());
    await user.click(screen.getByText("连接 B"));
    expect(onChange).toHaveBeenCalledWith("b");
  });

  it("searches connections by name", async () => {
    const user = userEvent.setup();
    renderSelector([connection("a", "Alpha 连接"), connection("b", "Beta 连接")], "a", "");
    await user.click(screen.getByRole("button", { name: "选择 API 连接" }));
    await waitFor(() => expect(screen.getByPlaceholderText(/搜索连接名称或地址/)).toBeInTheDocument());
    const menu = await screen.findByRole("menu");
    fireEvent.change(screen.getByPlaceholderText(/搜索连接名称或地址/), { target: { value: "beta" } });
    await waitFor(() => expect(within(menu).queryByText("Alpha 连接")).not.toBeInTheDocument());
    expect(within(menu).getByText("Beta 连接")).toBeInTheDocument();
  });

  it("restores the cached model list when the connection switches", async () => {
    const connections = [connection("a", "连接 A", modelsA), connection("b", "连接 B", modelsB)];
    const { rerender } = renderSelector(connections, "a", "vision-pro");
    fireEvent.focus(screen.getByLabelText("模型 ID"));
    await waitFor(() => expect(screen.getByText("vision-pro")).toBeInTheDocument());
    // Switch to connection B: suggestions now come from B's cache.
    rerender(
      <ApiModelSelector
        connections={connections}
        connectionId="b"
        modelId="gemini-flash"
        onConnectionChange={vi.fn()}
        onModelChange={vi.fn()}
        onManage={vi.fn()}
      />
    );
    fireEvent.focus(screen.getByLabelText("模型 ID"));
    await waitFor(() => expect(screen.getByText("gemini-flash")).toBeInTheDocument());
    expect(screen.queryByText("vision-pro")).not.toBeInTheDocument();
  });

  it("lets the user type a manual model id and pick from suggestions", async () => {
    const onModelChange = vi.fn();
    render(
      <ApiModelSelector
        connections={[connection("a", "连接 A", modelsA)]}
        connectionId="a"
        modelId=""
        onConnectionChange={vi.fn()}
        onModelChange={onModelChange}
        onManage={vi.fn()}
      />
    );
    const input = screen.getByLabelText("模型 ID");
    fireEvent.change(input, { target: { value: "custom-model-1" } });
    expect(onModelChange).toHaveBeenCalledWith("custom-model-1");
    fireEvent.focus(input);
    await waitFor(() => expect(screen.getByText("vision-pro")).toBeInTheDocument());
    fireEvent.click(screen.getByText("vision-pro"));
    expect(onModelChange).toHaveBeenLastCalledWith("vision-pro");
  });

  it("refreshes the model list on demand", async () => {
    renderSelector([connection("a", "连接 A", modelsA)], "a", "");
    fireEvent.click(screen.getByLabelText("刷新模型列表"));
    await waitFor(() => expect(backend.listModels).toHaveBeenLastCalledWith("a", true));
  });

  it("runs a connection test and reports the result", async () => {
    renderSelector([connection("a", "连接 A")], "a", "");
    fireEvent.click(screen.getByLabelText("测试连接"));
    await waitFor(() => expect(backend.testConnection).toHaveBeenCalledWith("a"));
    await waitFor(() => expect(screen.getByText(/连接正常 · 42 ms · 发现 2 个模型/)).toBeInTheDocument());
  });
});
