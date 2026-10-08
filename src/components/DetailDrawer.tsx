import { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Clock3, RotateCcw, Save, Undo2, X } from "lucide-react";
import { ApiModelSelector } from "./ApiModelSelector";
import { backend } from "../lib/backend";
import type { ApiConnection, CaptionVersion, ImageJob } from "../types";

const statusText: Record<ImageJob["status"], string> = {
  pending: "等待", running: "处理中", succeeded: "已完成", revised: "已修订",
  no_response: "无响应", refused: "疑似拒绝", failed: "失败", skipped: "已跳过", cancelled: "已取消"
};

interface Props {
  job?: ImageJob;
  connections: ApiConnection[];
  defaultConnectionId: string;
  defaultModelId: string;
  /** 本地模型模式：仅重试本地打标 */
  localMode: boolean;
  localModelId?: string;
  onClose: () => void;
  onSaved: (content: string) => void;
  onRetry: (job: ImageJob, target: { connectionId?: string; modelId?: string; localModelId?: string }) => void;
}

export function DetailDrawer({ job, connections, defaultConnectionId, defaultModelId, localMode, localModelId, onClose, onSaved, onRetry }: Props) {
  const [content, setContent] = useState("");
  const [versions, setVersions] = useState<CaptionVersion[]>([]);
  const [preview, setPreview] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [retryConnection, setRetryConnection] = useState(defaultConnectionId);
  const [retryModel, setRetryModel] = useState(defaultModelId);

  useEffect(() => {
    setContent(job?.caption ?? "");
    setPreview(undefined);
    if (job) {
      backend.listVersions(job.captionPath).then(setVersions).catch(() => setVersions([]));
      const maxDimension = Math.min(1600, Math.max(800, Math.round((window.devicePixelRatio || 1) * 480)));
      backend.getImagePreview(job.imagePath, maxDimension)
        .then((value) => setPreview(value ?? undefined))
        .catch(() => setPreview(undefined));
    }
  }, [job]);

  useEffect(() => {
    if (job && defaultConnectionId && (!retryConnection || !connections.some((c) => c.id === retryConnection))) {
      setRetryConnection(defaultConnectionId);
      setRetryModel(defaultModelId);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job, connections]);

  async function save() {
    if (!job) return;
    setBusy(true);
    try { await backend.saveCaption(job.captionPath, content, "manual"); onSaved(content); setVersions(await backend.listVersions(job.captionPath)); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  async function restore(id: number) {
    setBusy(true);
    try { await backend.restoreVersion(id); const restored = versions.find((v) => v.id === id)?.oldContent ?? ""; setContent(restored); onSaved(restored); setVersions(await backend.listVersions(job?.captionPath)); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  const canRetry = job && (job.status === "failed" || job.status === "refused" || job.status === "cancelled") && retryConnection && retryModel.trim();

  return (
    <Dialog.Root open={Boolean(job)} onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="drawer-overlay" />
        <Dialog.Content className="detail-drawer" aria-describedby={undefined}>
          <div className="drawer-header"><div><Dialog.Title>{job?.fileName}</Dialog.Title>{job && <span className={`status-pill ${job.status}`}>{statusText[job.status]}</span>}</div><Dialog.Close className="icon-button" aria-label="关闭"><X size={18} /></Dialog.Close></div>
          <div className="drawer-scroll scroll-area">
            <div className="drawer-image">{preview ? <img src={preview} alt={job?.fileName} /> : job?.thumbnail ? <img src={job.thumbnail} alt={job?.fileName} /> : <div className="image-placeholder" />}</div>
            <label>完整标签<textarea className="caption-editor" value={content} onChange={(e) => setContent(e.target.value)} /></label>
            <button className="button primary full" disabled={busy || content === job?.caption} onClick={() => void save()}><Save size={16} />保存标签</button>
            {(job?.status === "failed" || job?.status === "refused" || job?.status === "cancelled") && (
              <section className="detail-section retry-box">
                <h3>单图重试</h3>
                {localMode ? (
                  <>
                    <p className="helper">使用本地模型 {localModelId ?? "（未选择）"} 重新打标。</p>
                    <button className="button secondary full" disabled={!localModelId || busy} onClick={() => job && onRetry(job, { localModelId })}><RotateCcw size={15} />用本地模型重试</button>
                  </>
                ) : (
                  <>
                    <p className="helper">使用下方 API/模型重新处理此图，可在重试前临时切换连接或模型。</p>
                    <ApiModelSelector
                      connections={connections}
                      connectionId={retryConnection}
                      modelId={retryModel}
                      onConnectionChange={(id) => { setRetryConnection(id); const next = connections.find((c) => c.id === id); setRetryModel(next?.lastModel ?? retryModel); }}
                      onModelChange={setRetryModel}
                      onManage={() => undefined}
                    />
                    <button className="button secondary full" disabled={!canRetry || busy} onClick={() => job && onRetry(job, { connectionId: retryConnection, modelId: retryModel })}><RotateCcw size={15} />重试此图</button>
                  </>
                )}
              </section>
            )}
            {job?.metrics && <section className="detail-section"><h3>请求指标</h3><dl className="metric-list"><div><dt>输入 Token</dt><dd>{job.metrics.inputTokens ?? "未提供"}</dd></div><div><dt>输出 Token</dt><dd>{job.metrics.outputTokens ?? "未提供"}</dd></div><div><dt>首字延迟</dt><dd>{job.metrics.ttftMs == null ? "未提供" : `${job.metrics.ttftMs.toFixed(0)} ms`}</dd></div><div><dt>Token 速度</dt><dd>{job.metrics.tokensPerSecond == null ? "未提供" : `${job.metrics.tokensPerSecond.toFixed(1)} t/s`}</dd></div></dl></section>}
            {job?.error && <div className="inline-error">{job.error}</div>}
            {job?.refusal && <section className="detail-section refusal-box"><h3>拒绝检测</h3><p>{job.refusal.excerpt}</p><small>规则：{job.refusal.rule}</small></section>}
            <section className="detail-section"><h3><Clock3 size={16} /> 版本历史</h3>{versions.length === 0 ? <p className="muted">暂无历史版本</p> : <div className="version-list">{versions.map((version) => <article key={version.id}><div><b>{version.source === "revision" ? "模型修订" : version.source === "batch_edit" ? "批量编辑" : version.source === "undo" ? "撤销恢复" : "手动编辑"}</b><small>{new Date(version.createdAt).toLocaleString()}</small></div><p>{version.oldContent}</p><button className="text-button" disabled={busy} onClick={() => void restore(version.id)}><Undo2 size={14} />恢复此版本</button></article>)}</div>}</section>
            {error && <div className="inline-error">{error}</div>}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
