import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Copy, Trash2 } from "lucide-react";
import type { LogEntry } from "../types";

interface Props {
  logs: LogEntry[];
  onClear: () => void;
}

export function LogPanel({ logs, onClear }: Props) {
  const [level, setLevel] = useState("all");
  const [scope, setScope] = useState("all");
  const [autoScroll, setAutoScroll] = useState(true);
  const [height, setHeight] = useState(() => Number(localStorage.getItem("logPanelHeight")) || 200);
  const [copied, setCopied] = useState(false);
  const streamRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ startY: number; startHeight: number } | undefined>(undefined);

  const visible = useMemo(() => logs.filter((log) =>
    (level === "all" || log.level === level) && (scope === "all" || log.scope === scope)
  ), [logs, level, scope]);

  useEffect(() => {
    if (autoScroll && streamRef.current) {
      streamRef.current.scrollTop = streamRef.current.scrollHeight;
    }
  }, [visible, autoScroll]);

  function onScroll() {
    const el = streamRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    if (!atBottom && autoScroll) setAutoScroll(false);
    if (atBottom && !autoScroll) setAutoScroll(true);
  }

  function startDrag(event: React.PointerEvent) {
    dragRef.current = { startY: event.clientY, startHeight: height };
    (event.target as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onDrag(event: React.PointerEvent) {
    if (!dragRef.current) return;
    const next = Math.min(560, Math.max(120, dragRef.current.startHeight + (dragRef.current.startY - event.clientY)));
    setHeight(next);
    localStorage.setItem("logPanelHeight", String(next));
  }

  function endDrag() {
    dragRef.current = undefined;
  }

  async function copy() {
    await navigator.clipboard.writeText(visible.map((log) => `[${log.timestamp}] [${log.level.toUpperCase()}] [${log.scope}] ${log.message}`).join("\n"));
    setCopied(true); setTimeout(() => setCopied(false), 1200);
  }

  return (
    <section className="log-panel" style={{ height }}>
      <div className="log-resize-handle" onPointerDown={startDrag} onPointerMove={onDrag} onPointerUp={endDrag} aria-label="调整日志高度" />
      <header><div><span className="live-dot" /><b>运行日志</b><small>{visible.length} 条</small></div><div>
        <select aria-label="日志级别" value={level} onChange={(e) => setLevel(e.target.value)}><option value="all">全部级别</option><option value="info">信息</option><option value="success">成功</option><option value="warn">警告</option><option value="error">错误</option></select>
        <select aria-label="日志来源" value={scope} onChange={(e) => setScope(e.target.value)}><option value="all">全部来源</option><option value="system">系统</option><option value="caption">打标</option><option value="revision">复核</option><option value="edit">批量编辑</option></select>
        <label className="check compact" title="自动滚动到底部"><input type="checkbox" checked={autoScroll} onChange={(e) => setAutoScroll(e.target.checked)} />自动滚动</label>
        <button className="icon-button mini" onClick={() => void copy()} title="复制日志" aria-label="复制日志">{copied ? <Check size={15} /> : <Copy size={15} />}</button>
        <button className="icon-button mini" onClick={onClear} title="清空日志" aria-label="清空日志"><Trash2 size={15} /></button>
      </div></header>
      <div className="log-stream scroll-area" ref={streamRef} onScroll={onScroll}>
        {visible.map((log) => <div key={log.id} className={`log-line ${log.level}`}><time>[{log.timestamp}]</time><span>[{log.level.toUpperCase()}]</span><em>[{log.scope}]</em><p>{log.message}</p></div>)}
        {visible.length === 0 && <div className="log-empty">暂无日志</div>}
      </div>
    </section>
  );
}
