import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronsUp, Plus, RefreshCcw, Settings2 } from "lucide-react";
import { backend } from "../lib/backend";
import type { ApiConnection, ModelOption } from "../types";

export interface ApiChannel {
  connectionId: string;
  modelId: string;
}

export const MAX_CHANNELS = 4;

interface Props {
  connections: ApiConnection[];
  /** 渠道列表（长度 ≤ MAX_CHANNELS；空项表示不使用） */
  channels: ApiChannel[];
  disabled?: boolean;
  onChange: (channels: ApiChannel[]) => void;
  onManage: () => void;
  /** 获取模型成功后回调（App 层持久化连接缓存） */
  onModelsRefreshed?: (connectionId: string, models: ModelOption[]) => void;
}

/**
 * 多渠道编辑器：固定 N 行下拉（连接 + 模型），行可留空。
 * 每渠道独立选择 API 连接与模型；模型列支持获取模型按钮与弹出选单。
 */
export function ChannelsEditor({ connections, channels, disabled, onChange, onManage, onModelsRefreshed }: Props) {
  const rows = Array.from({ length: MAX_CHANNELS }, (_, i) => channels[i] ?? { connectionId: "", modelId: "" });
  const [modelLists, setModelLists] = useState<Record<string, ModelOption[]>>({});
  const [openMenu, setOpenMenu] = useState<number | null>(null);
  const [fetching, setFetching] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(() => channels.slice(1).some((channel) => channel.connectionId));
  const menuRefs = useRef<(HTMLDivElement | null)[]>([]);
  const enabledCount = rows.filter((row) => row.connectionId).length;

  useEffect(() => {
    if (channels.slice(1).some((channel) => channel.connectionId)) setExpanded(true);
  }, [channels]);

  function updateRow(index: number, patch: Partial<ApiChannel>) {
    const next = rows.map((row, i) => (i === index ? { ...row, ...patch } : row));
    onChange(next);
  }

  async function fetchModels(row: ApiChannel, index: number) {
    if (!row.connectionId || fetching) return;
    setFetching(row.connectionId);
    try {
      const fetched = await backend.listModels(row.connectionId, true);
      setModelLists((cur) => ({ ...cur, [row.connectionId]: fetched }));
      if (onModelsRefreshed) onModelsRefreshed(row.connectionId, fetched);
    } catch (reason) {
      // 获取失败时保留缓存列表
      void reason;
    } finally {
      setFetching(null);
    }
    void index;
  }

  function modelsFor(connectionId: string): ModelOption[] {
    return modelLists[connectionId] ?? connections.find((c) => c.id === connectionId)?.cachedModels ?? [];
  }

  function toggleMenu(index: number, row: ApiChannel) {
    if (!row.connectionId) return;
    setOpenMenu((cur) => (cur === index ? null : index));
  }

  return (
    <div className="channels-editor" aria-label="渠道编辑器">
      <div className="channels-header">
        <span className="adv-label">模型渠道 <em>{enabledCount || 0}/{MAX_CHANNELS}</em></span>
        <div className="channel-header-actions">
          <button className="text-button" onClick={() => setExpanded((value) => !value)} aria-expanded={expanded}>{expanded ? <ChevronsUp size={14} /> : <Plus size={14} />}{expanded ? "收起备用" : "添加备用渠道"}</button>
          <button className="button small" onClick={onManage}><Settings2 size={14} />管理连接</button>
        </div>
      </div>
      {rows.map((row, i) => (i === 0 || expanded) && (
        <div className="channel-row" key={i}>
          <span className="channel-idx">{i === 0 ? "主渠道" : `备用 ${i}`}</span>
          <select
            aria-label={`渠道 ${i + 1} 连接`}
            value={row.connectionId}
            disabled={disabled}
            onChange={(e) => {
              const id = e.target.value;
              setOpenMenu(null);
              updateRow(i, {
                connectionId: id,
                modelId: id ? (connections.find((c) => c.id === id)?.lastModel ?? "") : "",
              });
            }}
          >
            <option value="">不使用</option>
            {connections.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
          </select>
          <div className="model-cell" ref={(el) => { menuRefs.current[i] = el; }}>
            <input
              aria-label={`渠道 ${i + 1} 模型`}
              placeholder={row.connectionId ? "选择或输入模型 ID" : "先选择连接"}
              value={row.modelId}
              disabled={disabled || !row.connectionId}
              onChange={(e) => updateRow(i, { modelId: e.target.value })}
            />
            <button
              className="icon-button mini"
              title="获取模型列表"
              disabled={disabled || !row.connectionId || fetching === row.connectionId}
              onClick={() => void fetchModels(row, i)}
            ><RefreshCcw size={13} className={fetching === row.connectionId ? "spin" : ""} /></button>
            <button
              className="icon-button mini"
              title="模型选单"
              disabled={disabled || !row.connectionId}
              onClick={() => toggleMenu(i, row)}
            ><ChevronDown size={13} /></button>
            {openMenu === i && (
              <div className="channel-model-menu" ref={(el) => { menuRefs.current[i] = el; }}>
                {modelsFor(row.connectionId).map((m) => (
                  <button key={m.id} className="menu-item" onClick={() => { updateRow(i, { modelId: m.id }); setOpenMenu(null); }}><span className="model-id">{m.id}</span><small>{m.name}</small></button>
                ))}
                {modelsFor(row.connectionId).length === 0 && <div className="muted">无模型缓存，点击刷新获取</div>}
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
