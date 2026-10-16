import { navigationURL, workbenchFetch } from './context';
export interface Coverage { state: string; reasons: string[] }
export type UnassignedReason = 'desktop_generated' | 'source_unassigned' | 'missing_cwd' | 'invalid_metadata' | 'identity_conflict' | 'user_unassigned' | 'unknown';
export interface LibraryEntry {
  entryId: string; sourceId: string; sourceIdentity?: string; kind: string; sourceRevision: string;
  recordedCwd?: string | null; projectBasis?: string;
  unassignedReason?: UnassignedReason | null;
  organization?: Organization;
  projectId: string | null; projectPath: string | null; title: string; recordedAt: string | null;
  preview?: { text: string; role: 'user' | 'assistant' } | null;
  nativeThreadId: string | null; runId: string | null; parentThreadId?: string | null; agentName?: string | null;
  coverage: Coverage; capabilities: { read: boolean; resume: boolean; inspectCalls: boolean; manage: boolean };
  isSubagent?: boolean; parentEntryId?: string | null;
  relatedEntryIds: string[]; match?: { record: number; text: string; sourceRevision: string };
}
export interface Project { projectId: string | null; path: string | null; entries: number; latestRecordedAt?: string | null }
export interface SelectedProject { projectId: string; path: string | null; status: 'available' | 'missing' | 'indexing' | 'unknown' }
export type SessionSection = 'primary' | 'auxiliary' | 'runs';
export type GroupMember = Pick<LibraryEntry, 'entryId' | 'sourceId' | 'sourceIdentity' | 'kind' | 'sourceRevision' | 'title' | 'preview' | 'coverage' | 'capabilities' | 'match'>;
export interface SessionGroup { groupId: string; section: SessionSection; conversationKind: 'ordinary' | 'subagent' | 'background_review' | 'unknown'; preferredEntryId: string; members: GroupMember[]; projectId: string | null; projectPath: string | null; latestRecordedAt: string | null }
export type SessionCounts = Record<SessionSection, { groups: number; records: number }>;
export interface Organization {
  automatic: { projectId: string | null; projectPath: string | null; basis: string };
  effective: { projectId: string | null; projectPath: string | null; basis: 'user_assigned' | 'user_unassigned' | 'automatic' };
  unassignedReason: UnassignedReason | null;
  conversationKind: 'ordinary' | 'subagent' | 'background_review' | 'unknown';
  kindBasis: string;
  overrideState: 'none' | 'applied' | 'stale' | 'unavailable';
}
export interface OrganizationResponse { organization: Organization; organizationRevision: string; revision: string; canAdjust: boolean; adjustmentReason?: string | null }
export type OrganizationAction = 'project' | 'unassigned' | 'automatic';
export interface LibraryPage<T> { revision: string; records: T[]; nextCursor: string | null; coverage: Coverage; entry?: LibraryEntry; window?: number | null; unassignedRecords?: number; projectExists?: boolean; selectedProject?: SelectedProject | null; totalRecords?: number; organizationRevision?: string; counts?: SessionCounts }
export interface LibraryRecord { kind: string; role?: string | null; presentation?: 'conversation' | 'context' | 'activity' | 'attention'; text?: string; label?: string; status?: string; truncated?: boolean; detailCursor: string; usage?: Record<string, number | null> | null }
export const sourceNames: Record<string, string> = { native: '原生会话', workbench: '工作台记录', observer: '旧版历史' };
export const projectName = (path: string | null) => path?.split(/[\\/]/).filter(Boolean).pop() || '未归属项目';
export const unassignedReasonText = (reason?: UnassignedReason | null) => ({ desktop_generated: '应用生成的工作目录', source_unassigned: '来源未记录项目', missing_cwd: '工作目录未记录', invalid_metadata: '元数据无法解释', identity_conflict: '身份信息冲突', user_unassigned: '用户设为未归类', unknown: '归属未确认' }[reason || 'unknown'] || '归属未确认');
export const dateLabel = (time: string | null) => time && Number.isFinite(Date.parse(time)) ? new Date(time).toLocaleString('zh-CN', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '时间未记录';
export class LibraryError extends Error { constructor(public status: number, message: string) { super(message); } }
async function readLibraryJSON(path: string, parameters: URLSearchParams, signal: AbortSignal, request?: RequestInit): Promise<unknown> {
  const response = await workbenchFetch(`/workbench/v1/library/${path}?${parameters}`, { ...request, credentials: 'same-origin', cache: 'no-store', signal });
  if (!response.ok) {
    let code = '';
    try { const error = await response.json(); if (typeof error.error === 'string') code = error.error; else if (typeof error.error?.code === 'string') code = error.error.code; } catch { /* Status remains sufficient when the response has no structured body. */ }
    const special = ({ query_budget_exceeded: '整理视图达到目录查询上限，可切换“全部来源记录”。此上限在筛选前检查，缩小筛选不能解除；原始历史保持不变。', source_seek_budget: '当前压缩记录的定位超过读取限制，未返回完整内容。已保存文件保持不变。', source_read_budget: '当前历史窗口超过读取限制，未返回完整内容。已保存文件保持不变。', source_read_failed: '此记录包含无法读取或校验的内容。原文件保留，可检查来源后重试。', source_unavailable: '历史来源位置无法读取，请检查目录是否存在以及读取权限。' } as Record<string, string>)[code];
    throw new LibraryError(response.status, special || ({ 400: '阅读位置无效，请重新打开记录。', 401: '连接验证已失效，请重新打开配对入口。', 404: '这条记录已不可用，可能已删除或移除来源。', 409: '历史已更新，当前位置属于旧版本。请重新读取后继续。', 503: '历史暂时无法读取，请检查来源状态或稍后重试。' } as Record<number, string>)[response.status] || '读取失败，请重试。');
  }
  const raw = await response.text();
  if (raw.length > 1024 * 1024) throw new Error('返回内容超过阅读限制。');
  return JSON.parse(raw);
}
export function validSessionGroup(group: SessionGroup): boolean {
  return !!group && typeof group.groupId === 'string' && typeof group.preferredEntryId === 'string' && Array.isArray(group.members) && group.members.length > 0 && group.members.length <= 10 && group.members.every(member => member && typeof member.entryId === 'string' && typeof member.sourceId === 'string' && typeof member.sourceRevision === 'string' && typeof member.title === 'string' && (member.preview == null || (typeof member.preview.text === 'string' && member.preview.text.length <= 960 && Array.from(member.preview.text).length <= 240 && ['user', 'assistant'].includes(member.preview.role))) && member.coverage && Array.isArray(member.coverage.reasons) && member.capabilities) && group.members.some(member => member.entryId === group.preferredEntryId);
}
export async function libraryRead<T>(path: string, parameters: URLSearchParams, signal: AbortSignal): Promise<LibraryPage<T>> {
  const page = await readLibraryJSON(path, parameters, signal) as LibraryPage<T>;
  if (!Array.isArray(page.records) || page.records.length > 200 || !page.coverage || typeof page.revision !== 'string') throw new Error('历史数据格式无法识别。');
  if (path === 'session-groups' && !page.records.every(group => validSessionGroup(group as SessionGroup))) throw new Error('会话分组格式无法识别。');
  return page;
}
export async function readSessionGroup(id: string, anchor: string, revision: string, signal: AbortSignal): Promise<SessionGroup> {
  // The endpoint returns a bare group; the server checks the requested catalog
  // revision and responds 409 rather than returning a different generation.
  const group = await readLibraryJSON(`session-groups/${encodeURIComponent(id)}`, new URLSearchParams({ anchorEntryId: anchor, revision }), signal) as SessionGroup;
  if (!validSessionGroup(group) || group.groupId !== id || !group.members.some(member => member.entryId === anchor)) throw new LibraryError(409, '历史已更新，请重新读取列表后查看关联来源。');
  return group;
}
function organizationResponse(value: unknown): OrganizationResponse {
  const result = value as OrganizationResponse;
  const validProject = (project: Organization['automatic'] | Organization['effective']) => project && (project.projectId === null || typeof project.projectId === 'string') && (project.projectPath === null || typeof project.projectPath === 'string') && typeof project.basis === 'string';
  if (!result || typeof result.organizationRevision !== 'string' || !result.organizationRevision || typeof result.revision !== 'string' || typeof result.canAdjust !== 'boolean' || !validProject(result.organization?.automatic) || !validProject(result.organization?.effective) || !['user_assigned', 'user_unassigned', 'automatic'].includes(result.organization.effective.basis) || !['ordinary', 'subagent', 'background_review', 'unknown'].includes(result.organization.conversationKind) || typeof result.organization.kindBasis !== 'string' || (result.organization.unassignedReason !== null && !['desktop_generated', 'source_unassigned', 'missing_cwd', 'invalid_metadata', 'identity_conflict', 'user_unassigned', 'unknown'].includes(result.organization.unassignedReason)) || !['none', 'applied', 'stale', 'unavailable'].includes(result.organization.overrideState)) throw new Error('用户归类数据无法识别。');
  return result;
}
export async function readOrganization(entryId: string, signal: AbortSignal): Promise<OrganizationResponse> {
  return organizationResponse(await readLibraryJSON(`entries/${encodeURIComponent(entryId)}/organization`, new URLSearchParams(), signal));
}
export async function saveOrganization(entry: LibraryEntry, instanceId: string, revision: string, action: OrganizationAction, projectId: string | undefined, signal: AbortSignal): Promise<OrganizationResponse> {
  const body = { instanceId, sourceIdentity: entry.sourceIdentity, sourceRevision: entry.sourceRevision, action, ...(action === 'project' ? { projectId } : {}) };
  return organizationResponse(await readLibraryJSON(`entries/${encodeURIComponent(entry.entryId)}/organization`, new URLSearchParams(), signal, { method: 'PUT', headers: { 'Content-Type': 'application/json', 'If-Match': `"${revision}"` }, body: JSON.stringify(body) }));
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
export interface HistoryRoute { project: string; source: string; group: string; q: string; entry: string; revision: string; match: string; projectQ?: string; view?: string; section?: string }
const keys = ['project', 'source', 'group', 'q', 'entry', 'revision', 'match', 'projectQ', 'view', 'section'] as const;
export const historyView = (route: HistoryRoute) => route.view === 'records' || (!route.view && (route.group === 'main' || route.group === 'agents')) ? 'records' : 'groups';
export function routeFromURL(): HistoryRoute {
  const params = new URLSearchParams(location.search);
  return Object.fromEntries(keys.map(k => [k, (params.get(k) || '').slice(0, k === 'q' || k === 'projectQ' ? 512 : 256)])) as unknown as HistoryRoute;
}
export function routeURL(route: HistoryRoute) {
  const url = new URL(location.href);
  for (const key of keys) { if (route[key]) url.searchParams.set(key, route[key]); else url.searchParams.delete(key); }
  return navigationURL(url.pathname + url.search);
}
