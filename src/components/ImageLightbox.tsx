import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { FileImage, LoaderCircle, Maximize2, RotateCcw, RotateCw, Scan, X, ZoomIn, ZoomOut } from "lucide-react";
import { backend } from "../lib/backend";

interface Props {
  path?: string;
  name?: string;
  thumbnail?: string;
  onClose: () => void;
}

interface ViewTransform {
  scale: number;
  rotation: number;
  x: number;
  y: number;
}

interface Point { x: number; y: number }

const INITIAL_VIEW: ViewTransform = { scale: 1, rotation: 0, x: 0, y: 0 };

/**
 * 共用画布式大图查看器：默认完整适配，支持连续滚轮/双指缩放、拖拽平移、旋转、
 * 100% 像素查看与随窗口重新适配。所有变换只作用于预览，不修改源图片。
 */
export function ImageLightbox({ path, name, thumbnail, onClose }: Props) {
  const [preview, setPreview] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [view, setView] = useState<ViewTransform>(INITIAL_VIEW);
  const [imageReady, setImageReady] = useState(false);
  const [dragging, setDragging] = useState(false);
  const stageRef = useRef<HTMLDivElement>(null);
  const imageSizeRef = useRef({ width: 0, height: 0 });
  const fitScaleRef = useRef(1);
  const fitModeRef = useRef(true);
  const pointersRef = useRef(new Map<number, Point>());
  const gestureRef = useRef<{ midpoint: Point; distance?: number } | undefined>(undefined);

  function stageMetrics() {
    const rect = stageRef.current?.getBoundingClientRect();
    return {
      left: rect?.left ?? 0,
      top: rect?.top ?? 0,
      width: rect?.width || Math.max(320, window.innerWidth - 36),
      height: rect?.height || Math.max(240, window.innerHeight - 150),
    };
  }

  function fitToView(rotation = view.rotation, reveal = true) {
    const image = imageSizeRef.current;
    if (!image.width || !image.height) return;
    const stage = stageMetrics();
    const sideways = Math.abs(rotation % 180) === 90;
    const width = sideways ? image.height : image.width;
    const height = sideways ? image.width : image.height;
    const scale = Math.max(0.001, Math.min((stage.width - 24) / width, (stage.height - 24) / height));
    fitScaleRef.current = scale;
    fitModeRef.current = true;
    setView({ scale, rotation, x: 0, y: 0 });
    if (reveal) setImageReady(true);
  }

  function clampScale(value: number) {
    return Math.min(100, Math.max(Math.max(0.0005, fitScaleRef.current * 0.05), value));
  }

  function zoomAt(clientX: number, clientY: number, requestedScale: number) {
    const stage = stageMetrics();
    const px = clientX - stage.left - stage.width / 2;
    const py = clientY - stage.top - stage.height / 2;
    fitModeRef.current = false;
    setView((current) => {
      const scale = clampScale(requestedScale);
      const ratio = scale / current.scale;
      return {
        ...current,
        scale,
        x: px - (px - current.x) * ratio,
        y: py - (py - current.y) * ratio,
      };
    });
  }

  function zoomFromCenter(factor: number) {
    const stage = stageMetrics();
    zoomAt(stage.left + stage.width / 2, stage.top + stage.height / 2, view.scale * factor);
  }

  function actualSize() {
    fitModeRef.current = false;
    setView((current) => ({ ...current, scale: 1, x: 0, y: 0 }));
  }

  function rotate(delta: number) {
    const rotation = ((view.rotation + delta) % 360 + 360) % 360;
    fitToView(rotation);
  }

  function handleWheel(event: WheelEvent<HTMLDivElement>) {
    event.preventDefault();
    const normalizedDelta = event.deltaMode === 1 ? event.deltaY * 16 : event.deltaMode === 2 ? event.deltaY * window.innerHeight : event.deltaY;
    const factor = Math.exp(-normalizedDelta * 0.0015);
    zoomAt(event.clientX, event.clientY, view.scale * factor);
  }

  function midpoint(points: Point[]): Point {
    return points.length === 1 ? points[0] : { x: (points[0].x + points[1].x) / 2, y: (points[0].y + points[1].y) / 2 };
  }

  function pointerDistance(points: Point[]) {
    return points.length < 2 ? undefined : Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y);
  }

  function handlePointerDown(event: PointerEvent<HTMLDivElement>) {
    event.currentTarget.setPointerCapture?.(event.pointerId);
    pointersRef.current.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...pointersRef.current.values()].slice(0, 2);
    gestureRef.current = { midpoint: midpoint(points), distance: pointerDistance(points) };
    setDragging(true);
  }

  function handlePointerMove(event: PointerEvent<HTMLDivElement>) {
    if (!pointersRef.current.has(event.pointerId)) return;
    pointersRef.current.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...pointersRef.current.values()].slice(0, 2);
    const previous = gestureRef.current;
    if (!previous || !points.length) return;
    const nextMidpoint = midpoint(points);
    const nextDistance = pointerDistance(points);
    fitModeRef.current = false;
    setView((current) => {
      if (nextDistance && previous.distance) {
        const stage = stageMetrics();
        const scale = clampScale(current.scale * (nextDistance / previous.distance));
        const ratio = scale / current.scale;
        const previousX = previous.midpoint.x - stage.left - stage.width / 2;
        const previousY = previous.midpoint.y - stage.top - stage.height / 2;
        const nextX = nextMidpoint.x - stage.left - stage.width / 2;
        const nextY = nextMidpoint.y - stage.top - stage.height / 2;
        return { ...current, scale, x: nextX - (previousX - current.x) * ratio, y: nextY - (previousY - current.y) * ratio };
      }
      return { ...current, x: current.x + nextMidpoint.x - previous.midpoint.x, y: current.y + nextMidpoint.y - previous.midpoint.y };
    });
    gestureRef.current = { midpoint: nextMidpoint, distance: nextDistance };
  }

  function handlePointerEnd(event: PointerEvent<HTMLDivElement>) {
    pointersRef.current.delete(event.pointerId);
    const points = [...pointersRef.current.values()].slice(0, 2);
    gestureRef.current = points.length ? { midpoint: midpoint(points), distance: pointerDistance(points) } : undefined;
    if (!points.length) setDragging(false);
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "+" || event.key === "=") { event.preventDefault(); zoomFromCenter(1.2); }
    else if (event.key === "-") { event.preventDefault(); zoomFromCenter(1 / 1.2); }
    else if (event.key === "0") { event.preventDefault(); fitToView(); }
    else if (event.key === "1") { event.preventDefault(); actualSize(); }
    else if (event.key.toLocaleLowerCase() === "r") { event.preventDefault(); rotate(event.shiftKey ? -90 : 90); }
  }

  useEffect(() => {
    if (!path) {
      setPreview(undefined);
      setError("");
      return;
    }
    let active = true;
    setPreview(undefined);
    setError("");
    setLoading(true);
    setImageReady(false);
    setView(INITIAL_VIEW);
    imageSizeRef.current = { width: 0, height: 0 };
    fitModeRef.current = true;
    const dimension = Math.min(8192, Math.max(2400, Math.ceil(Math.max(window.innerWidth, window.innerHeight) * (window.devicePixelRatio || 1) * 3)));
    void backend.getImagePreview(path, dimension).then(async (value) => {
      if (!active) return;
      if (value) {
        // 先让 WebView 完成高分辨率图片解码，再替换缩略图，避免 src 切换时出现白帧。
        const decoded = new Image();
        decoded.src = value;
        if (typeof decoded.decode === "function") await decoded.decode().catch(() => undefined);
        if (active) setPreview(value);
      }
      else setError("无法生成高分辨率预览，当前显示缩略图");
    }).catch((reason) => {
      if (active) setError(String(reason));
    }).finally(() => {
      if (active) setLoading(false);
    });
    return () => { active = false; };
  }, [path]);

  useEffect(() => {
    function handleResize() { if (fitModeRef.current) fitToView(); }
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [path, view.rotation]);

  const source = preview ?? thumbnail;
  const zoomPercent = Math.max(0.1, view.scale * 100);

  return (
    <Dialog.Root open={Boolean(path)} onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="image-lightbox-overlay" />
        <Dialog.Content className="image-lightbox-content" aria-describedby={undefined} onKeyDown={handleKeyDown}>
          <Dialog.Title className="image-lightbox-title">{name ?? "图片大图"}</Dialog.Title>
          <Dialog.Close className="image-lightbox-close" aria-label="关闭大图"><X size={20} /></Dialog.Close>
          <div className="image-lightbox-toolbar" role="toolbar" aria-label="图片查看工具">
            <button aria-label="完整显示" title="完整显示 (0)" onClick={() => fitToView()}><Maximize2 size={16} /></button>
            <button aria-label="实际大小" title="100% 实际大小 (1)" onClick={actualSize}><Scan size={16} /></button>
            <span className="lightbox-separator" />
            <button aria-label="缩小" title="缩小 (-)" onClick={() => zoomFromCenter(1 / 1.2)}><ZoomOut size={16} /></button>
            <output aria-label="当前缩放比例">{zoomPercent < 10 ? zoomPercent.toFixed(1) : Math.round(zoomPercent)}%</output>
            <button aria-label="放大" title="放大 (+)" onClick={() => zoomFromCenter(1.2)}><ZoomIn size={16} /></button>
            <span className="lightbox-separator" />
            <button aria-label="向左旋转" title="向左旋转 (Shift+R)" onClick={() => rotate(-90)}><RotateCcw size={16} /></button>
            <button aria-label="向右旋转" title="向右旋转 (R)" onClick={() => rotate(90)}><RotateCw size={16} /></button>
          </div>
          <div
            ref={stageRef}
            className={`image-lightbox-stage ${dragging ? "dragging" : ""}`}
            onWheel={handleWheel}
            onPointerDown={handlePointerDown}
            onPointerMove={handlePointerMove}
            onPointerUp={handlePointerEnd}
            onPointerCancel={handlePointerEnd}
            onDoubleClick={(event) => view.scale > fitScaleRef.current * 1.05 ? fitToView() : zoomAt(event.clientX, event.clientY, view.scale * 2)}
          >
            {source && <div className={`image-lightbox-canvas ${imageReady ? "ready" : ""}`} style={{ transform: `translate3d(${view.x}px, ${view.y}px, 0)` }}><img
              src={source}
              alt={name ?? "图片大图"}
              draggable={false}
              style={{ transform: `translate(-50%, -50%) rotate(${view.rotation}deg) scale(${view.scale})` }}
              onLoad={(event) => {
                imageSizeRef.current = { width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight };
                // 首次显示和视图变换在同一批 React 更新中提交，杜绝原尺寸图片闪现一帧。
                if (!imageReady) fitToView(view.rotation);
              }}
            /></div>}
            {loading && <div className="image-lightbox-loading"><LoaderCircle className="spin" size={24} />正在加载高分辨率大图…</div>}
            {!loading && !source && <div className="image-lightbox-error"><FileImage size={30} /><span>{error || "没有可用预览"}</span></div>}
          </div>
          <div className="image-lightbox-footer"><span title={path}>{path}</span><small>拖拽移动 · 滚轮/双指无级缩放 · 双击放大/复位 · R 旋转</small></div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
