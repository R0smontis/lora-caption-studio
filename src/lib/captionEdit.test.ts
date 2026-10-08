import { describe, expect, it } from "vitest";
import { applyCaptionEdit } from "./captionEdit";
import type { CaptionEditRule } from "../types";

const base: CaptionEditRule = {
  mode: "tag",
  action: "insert",
  position: "end",
  content: "masterpiece",
  regex: false,
  caseSensitive: false,
  allMatches: false,
  deduplicate: true
};

describe("applyCaptionEdit", () => {
  it("inserts and deduplicates tags", () => {
    expect(applyCaptionEdit("1girl, solo, masterpiece", base)).toBe("1girl, solo, masterpiece");
  });

  it("inserts before anchor", () => {
    expect(applyCaptionEdit("1girl, outdoors", { ...base, position: "before_anchor", anchor: "outdoors", content: "smile" })).toBe("1girl, smile, outdoors");
  });

  it("deletes all matching tags", () => {
    expect(applyCaptionEdit("bad, good, bad", { ...base, action: "delete", content: "bad", allMatches: true })).toBe("good");
  });

  it("supports text prefix and regex replacement", () => {
    expect(applyCaptionEdit("hello world", { ...base, mode: "text", position: "start", content: "note: " })).toBe("note: hello world");
    expect(applyCaptionEdit("foo 123 bar", { ...base, mode: "text", action: "replace", content: "\\d+", replacement: "N", regex: true })).toBe("foo N bar");
  });
});
