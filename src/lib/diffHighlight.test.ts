import { describe, expect, it } from "vitest";
import { diffParts } from "./diffHighlight";

describe("diffParts", () => {
  it("highlights an appended tail", () => {
    const parts = diffParts("1girl, solo", "1girl, solo, smile");
    expect(parts.beforePrefix).toBe("1girl, solo");
    expect(parts.beforeChanged).toBe("");
    expect(parts.afterPrefix).toBe("1girl, solo");
    expect(parts.afterChanged).toBe(", smile");
    expect(parts.changed).toBe(true);
  });

  it("highlights a deleted middle segment", () => {
    const parts = diffParts("1girl, bad, solo", "1girl, solo");
    // 公共前缀 "1girl, "，删除段为 "bad, "（含分隔符），剩余 "solo"。
    expect(parts.beforePrefix).toBe("1girl, ");
    expect(parts.beforeChanged).toBe("bad, ");
    expect(parts.beforeSuffix).toBe("solo");
    expect(parts.afterPrefix).toBe("1girl, ");
    expect(parts.afterChanged).toBe("");
    expect(parts.afterSuffix).toBe("solo");
  });

  it("highlights a replaced middle segment", () => {
    const parts = diffParts("foo 12 bar", "foo N bar");
    expect(parts.beforeChanged).toBe("12");
    expect(parts.afterChanged).toBe("N");
  });

  it("handles identical and empty inputs", () => {
    expect(diffParts("same", "same").changed).toBe(false);
    const empty = diffParts("", "");
    expect(empty.changed).toBe(false);
    expect(empty.afterChanged).toBe("");
  });

  it("handles chinese characters as code points", () => {
    const parts = diffParts("1girl, 长发", "1girl, 短发");
    expect(parts.beforeChanged).toBe("长");
    expect(parts.afterChanged).toBe("短");
  });
});
