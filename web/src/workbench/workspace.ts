import { useEffect, useRef, useState } from 'preact/hooks';

export type WorkspaceTab = 'files' | 'search' | 'git';
export interface FileSelection { path: string; line?: number; scope?: 'working' | 'staged' }
export interface QueryMeta { readAt: string; elapsedMs: number; truncated: boolean }
export interface Directory extends QueryMeta { path: string; entries: { name: string; path: string; kind: 'file' | 'directory' | 'unavailable' }[]; nextCursor: string | null; omitted: number }
export interface SearchResult extends QueryMeta { hits: { path: string; line: number; text: string; truncated: boolean }[]; scannedFiles: number; omitted: number; limitReason: string | null }
export interface GitStatus extends QueryMeta { branch: string | null; entries: { path: string; index: string; working: string; originalPath: string | null }[]; omitted: number }
export interface GitLog extends QueryMeta { commits: { id: string; shortId: string; author: string; date: string; subject: string }[]; nextCursor: string | null }
export interface FileContent extends QueryMeta { path: string; text?: string; patch?: string; revision: string; empty?: boolean }

const errors: Record<string, string> = {
  pairing_required: '连接已失效，请刷新工作台。', forbidden_path: '该路径不允许阅读：可能位于项目之外、是链接或包含凭证。',
  not_found: '文件不存在或已被移动。', binary_file: '该文件不是 UTF-8 文本，无法预览。', file_too_large: '文件超过 1 MiB，无法预览。',
  directory_too_large: '目录超过 10,000 项，暂不支持浏览。', workspace_changed: '目录或提交记录已变化，请回到第一页重新读取。', file_changed: '文件正在修改，请重新读取。',
  invalid_cursor: '分页位置无效，请回到第一页。', invalid_search: '请输入单行搜索内容（最多 4 KiB）。', invalid_regex: '正则表达式无效或过于复杂。',
  not_git_repository: '当前目录不在 Git 工作区中。', rg_unavailable: '未找到 rg，代码搜索暂不可用。', git_unavailable: '未找到 Git，版本记录暂不可用。',
  git_conflict: '文件存在未解决的冲突，暂不显示内容 Diff。', git_output_limit: 'Git 输出超过 1 MiB，请缩小阅读范围。',
  workspace_timeout: '读取超过 5 秒，已停止。可稍后重试。', workspace_busy: '读取请求较多，请稍后重试。', cancelled: '已取消读取。',
};
export function workspaceError(code: string): string { return errors[code] || '读取未完成，请刷新重试。'; }
export function endpoint(kind: string, params: Record<string, string | undefined> = {}) {
  const query = new URLSearchParams(); for (const [key, value] of Object.entries(params)) if (value !== undefined) query.set(key, value);
  return `/workbench/v1/workspace/${kind}${query.size ? `?${query}` : ''}`;
}
// Poll only the visible selection, never the entire tree or every Git file.
// Aborting a fetch also cancels its bounded backend query.
export function useWorkspaceQuery<T>(url: string | null, active: boolean, poll = false, refreshVersion = 0) {
  const [result, setResult] = useState<{ url: string; data: T } | null>(null);
  const [error, setError] = useState(''); const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0); const controller = useRef<AbortController | null>(null);
  const previous = useRef<string | null>(null);
  useEffect(() => {
    if (!active || !url) return;
    if (previous.current !== url) { setResult(null); previous.current = url; }
    let disposed = false; let timer: ReturnType<typeof setTimeout> | undefined;
    function schedule() { timer = setTimeout(() => { if (document.visibilityState === 'hidden') schedule(); else void load(); }, 5000); }
    async function load() {
      const abort = new AbortController(); controller.current = abort; setLoading(true); setError('');
      try {
        const response = await fetch(url!, { credentials: 'same-origin', signal: abort.signal });
        const value = await response.json();
        if (!response.ok) throw new Error(workspaceError(value.error?.code || 'failed'));
        if (!disposed && !abort.signal.aborted) setResult({ url: url!, data: value as T });
      } catch (e) { if (!disposed && !abort.signal.aborted) setError((e as Error).message); }
      finally {
        if (!disposed) { setLoading(false); if (poll && !abort.signal.aborted) schedule(); }
      }
    }
    void load();
    return () => { disposed = true; controller.current?.abort(); if (timer) clearTimeout(timer); };
  }, [url, active, revision, poll, refreshVersion]);
  return { data: result?.url === url ? result.data : null, error, loading, refresh: () => setRevision(v => v + 1), cancel: () => { controller.current?.abort(); setLoading(false); setError(workspaceError('cancelled')); } };
}
export function relativeFile(root: string, path: string, cwd?: string | null): string | null {
  if (!path || path.includes('\0') || path.includes('\\') || /^[a-z]+:\/\//i.test(path)) return null;
  let candidate = path;
  if (!candidate.startsWith('/') && cwd) candidate = `${cwd}/${candidate}`;
  if (candidate.startsWith('/')) {
    if (!candidate.startsWith(`${root.replace(/\/$/, '')}/`)) return null;
    candidate = candidate.slice(root.replace(/\/$/, '').length + 1);
  }
  candidate = candidate.replace(/^\.\//, '');
  if (candidate.split('/').some(p => !p || p === '.' || p === '..')) return null;
  return candidate;
}
