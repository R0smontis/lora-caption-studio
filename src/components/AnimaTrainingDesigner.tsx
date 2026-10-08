import { useEffect, useMemo, useState } from "react";
import { Calculator, Check, Copy, Gauge, Images, Layers3, Repeat2, Sparkles } from "lucide-react";
import { designAnimaTraining, type AnimaLoraKind, type AnimaTrainingStrength } from "../lib/animaTraining";

const KIND_HELP: Record<AnimaLoraKind, string> = {
  character: "学习单个角色的身份、外观与服装；数据变化应集中在姿态、表情和场景。",
  style: "学习稳定的线条、上色、构图和质感；应包含不同角色与不同题材。",
  concept: "学习服装、物件或独立视觉概念；避免让单一人物与概念绑定。",
  pose: "学习动作、镜头或构图规律；主体身份和画风应尽量多样。",
};

const ANIMA_DESIGNER_MEMORY_KEY = "lora-caption-studio.anima-designer.v1";

function readDesignerMemory() {
  try {
    const value = JSON.parse(localStorage.getItem(ANIMA_DESIGNER_MEMORY_KEY) ?? "null") as Partial<{ imageCount: number; batchSize: number; kind: AnimaLoraKind; strength: AnimaTrainingStrength }> | null;
    return value && typeof value === "object" ? value : {};
  } catch {
    return {};
  }
}

export function AnimaTrainingDesigner({ currentDatasetCount = 0 }: { currentDatasetCount?: number }) {
  const memory = useMemo(readDesignerMemory, []);
  const [imageCount, setImageCount] = useState(memory.imageCount ?? (currentDatasetCount > 0 ? currentDatasetCount : 30));
  const [batchSize, setBatchSize] = useState(memory.batchSize ?? 2);
  const [kind, setKind] = useState<AnimaLoraKind>(memory.kind ?? "character");
  const [strength, setStrength] = useState<AnimaTrainingStrength>(memory.strength ?? "balanced");
  const [copied, setCopied] = useState(false);
  const plan = useMemo(() => designAnimaTraining({ imageCount, batchSize, kind, strength }), [imageCount, batchSize, kind, strength]);

  useEffect(() => {
    localStorage.setItem(ANIMA_DESIGNER_MEMORY_KEY, JSON.stringify({ imageCount, batchSize, kind, strength }));
  }, [imageCount, batchSize, kind, strength]);

  async function copyConfig() {
    await navigator.clipboard.writeText(plan.toml);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  }

  return <div className="anima-designer-page">
    <section className="anima-source-note">
      <Sparkles size={18} />
      <div><b>面向 Anima-Base v1.0</b><span>官方模型卡指定 LoRA 使用 Base 版本训练；计算结果是首轮基线，建议每个 epoch 保存并对照样图选择停止点。</span></div>
    </section>

    <section className="anima-input-card">
      <div className="anima-section-title"><Calculator size={18} /><div><h2>训练规模</h2><p>Batch Size 完全采用你的指定值，设计器据此计算 epoch 与 repeat。</p></div></div>
      <div className="anima-input-grid">
        <label>图片数量<input aria-label="Anima 图片数量" type="number" min={1} max={100000} value={imageCount} onChange={(event) => setImageCount(Math.min(100000, Math.max(1, Math.floor(Number(event.target.value)))))} /></label>
        <label>LoRA 类型<select aria-label="Anima LoRA 类型" value={kind} onChange={(event) => setKind(event.target.value as AnimaLoraKind)}><option value="character">角色</option><option value="style">画风</option><option value="concept">服装 / 物件 / 概念</option><option value="pose">姿态 / 构图</option></select></label>
        <label>Batch Size<input aria-label="Anima Batch Size" type="number" min={1} max={64} value={batchSize} onChange={(event) => setBatchSize(Math.min(64, Math.max(1, Math.floor(Number(event.target.value)))))} /></label>
        <label>训练强度<select aria-label="Anima 训练强度" value={strength} onChange={(event) => setStrength(event.target.value as AnimaTrainingStrength)}><option value="conservative">保守 · 80%</option><option value="balanced">均衡 · 推荐</option><option value="strong">加强 · 115%</option></select></label>
      </div>
      <div className="anima-data-hint">
        <p className="anima-kind-help">{KIND_HELP[kind]}</p>
        {currentDatasetCount > 0 && <button type="button" className="text-button" onClick={() => setImageCount(currentDatasetCount)}>使用打标页当前 {currentDatasetCount} 张</button>}
      </div>
    </section>

    <section className="anima-plan-grid" aria-label="Anima 训练参数建议">
      <article><span><Layers3 size={17} />Epoch</span><strong>{plan.epochs}</strong><small>保存间隔建议 1 epoch</small></article>
      <article><span><Repeat2 size={17} />Repeat</span><strong>{plan.repeats}</strong><small>每张图总共参与 {plan.presentationsPerImage} 次</small></article>
      <article><span><Images size={17} />Batch Size</span><strong>{plan.batchSize}</strong><small>用户指定，不自动修改</small></article>
      <article><span><Gauge size={17} />预计 Steps</span><strong>{plan.totalSteps.toLocaleString()}</strong><small>{plan.stepsPerEpoch} steps / epoch · 目标约 {plan.targetSteps}</small></article>
    </section>

    <section className="anima-detail-grid">
      <div className="anima-explanation-card">
        <h3>计算说明</h3>
        <div className="anima-formula"><code>steps / epoch = ceil(图片数 × repeat ÷ batch)</code><code>total steps = steps / epoch × epoch</code></div>
        <dl><div><dt>样本总呈现次数</dt><dd>{plan.totalPresentations.toLocaleString()}</dd></div><div><dt>目标步数</dt><dd>{plan.targetSteps.toLocaleString()}</dd></div><div><dt>实际偏差</dt><dd>{Math.round((plan.totalSteps / plan.targetSteps - 1) * 100)}%</dd></div></dl>
        {plan.warnings.length > 0 && <div className="anima-warnings">{plan.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div>}
      </div>
      <div className="anima-config-card">
        <div className="anima-config-header"><div><h3>数据集 TOML 片段</h3><small>兼容 Anima LoRA 工具的数据集字段</small></div><button className="button ghost small" onClick={() => void copyConfig()}>{copied ? <Check size={14} /> : <Copy size={14} />}{copied ? "已复制" : "复制配置"}</button></div>
        <pre>{plan.toml}</pre>
      </div>
    </section>

    <section className="anima-references">
      <b>设计依据</b>
      <a href="https://huggingface.co/circlestone-labs/Anima/blob/main/README.md" target="_blank" rel="noreferrer">Anima 官方模型卡</a>
      <a href="https://github.com/sorryhyun/anima_lora/blob/main/docs/guidelines/base-config.md" target="_blank" rel="noreferrer">Anima LoRA 工具配置说明</a>
      <a href="https://huggingface.co/circlestone-labs/Anima/discussions/106" target="_blank" rel="noreferrer">社区训练参数讨论</a>
      <span>提示：Anima 权重及其衍生训练结果受 CircleStone Labs 非商业许可约束。</span>
    </section>
  </div>;
}
