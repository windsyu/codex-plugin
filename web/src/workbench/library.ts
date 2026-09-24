export interface Coverage { state: string; reasons: string[] }
export interface LibraryEntry {
  entryId: string; sourceId: string; sourceIdentity?: string; kind: string; sourceRevision: string;
  recordedCwd?: string | null; projectBasis?: string;
  projectId: string | null; projectPath: string | null; title: string; recordedAt: string | null;
  nativeThreadId: string | null; runId: string | null; parentThreadId?: string | null; agentName?: string | null;
  coverage: Coverage; capabilities: { read: boolean; resume: boolean; inspectCalls: boolean; manage: boolean };
  isSubagent?: boolean; parentEntryId?: string | null;
  relatedEntryIds: string[]; match?: { record: number; text: string; sourceRevision: string };
}
export interface Project { projectId: string | null; path: string | null; entries: number; latestRecordedAt?: string | null }
export interface LibraryPage<T> { revision: string; records: T[]; nextCursor: string | null; coverage: Coverage; entry?: LibraryEntry; window?: number | null; unassignedRecords?: number; projectExists?: boolean }
export interface LibraryRecord { kind: string; role?: string | null; text?: string; label?: string; status?: string; truncated?: boolean; detailCursor: string; usage?: Record<string, number | null> | null }
export const sourceNames: Record<string, string> = { native: '原生会话', workbench: '工作台记录', observer: '旧版历史' };
export const projectName = (path: string | null) => path?.split(/[\\/]/).filter(Boolean).pop() || '未归属项目';
export const dateLabel = (time: string | null) => time && Number.isFinite(Date.parse(time)) ? new Date(time).toLocaleString('zh-CN', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '时间未记录';
export class LibraryError extends Error { constructor(public status: number, message: string) { super(message); } }
export async function libraryRead<T>(path: string, parameters: URLSearchParams, signal: AbortSignal): Promise<LibraryPage<T>> {
  const response = await fetch(`/workbench/v1/library/${path}?${parameters}`, { credentials: 'same-origin', cache: 'no-store', signal });
  if (!response.ok) {
    let code = '';
    try { const error = await response.json(); if (typeof error.error === 'string') code = error.error; } catch { /* Status remains sufficient when the response has no structured body. */ }
    const special = ({ source_seek_budget: '当前压缩记录的定位超过读取限制，未返回完整内容。已保存文件保持不变。', source_read_budget: '当前历史窗口超过读取限制，未返回完整内容。已保存文件保持不变。', source_read_failed: '此记录包含无法读取或校验的内容。原文件保留，可检查来源后重试。', source_unavailable: '历史来源位置无法读取，请检查目录是否存在以及读取权限。' } as Record<string, string>)[code];
    throw new LibraryError(response.status, special || ({ 400: '阅读位置无效，请重新打开记录。', 401: '连接验证已失效，请重新打开配对入口。', 404: '这条记录已不可用，可能已删除或移除来源。', 409: '历史已更新，当前位置属于旧版本。请重新读取后继续。', 503: '历史暂时无法读取，请检查来源状态或稍后重试。' } as Record<number, string>)[response.status] || '读取失败，请重试。');
  }
  const raw = await response.text();
  if (raw.length > 1024 * 1024) throw new Error('返回内容超过阅读限制。');
  const page: LibraryPage<T> = JSON.parse(raw);
  if (!Array.isArray(page.records) || page.records.length > 200 || !page.coverage || typeof page.revision !== 'string') throw new Error('历史数据格式无法识别。');
  return page;
}
export const failure = (e: unknown) => e instanceof LibraryError ? e.message : '连接暂时不可用，请重试。';
export function coverageText(coverage: Coverage): string {
  const r = coverage.reasons;
  if (r.some(s => /cache_budget|body_cache_limit|search_excerpt_limit|entry_coverage_partial/.test(s))) return '搜索只覆盖已索引的内容；打开会话可按需读取正文。';
  if (r.includes('observer_capture_scope')) return '旧版历史仅包含当时捕获的记录，不能代表完整原生会话。';
  if (r.some(s => /unavailable/.test(s))) return '部分来源不可用，当前结果可能不全。';
  if (r.some(s => /indexing/.test(s))) return '正在整理历史，已有记录可以先阅读。';
  if (r.filter(s => s !== 'windowed_read').length) return '此来源含缺口或未识别内容，已保存的记录仍可阅读。';
  return '';
}
export interface HistoryRoute { project: string; source: string; group: string; q: string; entry: string; revision: string; match: string }
const keys = ['project', 'source', 'group', 'q', 'entry', 'revision', 'match'] as const;
export function routeFromURL(): HistoryRoute {
  const params = new URLSearchParams(location.search);
  return Object.fromEntries(keys.map(k => [k, (params.get(k) || '').slice(0, k === 'q' ? 512 : 256)])) as unknown as HistoryRoute;
}
export function routeURL(route: HistoryRoute) {
  const url = new URL(location.href);
  for (const key of keys) { if (route[key]) url.searchParams.set(key, route[key]); else url.searchParams.delete(key); }
  return url.pathname + url.search;
}
