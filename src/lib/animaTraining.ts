export type AnimaLoraKind = "character" | "style" | "concept" | "pose";
export type AnimaTrainingStrength = "conservative" | "balanced" | "strong";

export interface AnimaTrainingInput {
  imageCount: number;
  kind: AnimaLoraKind;
  batchSize: number;
  strength: AnimaTrainingStrength;
}

export interface AnimaTrainingPlan {
  imageCount: number;
  kind: AnimaLoraKind;
  batchSize: number;
  epochs: number;
  repeats: number;
  stepsPerEpoch: number;
  totalSteps: number;
  totalPresentations: number;
  presentationsPerImage: number;
  targetSteps: number;
  warnings: string[];
  toml: string;
}

const KIND_LABELS: Record<AnimaLoraKind, string> = {
  character: "角色",
  style: "画风",
  concept: "服装 / 物件 / 概念",
  pose: "姿态 / 构图",
};

function balancedTargetSteps(kind: AnimaLoraKind, images: number) {
  if (kind === "character") {
    if (images < 15) return 800;
    if (images <= 30) return 1000;
    if (images <= 60) return 1200;
    if (images <= 120) return 1400;
    return 1600;
  }
  if (kind === "style") {
    if (images < 30) return 1200;
    if (images <= 80) return 1500;
    if (images <= 200) return 1800;
    return 2000;
  }
  if (kind === "concept") {
    if (images < 15) return 700;
    if (images <= 40) return 900;
    if (images <= 100) return 1200;
    return 1500;
  }
  if (images < 20) return 650;
  if (images <= 60) return 850;
  if (images <= 150) return 1100;
  return 1300;
}

function preferredEpochs(kind: AnimaLoraKind) {
  return kind === "style" ? 25 : kind === "concept" ? 12 : 10;
}

/**
 * 在可读的 4–30 epochs 与 1–20 repeats 中搜索最接近目标 optimizer steps 的组合。
 * Anima 官方工具把 repeat 默认设为 1；仅在小数据集无法用合理 epoch 达到目标步数时提高 repeat。
 */
export function designAnimaTraining(input: AnimaTrainingInput): AnimaTrainingPlan {
  const imageCount = Math.min(100_000, Math.max(1, Math.floor(input.imageCount)));
  const batchSize = Math.min(64, Math.max(1, Math.floor(input.batchSize)));
  const factor = input.strength === "conservative" ? 0.8 : input.strength === "strong" ? 1.15 : 1;
  const targetSteps = Math.min(2200, Math.max(400, Math.round(balancedTargetSteps(input.kind, imageCount) * factor / 25) * 25));
  const preferred = preferredEpochs(input.kind);
  const repeatSoftLimit = imageCount < 15 ? 20 : imageCount < 30 ? 12 : imageCount < 60 ? 8 : imageCount < 120 ? 4 : 2;
  let best = { epochs: preferred, repeats: 1, stepsPerEpoch: Math.ceil(imageCount / batchSize), totalSteps: 0, score: Number.POSITIVE_INFINITY };

  for (let repeats = 1; repeats <= 20; repeats += 1) {
    const stepsPerEpoch = Math.max(1, Math.ceil((imageCount * repeats) / batchSize));
    for (let epochs = 4; epochs <= 30; epochs += 1) {
      const totalSteps = stepsPerEpoch * epochs;
      const deviation = Math.abs(totalSteps - targetSteps) / targetSteps;
      const epochPenalty = Math.abs(epochs - preferred) * 0.0025;
      const repeatPenalty = repeats * (input.kind === "style" ? 0.0035 : 0.0018);
      const excessiveRepeatPenalty = repeats > repeatSoftLimit ? (repeats - repeatSoftLimit) * 0.04 : 0;
      const score = deviation + epochPenalty + repeatPenalty + excessiveRepeatPenalty;
      if (score < best.score) best = { epochs, repeats, stepsPerEpoch, totalSteps, score };
    }
  }

  const totalPresentations = imageCount * best.repeats * best.epochs;
  const warnings: string[] = [];
  if (input.kind === "character" && imageCount < 15) warnings.push("角色数据少于 15 张：优先增加姿态、表情、角度和背景变化，并关注过拟合。");
  if (input.kind === "character" && imageCount > 50) warnings.push("角色数据超过常见的 15–50 张范围：确认没有重复帧，并保持角色身份标签一致。");
  if (input.kind === "style" && imageCount < 60) warnings.push("画风数据少于常见的 60–400 张范围：减少重复构图，避免模型把单个角色学成画风。");
  if (input.kind === "style" && imageCount > 400) warnings.push("画风数据超过 400 张：建议先去重和抽样，确保风格一致性高于单纯数量。");
  if (batchSize === 1) warnings.push("Batch Size 1 可用，但社区实测更容易波动；显存允许时可尝试 2 或 4。");
  if (batchSize > 4) warnings.push("Batch Size 大于 4 的收益通常递减，并可能需要同步校准学习率。");
  if (best.totalSteps > 2000) warnings.push("计划超过 2000 steps：请逐 epoch 查看样图，出现构图复制或提示词响应下降时提前停止。");

  const toml = `# Anima LoRA 参数设计器\n# 类型：${KIND_LABELS[input.kind]}；图片：${imageCount}\nmax_train_epochs = ${best.epochs}\nsave_every_n_epochs = 1\n\n[[datasets]]\nbatch_size = ${batchSize}\n\n  [[datasets.subsets]]\n  num_repeats = ${best.repeats}\n  recursive = true`;

  return {
    imageCount,
    kind: input.kind,
    batchSize,
    epochs: best.epochs,
    repeats: best.repeats,
    stepsPerEpoch: best.stepsPerEpoch,
    totalSteps: best.totalSteps,
    totalPresentations,
    presentationsPerImage: best.repeats * best.epochs,
    targetSteps,
    warnings,
    toml,
  };
}

