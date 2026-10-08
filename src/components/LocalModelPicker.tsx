import { Download, HardDrive, RefreshCw, WifiOff } from "lucide-react";
import type { LocalModelState } from "../types";

interface Props {
  models: LocalModelState[];
  selectedId: string;
  disabled?: boolean;
  onSelect: (id: string) => void;
  onDownload: (id: string) => void;
  onRefresh: () => void;
}

/**
 * 本地打标模型选择：恒定高度的单行结构（下拉 + 状态 + 下载按钮）。
 * 下载进度条与错误提示使用绝对定位浮层，不参与布局，避免撑动
 * 同一控制卡内的提示词等相邻 UI。
 */
export function LocalModelPicker({ models, selectedId, disabled, onSelect, onDownload, onRefresh }: Props) {
  const selected = models.find((model) => model.id === selectedId);
  const storagePath = selected?.storagePath ?? models[0]?.storagePath;
  const downloading = Boolean(selected?.downloading);
  const progress = selected && selected.total ? Math.min(100, (selected.received / selected.total) * 100) : 0;
  return (
    <div className="local-model-picker" title={storagePath ? `模型目录：${storagePath}` : undefined}>
      <div className="local-model-row">
        <HardDrive size={15} />
        <select
          aria-label="本地打标模型"
          value={selectedId}
          disabled={disabled}
          onChange={(event) => onSelect(event.target.value)}
        >
          <option value="">选择本地模型…</option>
          {models.map((model) => (
            <option key={model.id} value={model.id}>
              {model.name} · {model.installed ? "已安装" : model.downloading ? "下载中" : `${model.sizeMb}MB`}
            </option>
          ))}
        </select>
        {selected?.installed && <span className="model-badge ready">已就绪</span>}
        {selected && !selected.installed && !downloading && (
          <button className="button small" disabled={disabled} onClick={() => onDownload(selected.id)}><Download size={14} />下载 {selected.sizeMb}MB</button>
        )}
        {downloading && <span className="model-badge downloading">{progress.toFixed(0)}%</span>}
        <button className="icon-button mini" onClick={onRefresh} disabled={disabled} aria-label="刷新本地模型" title="刷新本地模型">
          <RefreshCw size={15} />
        </button>
      </div>
      {downloading && (
        <div className="download-progress overlay" aria-label="下载进度">
          <div className="progress-track"><div className="progress-fill" style={{ width: `${progress}%` }} /></div>
        </div>
      )}
      {selected?.error && (
        <div className="download-error overlay"><WifiOff size={12} />{selected.error}</div>
      )}
    </div>
  );
}
