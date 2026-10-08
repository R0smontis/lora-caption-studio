import { useEffect, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import * as Tabs from "@radix-ui/react-tabs";
import { ArrowRight, CheckCircle2, Edit3, Eye, FileText, FolderOpen, X } from "lucide-react";
import { backend } from "../lib/backend";
import { DEFAULT_EDIT_RULE } from "../lib/defaults";
import { diffParts } from "../lib/diffHighlight";
import type { CaptionEditBatch, CaptionEditRule, EditPreview } from "../types";

type EditScope = "selected" | "filtered" | "all" | "files" | "folder";

interface Props {
  open: boolean;
  /** paths for the "current selection" scope */
  selectedPaths: string[];
  /** paths for the "current filter" scope */
  filteredPaths: string[];
  /** paths for the "all items" scope */
  allPaths: string[];
  counts: { selected: number; filtered: number; all: number };
  onOpenChange: (open: boolean) => void;
  onApplied: (batch: CaptionEditBatch) => void;
}

function DiffPreview({ before, after }: { before: string; after: string }) {
  const parts = diffParts(before, after);
  return (
    <>
      <p className="diff-line">
        <span>{parts.beforePrefix}</span>
        {parts.beforeChanged && <mark className="diff-removed">{parts.beforeChanged}</mark>}
        <span>{parts.beforeSuffix}</span>
      </p>
      <ArrowRight size={14} />
      <p className="diff-line">
        <span>{parts.afterPrefix}</span>
        {parts.afterChanged && <mark className="diff-added">{parts.afterChanged}</mark>}
        <span>{parts.afterSuffix}</span>
      </p>
    </>
  );
}

const SCOPE_LABELS: Record<EditScope, string> = {
  selected: "当前选中",
  filtered: "当前筛选结果",
  all: "任务中全部",
  files: "独立 TXT 多选",
  folder: "TXT 文件夹",
};

export function BatchEditDialog({ open, selectedPaths, filteredPaths, allPaths, counts, onOpenChange, onApplied }: Props) {
  const [rule, setRule] = useState<CaptionEditRule>(DEFAULT_EDIT_RULE);
  const [scope, setScope] = useState<EditScope>("selected");
  const [filePaths, setFilePaths] = useState<string[]>([]);
  const [batch, setBatch] = useState<CaptionEditBatch>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => { if (!open) { setBatch(undefined); setError(""); } }, [open]);

  const paths = useMemo(() => {
    switch (scope) {
      case "selected": return selectedPaths;
      case "filtered": return filteredPaths;
      case "all": return allPaths;
      case "files": return filePaths;
      case "folder": return filePaths;
    }
  }, [scope, selectedPaths, filteredPaths, allPaths, filePaths]);

  const scopeDisabled: Record<EditScope, boolean> = {
    selected: counts.selected === 0,
    filtered: counts.filtered === 0,
    all: counts.all === 0,
    files: false,
    folder: false,
  };

  function patch<K extends keyof CaptionEditRule>(key: K, value: CaptionEditRule[K]) {
    setRule((current) => ({ ...current, [key]: value }));
    setBatch(undefined);
  }

  async function pickFiles() {
    try {
      setFilePaths(await backend.selectTextFiles());
      setBatch(undefined);
    } catch (reason) {
      if (!String(reason).toLocaleLowerCase().includes("cancel")) setError(String(reason));
    }
  }

  async function pickFolder() {
    try {
      setFilePaths(await backend.selectTextFolder(false));
      setBatch(undefined);
    } catch (reason) {
      if (!String(reason).toLocaleLowerCase().includes("cancel")) setError(String(reason));
    }
  }

  async function preview() {
    if (!rule.content && rule.action !== "insert") return setError("请输入要处理的 Tag 或内容");
    if (paths.length === 0) return setError("该范围下没有可处理的文本文档");
    setBusy(true);
    try { setBatch(await backend.previewEdit(paths, rule)); setError(""); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  async function apply() {
    if (!batch || batch.applied) return;
    setBusy(true);
    try {
      const result = await backend.applyEdit(batch.id);
      onApplied(result);
      onOpenChange(false);
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  const changed = batch?.affected ?? 0;
  const previews = batch?.previews ?? [];
  const results = batch?.results;
  const needsPosition = rule.action === "insert";
  const needsAnchor = needsPosition && (rule.position === "before_anchor" || rule.position === "after_anchor");

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className="dialog-content edit-dialog" aria-describedby={undefined}>
          <div className="dialog-header">
            <div><Dialog.Title>批量编辑标签</Dialog.Title><p>选择范围后预览，应用时校验文件在预览后未被外部修改</p></div>
            <Dialog.Close className="icon-button" aria-label="关闭"><X size={18} /></Dialog.Close>
          </div>
          <Tabs.Root value={rule.mode} onValueChange={(value) => patch("mode", value as CaptionEditRule["mode"])}>
            <Tabs.List className="segmented"><Tabs.Trigger value="tag">Tag 模式</Tabs.Trigger><Tabs.Trigger value="text">纯文本模式</Tabs.Trigger></Tabs.List>
          </Tabs.Root>
          <div className="edit-layout">
            <div className="edit-form scroll-area">
              <div className="scope-label"><span>编辑范围</span>
                <div className="scope-grid">
                  {(Object.keys(SCOPE_LABELS) as EditScope[]).map((item) => (
                    <button key={item} type="button" className={scope === item ? "active" : ""} disabled={scopeDisabled[item]} onClick={() => { setScope(item); setBatch(undefined); setError(""); }}>
                      {item === "files" ? <FileText size={13} /> : item === "folder" ? <FolderOpen size={13} /> : null}
                      {SCOPE_LABELS[item]}
                      {item !== "files" && item !== "folder" && <small>{item === "selected" ? counts.selected : item === "filtered" ? counts.filtered : counts.all}</small>}
                    </button>
                  ))}
                </div>
                {scope === "files" && <span className="scope-actions"><button className="text-button" onClick={() => void pickFiles()}><FileText size={13} />选择 TXT 文件</button>{filePaths.length > 0 && <small>{filePaths.length} 个文件</small>}</span>}
                {scope === "folder" && <span className="scope-actions"><button className="text-button" onClick={() => void pickFolder()}><FolderOpen size={13} />选择 TXT 文件夹</button>{filePaths.length > 0 && <small>{filePaths.length} 个文件</small>}</span>}
              </div>
              <div className="form-grid two">
                <label>操作<select value={rule.action} onChange={(e) => patch("action", e.target.value as CaptionEditRule["action"])}><option value="insert">添加</option><option value="delete">删除</option><option value="replace">替换</option></select></label>
                {needsPosition && <label>位置<select value={rule.position} onChange={(e) => patch("position", e.target.value as CaptionEditRule["position"])}><option value="start">首部</option><option value="end">尾部</option><option value="index">指定位置</option><option value="before_anchor">锚点前</option><option value="after_anchor">锚点后</option>{rule.mode === "text" && <option value="line">指定行</option>}</select></label>}
              </div>
              <label>{rule.action === "insert" ? "要添加的内容" : "查找内容"}<textarea value={rule.content} onChange={(e) => patch("content", e.target.value)} placeholder={rule.mode === "tag" ? "masterpiece, best quality" : "输入文本"} /></label>
              {rule.action === "replace" && <label>替换为<textarea value={rule.replacement} onChange={(e) => patch("replacement", e.target.value)} /></label>}
              {(rule.position === "index" || rule.position === "line") && needsPosition && <label>{rule.position === "line" ? "行号" : rule.mode === "tag" ? "第 N 个 Tag" : "字符位置"}<input type="number" min={rule.mode === "text" && rule.position === "index" ? 0 : 1} value={rule.index} onChange={(e) => patch("index", Number(e.target.value))} /></label>}
              {needsAnchor && <label>锚点<input value={rule.anchor} onChange={(e) => patch("anchor", e.target.value)} /></label>}
              <div className="option-list">
                <label className="check"><input type="checkbox" checked={rule.regex} onChange={(e) => patch("regex", e.target.checked)} /> 使用正则表达式</label>
                <label className="check"><input type="checkbox" checked={rule.caseSensitive} onChange={(e) => patch("caseSensitive", e.target.checked)} /> 区分大小写</label>
                <label className="check"><input type="checkbox" checked={rule.allMatches} onChange={(e) => patch("allMatches", e.target.checked)} /> 处理全部匹配项</label>
                {rule.mode === "tag" && <label className="check"><input type="checkbox" checked={rule.deduplicate} onChange={(e) => patch("deduplicate", e.target.checked)} /> 修改后去重</label>}
                {rule.action === "insert" && <label className="check"><input type="checkbox" checked={rule.onlyIfMissing ?? false} onChange={(e) => patch("onlyIfMissing", e.target.checked)} /> 仅当内容缺失时添加</label>}
              </div>
              {error && <div className="inline-error">{error}</div>}
              <button className="button secondary full" disabled={busy} onClick={() => void preview()}><Eye size={16} />生成预览</button>
            </div>
            <div className="preview-pane">
              <div className="preview-summary"><Edit3 size={16} /><b>{batch ? `${changed} 个文件将被修改${batch.errors ? `，${batch.errors} 个错误` : ""}${batch.skipped ? `，${batch.skipped} 个跳过` : ""}` : "等待预览"}</b></div>
              <div className="preview-list scroll-area">
                {previews.slice(0, 30).map((item) => {
                  const result = results?.find((entry) => entry.path === item.path);
                  return (
                    <article key={item.path} className={item.error ? "errored" : item.changed ? "changed" : "unchanged"}>
                      <small>{item.path}</small>
                      {result && <em className={`apply-badge ${result.status}`}>{result.status === "applied" ? "已应用" : result.status === "conflict" ? "冲突跳过" : result.status === "skipped" ? "无变化" : "错误"}</em>}
                      {item.error ? <p className="error-text">{item.error}</p> : result?.error ? <p className="error-text">{result.error}</p> : item.changed ? (
                        <DiffPreview before={item.before} after={item.after} />
                      ) : <><p>{item.before || "（空）"}</p><ArrowRight size={14} /><p>{item.after || "（空）"}</p></>}
                    </article>
                  );
                })}
              </div>
            </div>
          </div>
          <div className="dialog-footer"><span>{previews.length > 30 ? `仅展示前 30 项，共 ${previews.length} 项` : "所有修改均可在版本历史中撤销"}</span><Dialog.Close className="button ghost">取消</Dialog.Close><button className="button primary" disabled={busy || !batch || batch.applied || changed === 0} onClick={() => void apply()}><CheckCircle2 size={16} />应用 {changed || ""} 项修改</button></div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
