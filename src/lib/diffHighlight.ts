/**
 * 简化的字符级差异高亮：剥离公共前缀/后缀后，中间段即变更区。
 * 对批量编辑（单点插入/删除/替换）效果良好；多处修改时整段高亮。
 */
export interface DiffParts {
  /** before 中保留的公共前缀 */
  beforePrefix: string;
  /** before 中被删除/修改的片段 */
  beforeChanged: string;
  /** before 中保留的公共后缀 */
  beforeSuffix: string;
  /** after 中保留的公共前缀 */
  afterPrefix: string;
  /** after 中新增/修改的片段 */
  afterChanged: string;
  /** after 中保留的公共后缀 */
  afterSuffix: string;
  /** 是否存在变更 */
  changed: boolean;
}

function commonPrefix(a: string[], b: string[]): number {
  let n = 0;
  while (n < a.length && n < b.length && a[n] === b[n]) n += 1;
  return n;
}

function commonSuffix(a: string[], b: string[]): number {
  let n = 0;
  while (n < a.length && n < b.length && a[a.length - 1 - n] === b[b.length - 1 - n]) n += 1;
  return n;
}

export function diffParts(before: string, after: string): DiffParts {
  const a = [...before];
  const b = [...after];
  if (a.length === 0 && b.length === 0) {
    return { beforePrefix: "", beforeChanged: "", beforeSuffix: "", afterPrefix: "", afterChanged: "", afterSuffix: "", changed: false };
  }
  const prefix = commonPrefix(a, b);
  const suffix = commonSuffix(a.slice(prefix), b.slice(prefix));
  return {
    beforePrefix: a.slice(0, prefix).join(""),
    beforeChanged: a.slice(prefix, a.length - suffix).join(""),
    beforeSuffix: a.slice(a.length - suffix).join(""),
    afterPrefix: b.slice(0, prefix).join(""),
    afterChanged: b.slice(prefix, b.length - suffix).join(""),
    afterSuffix: b.slice(b.length - suffix).join(""),
    changed: a.join("") !== b.join(""),
  };
}
