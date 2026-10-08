import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { BatchEditDialog } from "./BatchEditDialog";
import { backend } from "../lib/backend";
import type { CaptionEditBatch } from "../types";

vi.mock("../lib/backend", () => ({
  backend: {
    previewEdit: vi.fn(),
    applyEdit: vi.fn(),
    selectTextFiles: vi.fn(),
    selectTextFolder: vi.fn(),
  },
}));

const paths = (prefix: string, count: number) => Array.from({ length: count }, (_, i) => `${prefix}${i}.txt`);

const previewBatch: CaptionEditBatch = {
  id: "batch-1",
  rule: { mode: "tag", action: "insert", position: "end", content: "smile", regex: false, caseSensitive: false, allMatches: true, deduplicate: true },
  previews: [
    { path: "a.txt", before: "1girl", after: "1girl, smile", changed: true, error: undefined },
    { path: "b.txt", before: "1boy", after: "1boy, smile", changed: true, error: undefined },
    { path: "c.txt", before: "1girl, smile", after: "1girl, smile", changed: false, error: undefined },
    { path: "missing.txt", before: "", after: "", changed: false, error: "文件不存在" },
  ],
  affected: 2, skipped: 1, errors: 1, createdAt: new Date().toISOString(), applied: false,
};

function renderDialog(overrides: Partial<Parameters<typeof BatchEditDialog>[0]> = {}) {
  return render(
    <BatchEditDialog
      open
      selectedPaths={paths("sel-", 2)}
      filteredPaths={paths("filter-", 3)}
      allPaths={paths("all-", 5)}
      counts={{ selected: 2, filtered: 3, all: 5 }}
      onOpenChange={vi.fn()}
      onApplied={vi.fn()}
      {...overrides}
    />
  );
}

describe("BatchEditDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(backend.previewEdit).mockResolvedValue(previewBatch);
    vi.mocked(backend.applyEdit).mockResolvedValue({ ...previewBatch, applied: true });
    vi.mocked(backend.selectTextFiles).mockResolvedValue(paths("picked-", 4));
    vi.mocked(backend.selectTextFolder).mockResolvedValue(paths("folder-", 6));
  });

  it("offers all five scopes with disabled states", () => {
    renderDialog({ counts: { selected: 0, filtered: 0, all: 5 } });
    const buttons = screen.getAllByRole("button");
    const scopeButtons = buttons.filter((button) => ["当前选中", "当前筛选结果", "任务中全部", "独立 TXT 多选", "TXT 文件夹"].some((label) => button.textContent?.startsWith(label) ?? false));
    expect(scopeButtons).toHaveLength(5);
    expect(scopeButtons[0]).toBeDisabled(); // no selection
    expect(scopeButtons[1]).toBeDisabled(); // no filtered results
    expect(scopeButtons[2]).not.toBeDisabled(); // all exists
  });

  it("previews with the active scope paths and applies by batch id", async () => {
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: /任务中全部/ }));
    fireEvent.change(screen.getByLabelText("要添加的内容"), { target: { value: "smile" } });
    fireEvent.click(screen.getByRole("button", { name: /生成预览/ }));
    await waitFor(() => expect(backend.previewEdit).toHaveBeenCalledWith(paths("all-", 5), expect.objectContaining({ content: "smile" })));
    await waitFor(() => expect(screen.getByText("2 个文件将被修改，1 个错误，1 个跳过")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: /应用 2 项修改/ }));
    await waitFor(() => expect(backend.applyEdit).toHaveBeenCalledWith("batch-1"));
  });

  it("highlights the changed segment in previews with colored marks", async () => {
    vi.mocked(backend.previewEdit).mockResolvedValue({
      id: "batch-diff",
      rule: previewBatch.rule,
      previews: [
        { path: "a.txt", before: "1girl, solo", after: "1girl, solo, smile", changed: true, error: undefined },
        { path: "b.txt", before: "foo 12 bar", after: "foo N bar", changed: true, error: undefined },
      ],
      affected: 2, skipped: 0, errors: 0, createdAt: new Date().toISOString(), applied: false,
    });
    renderDialog();
    fireEvent.change(screen.getByLabelText("要添加的内容"), { target: { value: "smile" } });
    fireEvent.click(screen.getByRole("button", { name: /生成预览/ }));
    await waitFor(() => {
      // after 侧新增片段以绿色底纹标记，before 侧删除片段以红色底纹标记。
      const added = document.querySelectorAll(".preview-list .diff-added");
      expect(added.length).toBeGreaterThan(0);
      expect(added[0].textContent).toContain(", smile");
      expect(document.querySelectorAll(".preview-list .diff-removed").length).toBeGreaterThan(0);
    });
  });

  it("switches scope to the current selection", async () => {
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: /当前选中/ }));
    fireEvent.change(screen.getByLabelText("要添加的内容"), { target: { value: "smile" } });
    fireEvent.click(screen.getByRole("button", { name: /生成预览/ }));
    await waitFor(() => expect(backend.previewEdit).toHaveBeenCalledWith(paths("sel-", 2), expect.anything()));
  });

  it("picks standalone text files as a scope", async () => {
    renderDialog();
    fireEvent.click(screen.getByRole("button", { name: /独立 TXT 多选/ }));
    fireEvent.click(screen.getByRole("button", { name: /选择 TXT 文件/ }));
    await waitFor(() => expect(backend.selectTextFiles).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByText("4 个文件")).toBeInTheDocument());
    fireEvent.change(screen.getByLabelText("要添加的内容"), { target: { value: "smile" } });
    fireEvent.click(screen.getByRole("button", { name: /生成预览/ }));
    await waitFor(() => expect(backend.previewEdit).toHaveBeenCalledWith(paths("picked-", 4), expect.anything()));
  });

  it("marks applied results with badges and disables re-apply", async () => {
    const onApplied = vi.fn();
    vi.mocked(backend.applyEdit).mockResolvedValue({
      ...previewBatch,
      applied: true,
      results: [
        { path: "a.txt", status: "applied" as const },
        { path: "b.txt", status: "conflict" as const, error: "文件在预览后被外部修改，已跳过" },
        { path: "missing.txt", status: "error" as const, error: "文件不存在" },
      ],
    });
    renderDialog({ onApplied });
    fireEvent.change(screen.getByLabelText("要添加的内容"), { target: { value: "smile" } });
    fireEvent.click(screen.getByRole("button", { name: /生成预览/ }));
    await waitFor(() => expect(screen.getByRole("button", { name: /应用 2 项修改/ })).not.toBeDisabled());
    fireEvent.click(screen.getByRole("button", { name: /应用 2 项修改/ }));
    await waitFor(() => {
      expect(onApplied).toHaveBeenCalledWith(expect.objectContaining({
        results: expect.arrayContaining([
          expect.objectContaining({ status: "applied" }),
          expect.objectContaining({ status: "conflict" }),
        ]),
      }));
    });
  });
});
