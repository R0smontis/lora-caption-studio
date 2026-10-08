import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { LocalModelPicker } from "./LocalModelPicker";
import type { LocalModelState } from "../types";

const models: LocalModelState[] = [
  { id: "wd-v1-4-vit-tagger-v2", name: "WD v1.4 ViT-B/16", sizeMb: 373, installed: false, downloading: false, received: 0, total: 373 * 1024 * 1024 },
  { id: "wd-v1-4-moat-tagger-v2", name: "WD v1.4 Moat", sizeMb: 326, installed: true, downloading: false, received: 0, total: 326 * 1024 * 1024 },
  { id: "wd-eva02-large-tagger-v3", name: "WD v3 EVA02-Large", sizeMb: 1260, installed: false, downloading: true, received: 630 * 1024 * 1024, total: 1260 * 1024 * 1024 },
];

function renderPicker(overrides: Partial<Parameters<typeof LocalModelPicker>[0]> = {}) {
  return render(
    <LocalModelPicker
      models={models}
      selectedId=""
      onSelect={vi.fn()}
      onDownload={vi.fn()}
      onRefresh={vi.fn()}
      {...overrides}
    />
  );
}

describe("LocalModelPicker", () => {
  it("keeps candidates collapsed in a single select and marks install state", () => {
    renderPicker();
    const select = screen.getByLabelText("本地打标模型");
    const options = Array.from((select as HTMLSelectElement).options).map((o) => o.textContent);
    expect(options).toContain("WD v1.4 ViT-B/16 · 373MB");
    expect(options).toContain("WD v1.4 Moat · 已安装");
    expect(options).toContain("WD v3 EVA02-Large · 下载中");
  });

  it("shows a download button for uninstalled models", () => {
    const onDownload = vi.fn();
    renderPicker({ selectedId: "wd-v1-4-vit-tagger-v2", onDownload });
    fireEvent.click(screen.getByRole("button", { name: /下载 373MB/ }));
    expect(onDownload).toHaveBeenCalledWith("wd-v1-4-vit-tagger-v2");
  });

  it("shows a progress badge and an overlay bar while downloading", () => {
    renderPicker({ selectedId: "wd-eva02-large-tagger-v3" });
    expect(screen.getByText("50%")).toBeInTheDocument();
    const fill = document.querySelector(".progress-fill") as HTMLElement;
    expect(fill.style.width).toBe("50%");
  });

  it("marks installed models as ready without adding layout rows", () => {
    const { container } = renderPicker({ selectedId: "wd-v1-4-moat-tagger-v2" });
    expect(screen.getByText("已就绪")).toBeInTheDocument();
    // 恒定结构：选择器下不再出现 meta 行；状态与按钮都收在单行内。
    expect(container.querySelectorAll(".local-model-picker > .local-model-row")).toHaveLength(1);
    expect(container.querySelectorAll(".local-model-meta")).toHaveLength(0);
    expect(container.querySelectorAll(".download-progress")).toHaveLength(0);
  });

  it("fires onSelect when the user picks a model", () => {
    const onSelect = vi.fn();
    renderPicker({ onSelect });
    fireEvent.change(screen.getByLabelText("本地打标模型"), { target: { value: "wd-v1-4-moat-tagger-v2" } });
    expect(onSelect).toHaveBeenCalledWith("wd-v1-4-moat-tagger-v2");
  });

  it("fires onRefresh", () => {
    const onRefresh = vi.fn();
    renderPicker({ onRefresh });
    fireEvent.click(screen.getByLabelText("刷新本地模型"));
    expect(onRefresh).toHaveBeenCalled();
  });

  it("reports download errors", () => {
    renderPicker({ selectedId: "wd-v1-4-vit-tagger-v2", models: [{ ...models[0], error: "网络错误" }] });
    expect(screen.getByText("网络错误")).toBeInTheDocument();
  });
});
