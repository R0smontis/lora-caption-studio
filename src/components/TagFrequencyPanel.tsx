import { useMemo, useState } from "react";
import { BarChart3, Search, Tags } from "lucide-react";
import type { ImageJob } from "../types";

export interface TagFrequency {
  tag: string;
  imageCount: number;
  occurrenceCount: number;
  percentage: number;
}

export function buildTagFrequencies(jobs: ImageJob[]): TagFrequency[] {
  const counts = new Map<string, { label: string; images: number; occurrences: number }>();
  for (const job of jobs) {
    const seen = new Set<string>();
    for (const raw of job.caption.split(",")) {
      const tag = raw.trim();
      if (!tag) continue;
      const key = tag.toLocaleLowerCase();
      const current = counts.get(key) ?? { label: tag, images: 0, occurrences: 0 };
      current.occurrences += 1;
      if (!seen.has(key)) {
        current.images += 1;
        seen.add(key);
      }
      counts.set(key, current);
    }
  }
  return [...counts.values()]
    .map((entry) => ({
      tag: entry.label,
      imageCount: entry.images,
      occurrenceCount: entry.occurrences,
      percentage: jobs.length ? (entry.images / jobs.length) * 100 : 0,
    }))
    .sort((a, b) => b.imageCount - a.imageCount || b.occurrenceCount - a.occurrenceCount || a.tag.localeCompare(b.tag));
}

export function TagFrequencyPanel({ jobs }: { jobs: ImageJob[] }) {
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState(true);
  const frequencies = useMemo(() => buildTagFrequencies(jobs), [jobs]);
  const visible = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return needle ? frequencies.filter((item) => item.tag.toLocaleLowerCase().includes(needle)) : frequencies;
  }, [frequencies, query]);
  const taggedImages = jobs.filter((job) => job.caption.trim()).length;
  const assignments = frequencies.reduce((sum, item) => sum + item.occurrenceCount, 0);
  const maxCount = visible[0]?.imageCount || 1;

  return (
    <section className={`tag-frequency-card ${expanded ? "expanded" : "collapsed"}`}>
      <header>
        <div className="tag-frequency-title"><BarChart3 size={17} /><div><b>Tag 出现频率</b><small>按包含该 Tag 的图片数统计</small></div></div>
        <div className="tag-frequency-summary"><span><b>{frequencies.length}</b> 个 Tag</span><span><b>{taggedImages}</b> 张有标签</span><span><b>{assignments}</b> 次出现</span></div>
        <button className="button ghost small" onClick={() => setExpanded((value) => !value)}>{expanded ? "收起统计" : "展开统计"}</button>
      </header>
      {expanded && <>
        <div className="tag-frequency-tools"><Search size={14} /><input aria-label="搜索 Tag 统计" placeholder="筛选 Tag…" value={query} onChange={(event) => setQuery(event.target.value)} /><span>{visible.length} 项</span></div>
        <div className="tag-frequency-chart">
          {visible.length ? visible.map((item) => <div className="tag-frequency-row" key={item.tag} title={`${item.tag}：${item.imageCount}/${jobs.length} 张，出现 ${item.occurrenceCount} 次`}>
            <span className="tag-frequency-name">{item.tag}</span>
            <span className="tag-frequency-track"><i style={{ width: `${Math.max(2, item.imageCount / maxCount * 100)}%` }} /></span>
            <b>{item.imageCount}</b><small>{item.percentage.toFixed(item.percentage >= 10 ? 0 : 1)}%</small>
          </div>) : <div className="tag-frequency-empty"><Tags size={22} /><span>{jobs.length ? "没有匹配的 Tag" : "导入图片后显示 Tag 统计"}</span></div>}
        </div>
      </>}
    </section>
  );
}
