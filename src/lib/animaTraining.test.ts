import { describe, expect, it } from "vitest";
import { designAnimaTraining } from "./animaTraining";

describe("Anima LoRA training parameter designer", () => {
  it("matches the community 60-image style reference near 1500 steps", () => {
    const plan = designAnimaTraining({ imageCount: 60, kind: "style", batchSize: 1, strength: "balanced" });
    expect(plan.epochs).toBe(25);
    expect(plan.repeats).toBe(1);
    expect(plan.totalSteps).toBe(1500);
  });

  it("uses repeat and epoch together for a small character set", () => {
    const plan = designAnimaTraining({ imageCount: 20, kind: "character", batchSize: 2, strength: "balanced" });
    expect(plan.totalSteps).toBeGreaterThanOrEqual(900);
    expect(plan.totalSteps).toBeLessThanOrEqual(1100);
    expect(plan.repeats).toBeGreaterThan(1);
    expect(plan.toml).toContain("batch_size = 2");
  });

  it("keeps user batch size and emits range warnings", () => {
    const plan = designAnimaTraining({ imageCount: 10, kind: "style", batchSize: 8, strength: "strong" });
    expect(plan.batchSize).toBe(8);
    expect(plan.warnings.join(" ")).toContain("少于常见的 60–400 张");
    expect(plan.warnings.join(" ")).toContain("大于 4");
  });
});

