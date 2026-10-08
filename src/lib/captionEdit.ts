import type { CaptionEditRule } from "../types";

function makeRegExp(pattern: string, rule: CaptionEditRule): RegExp {
  const flags = `${rule.caseSensitive ? "" : "i"}${rule.allMatches ? "g" : ""}${rule.mode === "text" ? "m" : ""}`;
  return new RegExp(rule.regex ? pattern : pattern.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), flags);
}

function dedupe(tags: string[], caseSensitive: boolean): string[] {
  const seen = new Set<string>();
  return tags.filter((tag) => {
    const key = caseSensitive ? tag : tag.toLocaleLowerCase();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function applyTagRule(input: string, rule: CaptionEditRule): string {
  let tags = input.split(",").map((item) => item.trim()).filter(Boolean);
  const inserted = rule.content.split(",").map((item) => item.trim()).filter(Boolean);
  const matches = (tag: string) => makeRegExp(rule.anchor || rule.content, { ...rule, allMatches: false }).test(tag);

  if (rule.action === "delete") {
    let removed = false;
    tags = tags.filter((tag) => {
      if (matches(tag) && (rule.allMatches || !removed)) {
        removed = true;
        return false;
      }
      return true;
    });
  } else if (rule.action === "replace") {
    let replaced = false;
    tags = tags.flatMap((tag) => {
      if (matches(tag) && (rule.allMatches || !replaced)) {
        replaced = true;
        return (rule.replacement ?? "").split(",").map((v) => v.trim()).filter(Boolean);
      }
      return [tag];
    });
  } else {
    if (rule.position === "start") tags.unshift(...inserted);
    else if (rule.position === "end") tags.push(...inserted);
    else if (rule.position === "index") tags.splice(Math.max(0, Math.min(tags.length, (rule.index ?? 1) - 1)), 0, ...inserted);
    else {
      const found = tags.reduce<number[]>((out, tag, index) => matches(tag) ? [...out, index] : out, []);
      const targets = rule.allMatches ? found : found.slice(0, 1);
      let offset = 0;
      for (const target of targets) {
        const at = target + offset + (rule.position === "after_anchor" ? 1 : 0);
        tags.splice(at, 0, ...inserted);
        offset += inserted.length;
      }
    }
  }
  if (rule.deduplicate) tags = dedupe(tags, rule.caseSensitive);
  return tags.join(", ");
}

function applyTextRule(input: string, rule: CaptionEditRule): string {
  if (rule.action === "delete") return input.replace(makeRegExp(rule.content, rule), "");
  if (rule.action === "replace") return input.replace(makeRegExp(rule.content, rule), rule.replacement ?? "");
  if (rule.position === "start") return rule.content + input;
  if (rule.position === "end") return input + rule.content;
  if (rule.position === "index") {
    const index = Math.max(0, Math.min(input.length, rule.index ?? 0));
    return input.slice(0, index) + rule.content + input.slice(index);
  }
  if (rule.position === "line") {
    const lines = input.split(/\r?\n/);
    lines.splice(Math.max(0, Math.min(lines.length, (rule.index ?? 1) - 1)), 0, rule.content);
    return lines.join("\n");
  }
  const anchor = makeRegExp(rule.anchor ?? "", rule);
  return input.replace(anchor, (match) => rule.position === "before_anchor" ? rule.content + match : match + rule.content);
}

export function applyCaptionEdit(input: string, rule: CaptionEditRule): string {
  return rule.mode === "tag" ? applyTagRule(input, rule) : applyTextRule(input, rule);
}
