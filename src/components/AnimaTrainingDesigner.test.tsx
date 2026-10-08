import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { AnimaTrainingDesigner } from "./AnimaTrainingDesigner";

const MEMORY_KEY = "lora-caption-studio.anima-designer.v1";

describe("AnimaTrainingDesigner memory", () => {
  beforeEach(() => localStorage.clear());

  it("restores the last user inputs instead of replacing them with the current dataset count", () => {
    localStorage.setItem(MEMORY_KEY, JSON.stringify({ imageCount: 60, batchSize: 1, kind: "style", strength: "strong" }));
    render(<AnimaTrainingDesigner currentDatasetCount={12} />);
    expect(screen.getByLabelText("Anima 图片数量")).toHaveValue(60);
    expect(screen.getByLabelText("Anima Batch Size")).toHaveValue(1);
    expect(screen.getByLabelText("Anima LoRA 类型")).toHaveValue("style");
    expect(screen.getByLabelText("Anima 训练强度")).toHaveValue("strong");
  });

  it("writes changed inputs to persistent memory", () => {
    render(<AnimaTrainingDesigner />);
    fireEvent.change(screen.getByLabelText("Anima 图片数量"), { target: { value: "88" } });
    fireEvent.change(screen.getByLabelText("Anima Batch Size"), { target: { value: "4" } });
    expect(JSON.parse(localStorage.getItem(MEMORY_KEY) ?? "{}")).toEqual(expect.objectContaining({ imageCount: 88, batchSize: 4 }));
  });
});
