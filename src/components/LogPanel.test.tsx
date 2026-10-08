import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { LogPanel } from "./LogPanel";
import type { LogEntry } from "../types";

const logs: LogEntry[] = [
  { id: "1", timestamp: "01:00:00.000", level: "info", scope: "caption", message: "开始请求" },
  { id: "2", timestamp: "01:00:01.000", level: "success", scope: "system", message: "数据库已加载" },
  { id: "3", timestamp: "01:00:02.000", level: "error", scope: "revision", message: "请求失败" },
  { id: "4", timestamp: "01:00:03.000", level: "warn", scope: "edit", message: "删除预设" },
  { id: "5", timestamp: "01:00:04.000", level: "info", scope: "system", message: "连接已保存" },
];

describe("LogPanel", () => {
  it("filters by level", () => {
    render(<LogPanel logs={logs} onClear={vi.fn()} />);
    expect(screen.getByText("开始请求")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("日志级别"), { target: { value: "error" } });
    expect(screen.queryByText("开始请求")).not.toBeInTheDocument();
    expect(screen.getByText("请求失败")).toBeInTheDocument();
    expect(screen.getByText("1 条")).toBeInTheDocument();
  });

  it("filters by scope", () => {
    render(<LogPanel logs={logs} onClear={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("日志来源"), { target: { value: "system" } });
    expect(screen.getByText("数据库已加载")).toBeInTheDocument();
    expect(screen.getByText("连接已保存")).toBeInTheDocument();
    expect(screen.queryByText("开始请求")).not.toBeInTheDocument();
  });

  it("combines both filters", () => {
    render(<LogPanel logs={logs} onClear={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("日志级别"), { target: { value: "info" } });
    fireEvent.change(screen.getByLabelText("日志来源"), { target: { value: "system" } });
    expect(screen.getByText("连接已保存")).toBeInTheDocument();
    expect(screen.queryByText("开始请求")).not.toBeInTheDocument();
    expect(screen.getByText("1 条")).toBeInTheDocument();
  });

  it("toggles auto-scroll and clears the panel", () => {
    const onClear = vi.fn();
    render(<LogPanel logs={logs} onClear={onClear} />);
    const checkbox = screen.getByLabelText("自动滚动");
    expect(checkbox).toBeChecked();
    fireEvent.click(checkbox);
    expect(checkbox).not.toBeChecked();
    fireEvent.click(screen.getByLabelText("清空日志"));
    expect(onClear).toHaveBeenCalled();
  });

  it("copies the filtered log lines to the clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    render(<LogPanel logs={logs} onClear={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("日志级别"), { target: { value: "warn" } });
    fireEvent.click(screen.getByLabelText("复制日志"));
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith(expect.stringContaining("[WARN]")));
    expect(writeText.mock.calls[0][0]).toContain("删除预设");
    vi.unstubAllGlobals();
  });
});
