import { useMemo, useState } from "react";
import { CheckCircle2, CircleAlert, Copy, Eraser, FilePlus2, LoaderCircle, ShieldCheck } from "lucide-react";
import { backend } from "../lib/backend";
import type { LoraMetadataReport, LoraSanitizeResult } from "../types";

function sizeLabel(bytes: number) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
}

export function PostprocessView() {
  const [reports, setReports] = useState<LoraMetadataReport[]>([]);
  const [results, setResults] = useState<LoraSanitizeResult[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const candidates = useMemo(() => reports.filter((item) => item.metadataPresent && !item.error), [reports]);

  async function choose() {
    setBusy(true);
    setError("");
    try {
      setReports(await backend.selectLoraFiles());
      setResults([]);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function sanitize() {
    setBusy(true);
    setError("");
    try {
      setResults(await backend.sanitizeLoraMetadata(candidates.map((item) => item.path)));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  return <div className="postprocess-page">
    <section className="page-card postprocess-hero">
      <div className="postprocess-hero-icon"><ShieldCheck size={28} /></div>
      <div><h2>LoRA 元数据净化</h2><p>删除 SafeTensors 的整个 <code>__metadata__</code> 区块，使最终文件无法读取 sd-scripts 训练参数；张量数据保持逐字节一致。</p></div>
      <button className="button primary" disabled={busy} onClick={() => void choose()}><FilePlus2 size={16} />选择 LoRA 模型</button>
    </section>

    <section className="page-card postprocess-guarantees">
      <div><CheckCircle2 size={17} /><span><b>全部元数据删除</b><small>不仅是 ss_*，所有训练期键值均清除</small></span></div>
      <div><CheckCircle2 size={17} /><span><b>不覆盖原文件</b><small>输出 *.metadata-removed.safetensors</small></span></div>
      <div><CheckCircle2 size={17} /><span><b>双重验证</b><small>重新扫描头部并校验张量数据 SHA-256</small></span></div>
    </section>

    <section className="page-card postprocess-files">
      <header><div><h3>待处理模型</h3><p>{reports.length ? `已检查 ${reports.length} 个文件，可净化 ${candidates.length} 个` : "支持批量选择 .safetensors 文件"}</p></div><button className="button secondary" disabled={busy || !candidates.length} onClick={() => void sanitize()}>{busy ? <LoaderCircle className="spin" size={16} /> : <Eraser size={16} />}彻底抹去元数据</button></header>
      <div className="postprocess-list">
        {error && <div className="postprocess-inline-error"><CircleAlert size={16} />{error}</div>}
        {reports.length ? reports.map((report) => <article key={report.path} className={report.error ? "error" : report.metadataPresent ? "pending" : "clean"}>
          <div className="postprocess-file-main"><b>{report.fileName}</b><small>{sizeLabel(report.sizeBytes)} · {report.tensorCount} 个张量</small><small title={report.path}>{report.path}</small></div>
          {report.error ? <span className="postprocess-status error"><CircleAlert size={14} />{report.error}</span> : report.metadataPresent ? <><span className="postprocess-status pending">检测到 {report.metadataCount} 个元数据字段</span><div className="metadata-key-list">{report.metadataKeys.slice(0, 8).map((key) => <code key={key}>{key}</code>)}{report.metadataKeys.length > 8 && <small>+{report.metadataKeys.length - 8}</small>}</div></> : <span className="postprocess-status clean"><CheckCircle2 size={14} />已无元数据</span>}
        </article>) : <div className="postprocess-empty"><ShieldCheck size={30} /><b>尚未选择模型</b><span>原始模型始终保留，净化结果写入同目录的新文件</span></div>}
      </div>
    </section>

    {!!results.length && <section className="page-card postprocess-results"><header><h3>净化结果</h3><span>{results.filter((item) => item.status === "sanitized").length}/{results.length} 完成</span></header>{results.map((result) => <article key={result.sourcePath} className={result.status}>
      {result.status === "sanitized" ? <CheckCircle2 size={18} /> : <CircleAlert size={18} />}
      <div><b>{result.status === "sanitized" ? `已删除 ${result.removedCount} 个元数据字段` : result.status === "already_clean" ? "文件原本已净化" : "净化失败"}</b><small>{result.error ?? result.outputPath ?? result.sourcePath}</small>{result.tensorDataVerified && <em>张量 SHA-256 一致 · 二次扫描无元数据</em>}</div>
      {result.outputPath && <button className="icon-button mini" aria-label="复制输出路径" onClick={() => void navigator.clipboard.writeText(result.outputPath!)}><Copy size={14} /></button>}
    </article>)}</section>}
  </div>;
}
