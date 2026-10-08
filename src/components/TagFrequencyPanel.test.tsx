import { describe, expect, it } from "vitest";
import { buildTagFrequencies } from "./TagFrequencyPanel";
import type { ImageJob } from "../types";

const job = (id: string, caption: string): ImageJob => ({
  id, imagePath: `${id}.png`, captionPath: `${id}.txt`, fileName: `${id}.png`,
  caption, status: "succeeded", selected: false,
});

describe("buildTagFrequencies", () => {
  it("按包含标签的图片数统计，并忽略同图重复与大小写", () => {
    const result = buildTagFrequencies([
      job("a", "1girl, blue eyes, 1GIRL"),
      job("b", "1girl, smile"),
      job("c", ""),
    ]);
    expect(result[0]).toMatchObject({ tag: "1girl", imageCount: 2, occurrenceCount: 3 });
    expect(result[0].percentage).toBeCloseTo(66.666, 2);
    expect(result.find((item) => item.tag === "blue eyes")?.imageCount).toBe(1);
  });

  it("忽略空标签并按频率降序", () => {
    expect(buildTagFrequencies([job("a", "solo, , smile"), job("b", "smile")]).map((item) => item.tag)).toEqual(["smile", "solo"]);
  });
});
