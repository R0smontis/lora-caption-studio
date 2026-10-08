import type { RefusalMatch, RefusalRule } from "../types";

export interface BuiltinRuleInfo {
  label: string;
  source: string;
}

/** 内置高置信拒绝规则（只读展示用）。 */
export const BUILTIN_RULE_INFO: BuiltinRuleInfo[] = [
  { label: "英文：I can't / am unable to help…", source: String(/\bi (?:can(?:not|'t)|am unable to) (?:help|assist|comply|provide|fulfill)/i) },
  { label: "英文：I must decline / refuse", source: String(/\bi must (?:decline|refuse)/i) },
  { label: "英文：this request cannot be…", source: String(/\bthis request (?:is not something i can|cannot be)\b/i) },
  { label: "中文：无法/不能/不便…协助/帮助/完成…", source: String(/(?:无法|不能|不便)(?:为你|向你|对此)?(?:协助|帮助|完成|提供|执行|生成|回答)/) },
  { label: "中文：我必须/需要…拒绝/婉拒", source: String(/(?:我必须|需要)(?:拒绝|婉拒)/) },
  { label: "中文：该请求/这个请求…无法处理/完成/满足", source: String(/(?:该请求|这个请求).{0,16}(?:无法|不能)(?:处理|完成|满足)/) },
];

const HIGH_CONFIDENCE = [
  /\bi (?:can(?:not|'t)|am unable to) (?:help|assist|comply|provide|fulfill)/i,
  /\bi must (?:decline|refuse)/i,
  /\bthis request (?:is not something i can|cannot be)\b/i,
  /(?:无法|不能|不便)(?:为你|向你|对此)?(?:协助|帮助|完成|提供|执行|生成|回答)/,
  /(?:我必须|需要)(?:拒绝|婉拒)/,
  /(?:该请求|这个请求).{0,16}(?:无法|不能)(?:处理|完成|满足)/
];

const APOLOGY = /(?:抱歉|对不起|很遗憾|i(?:'m| am) sorry|apologies)/i;
const INABILITY = /(?:无法|不能|不便|拒绝|不可以|can't|cannot|unable|decline|refuse)/i;

function normalize(value: string): string {
  return value.normalize("NFKC").replace(/[\s\u00a0]+/g, " ").trim();
}

function excerpt(text: string, index: number, length: number): string {
  const start = Math.max(0, index - 24);
  const end = Math.min(text.length, index + length + 48);
  return text.slice(start, end);
}

export function detectRefusal(
  text: string,
  customRules: RefusalRule[] = [],
  structuredReason?: string
): RefusalMatch | undefined {
  if (structuredReason) {
    return { rule: structuredReason, excerpt: structuredReason, structured: true };
  }
  const normalized = normalize(text);
  for (const pattern of HIGH_CONFIDENCE) {
    const match = pattern.exec(normalized);
    if (match) {
      return { rule: pattern.source, excerpt: excerpt(normalized, match.index, match[0].length), structured: false };
    }
  }
  const prefix = normalized.slice(0, 180);
  if (APOLOGY.test(prefix) && INABILITY.test(prefix)) {
    return { rule: "apology+inability", excerpt: prefix, structured: false };
  }
  for (const rule of customRules) {
    try {
      if (rule.regex) {
        const flags = rule.caseSensitive ? "u" : "iu";
        const match = new RegExp(rule.pattern, flags).exec(normalized);
        if (match) return { rule: rule.pattern, excerpt: excerpt(normalized, match.index, match[0].length), structured: false };
      } else {
        const haystack = rule.caseSensitive ? normalized : normalized.toLocaleLowerCase();
        const needle = rule.caseSensitive ? normalize(rule.pattern) : normalize(rule.pattern).toLocaleLowerCase();
        const index = haystack.indexOf(needle);
        if (needle && index >= 0) return { rule: rule.pattern, excerpt: excerpt(normalized, index, needle.length), structured: false };
      }
    } catch {
      // Invalid user regex is ignored here and validated in the preset editor.
    }
  }
  return undefined;
}
