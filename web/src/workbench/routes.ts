export const runPages = ['chat', 'terminal', 'files', 'search', 'git', 'history', 'usage', 'settings', 'access'] as const;
export type RunPage = typeof runPages[number];
export type HomePage = 'overview' | 'history' | 'launch' | 'settings' | 'relay';
export type WorkbenchRoute = { kind: 'home'; page: HomePage } | { kind: 'run'; runId: string; page: RunPage; legacy?: boolean } | { kind: 'missing' };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const validRunId = (id: string) => uuid.test(id) && id !== '00000000-0000-0000-0000-000000000000';
export function parseRoute(url: URL): WorkbenchRoute {
  const remote = /^\/g\/([^/]+)(\/.*)?$/.exec(url.pathname);
  if (remote && !validRunId(remote[1])) return { kind: 'missing' };
  const path = remote ? remote[2] || '/' : url.pathname;
  const legacy = url.searchParams.get('run');
  if (path === '/' && legacy !== null) return validRunId(legacy) ? { kind: 'run', runId: legacy, page: 'chat', legacy: true } : { kind: 'missing' };
  const match = /^\/runs\/([^/]+)\/([^/]+)$/.exec(path);
  if (match && validRunId(match[1]) && runPages.includes(match[2] as RunPage) && (legacy === null || legacy === match[1])) return { kind: 'run', runId: match[1], page: match[2] as RunPage };
  const page = ({ '/': 'overview', '/history': 'history', '/launch': 'launch', '/settings': 'settings', '/relay': 'relay' } as Record<string, HomePage>)[path];
  return page && !(remote && page === 'relay') && legacy === null ? { kind: 'home', page } : { kind: 'missing' };
}
export const runPath = (id: string, page: RunPage = 'chat') => `/runs/${encodeURIComponent(id)}/${page}`;
