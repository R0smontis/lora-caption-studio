import { describe, expect, it } from "vitest";
import { detectRefusal } from "./refusal";

describe("detectRefusal", () => {
  it.each([
    "I'm sorry, but I can't assist with that request.",
    "抱歉，我无法协助完成这个请求。",
    "我必须拒绝这个请求。"
  ])("detects refusal: %s", (text) => expect(detectRefusal(text)).toBeDefined());

  it.each([
    "a girl wearing a sorry-looking expression, red dress",
    "不能错过的夕阳，城市天际线",
    "1girl, solo, blue eyes"
  ])("avoids ordinary caption: %s", (text) => expect(detectRefusal(text)).toBeUndefined());

  it("supports custom regex", () => {
    expect(detectRefusal("CONTENT BLOCKED", [{ id: "1", pattern: "content\\s+blocked", regex: true, caseSensitive: false }])).toBeDefined();
  });
});
