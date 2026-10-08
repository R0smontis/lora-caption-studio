import { useEffect, useMemo, useRef, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { Check, ChevronDown, Cloud, PlugZap, RefreshCw, Search, Settings2 } from "lucide-react";
import { backend } from "../lib/backend";
import type { ApiConnection, ModelOption } from "../types";

interface Props {
  connections: ApiConnection[];
  connectionId: string;
  modelId: string;
  disabled?: boolean;
  onConnectionChange: (id: string) => void;
  onModelChange: (model: string) => void;
  onManage: () => void;
}

/**
 * Unified API/model selection used by the task bar, detail retry and any
 * future retry flows. The connection menu is searchable; the model combobox
 * supports search, click-to-pick, manual input and forced refresh. Models
 * fetched here are cached into the connection record so switching back
 * restores the last state.
 */
export function ApiModelSelector({ connections, connectionId, modelId, disabled, onConnectionChange, onModelChange, onManage }: Props) {
  const current = connections.find((item) => item.id === connectionId);
  const [models, setModels] = useState<ModelOption[]>(current?.cachedModels ?? []);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [connectionQuery, setConnectionQuery] = useState("");
  const [modelQuery, setModelQuery] = useState("");
  const [modelFocused, setModelFocused] = useState(false);
  const [open, setOpen] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string }>();
  const inputRef = useRef<HTMLInputElement>(null);

  async function refresh(force = false) {
    if (!connectionId) return;
    setLoading(true);
    setError("");
    try {
      const fetched = await backend.listModels(connectionId, force);
      setModels(fetched);
    } catch (reason) {
      setModels(current?.cachedModels ?? []);
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function testConnection() {
    if (!connectionId || testing) return;
    setTesting(true);
    setTestResult(undefined);
    try {
      const result = await backend.testConnection(connectionId);
      setTestResult({
        ok: result.ok,
        text: result.ok
          ? `连接正常 · ${result.latencyMs ?? "—"} ms · 发现 ${result.modelsFetched} 个模型${result.sampleModels.length ? `（如 ${result.sampleModels.slice(0, 3).join("、")}…）` : ""}`
          : `连接失败：${result.error ?? "未知错误"}`,
      });
      if (result.ok && result.modelsFetched > 0) {
        setModels((currentModels) => currentModels.length ? currentModels : result.sampleModels.map((id) => ({ id, name: id })));
      }
    } catch (reason) {
      setTestResult({ ok: false, text: `连接测试失败：${String(reason)}` });
    } finally {
      setTesting(false);
    }
  }

  useEffect(() => {
    setModels(current?.cachedModels ?? []);
    setError("");
    setTestResult(undefined);
    if (current && (!current.cachedModels?.length || !current.modelsCachedAt || Date.now() - current.modelsCachedAt > 600_000)) {
      void refresh(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId]);

  const filteredConnections = useMemo(() => {
    const query = connectionQuery.trim().toLocaleLowerCase();
    if (!query) return connections;
    return connections.filter((connection) =>
      connection.name.toLocaleLowerCase().includes(query) ||
      connection.baseUrl.toLocaleLowerCase().includes(query)
    );
  }, [connections, connectionQuery]);

  const filteredModels = useMemo(() => {
    const query = modelQuery.trim().toLocaleLowerCase();
    if (!query) return models;
    return models.filter((model) =>
      model.id.toLocaleLowerCase().includes(query) || model.name.toLocaleLowerCase().includes(query)
    );
  }, [models, modelQuery]);

  function pickModel(id: string) {
    onModelChange(id);
    setModelQuery("");
    inputRef.current?.focus();
  }

  return (
    <div className="api-model-selector" aria-label="API 与模型选择">
      <DropdownMenu.Root open={open} onOpenChange={setOpen}>
        <DropdownMenu.Trigger asChild>
          <button className="api-trigger" disabled={disabled} aria-label="选择 API 连接">
            <Cloud size={16} />
            <span>{current?.name ?? "选择 API"}</span>
            <ChevronDown size={14} />
          </button>
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content className="dropdown-content" sideOffset={8} align="start" onCloseAutoFocus={(event) => event.preventDefault()}>
            <div className="dropdown-label">API 连接</div>
            <div className="dropdown-search">
              <Search size={14} />
              <input
                value={connectionQuery}
                onChange={(event) => setConnectionQuery(event.target.value)}
                placeholder="搜索连接名称或地址…"
                autoFocus
              />
            </div>
            {filteredConnections.length === 0 && <div className="dropdown-empty">未找到匹配的连接</div>}
            {filteredConnections.map((connection) => (
              <DropdownMenu.Item
                className="dropdown-item"
                key={connection.id}
                onSelect={() => { onConnectionChange(connection.id); setConnectionQuery(""); }}
              >
                <span className={`provider-dot ${connection.provider}`} />
                <span>{connection.name}</span>
                <small>{connection.keyRefs.length} Keys</small>
                {connection.id === connectionId && <Check size={14} />}
              </DropdownMenu.Item>
            ))}
            <DropdownMenu.Separator className="dropdown-separator" />
            <DropdownMenu.Item className="dropdown-item" onSelect={() => { onManage(); setConnectionQuery(""); }}>
              <Settings2 size={15} /> 管理 API 连接
            </DropdownMenu.Item>
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
      <div className="model-combobox">
        <input
          ref={inputRef}
          aria-label="模型 ID"
          value={modelQuery || modelId}
          onChange={(event) => { setModelQuery(event.target.value); onModelChange(event.target.value); }}
          onFocus={() => { setModelFocused(true); setModelQuery(""); }}
          onBlur={() => setModelFocused(false)}
          placeholder={loading ? "正在获取模型…" : "选择或输入模型 ID"}
          disabled={!connectionId || disabled}
        />
        {(modelFocused || modelQuery) && (
          <div className="model-suggestions" role="listbox" aria-label="模型候选">
            {filteredModels.length === 0 && <div className="dropdown-empty">没有匹配的模型，可继续手动输入</div>}
            {filteredModels.map((model) => (
              <button
                role="option"
                aria-selected={model.id === modelId}
                key={model.id}
                className={model.id === modelId ? "active" : ""}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => pickModel(model.id)}
              >
                <b>{model.id}</b>
                {model.name !== model.id && <small>{model.name}</small>}
              </button>
            ))}
          </div>
        )}
        <button className="icon-button mini" onClick={() => void refresh(true)} disabled={!connectionId || loading || disabled} aria-label="刷新模型列表" title="刷新模型列表">
          <RefreshCw size={15} className={loading ? "spin" : ""} />
        </button>
      </div>
      <button className="icon-button mini" onClick={() => void testConnection()} disabled={!connectionId || testing || disabled} aria-label="测试连接" title="测试连接">
        <PlugZap size={15} className={testing ? "spin" : ""} />
      </button>
      {testResult && <span className={`selector-test ${testResult.ok ? "ok" : "fail"}`} title={testResult.text}>{testResult.text}</span>}
      {!testResult && error && <span className="selector-warning" title={error}>模型列表获取失败，可手动输入或点击测试</span>}
    </div>
  );
}
