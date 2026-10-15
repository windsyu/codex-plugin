export const UI_API_VERSION = 2;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export const AUTH_INVALID = 'workbench-auth-invalid';
const expired = '登录或工作台授权已失效，请返回门户重新登录并打开工作台。';
export function createContext(url: URL, storage: Storage) {
  const match = /^\/g\/([^/]+)(?:\/|$)/.exec(url.pathname);
  if (match && !uuid.test(match[1])) throw new Error('工作台地址无效。');
  const gateway = match?.[1];
  const basePath = gateway ? `/g/${gateway}` : '';
  const key = `workbench-auth:${gateway}`;
  let grant = '', session = '', valid = true;
  if (gateway) {
    const fragment = new URLSearchParams(url.hash.slice(1));
    let tuple: URLSearchParams;
    try { tuple = fragment.has('grant') || fragment.has('session') || fragment.has('api') ? fragment : new URLSearchParams(storage.getItem(key) || ''); }
    catch { tuple = fragment; }
    grant = tuple.get('grant') || ''; session = tuple.get('session') || '';
    valid = uuid.test(grant) && uuid.test(session) && tuple.get('api') === String(UI_API_VERSION);
    try { if (valid) storage.setItem(key, new URLSearchParams({ grant, session, api: String(UI_API_VERSION) }).toString()); else storage.removeItem(key); } catch { /* Explicit navigation can still pass this page's tuple. */ }
  }
  const assert = () => { if (!valid) throw new Error(expired); };
  const path = (value: string) => {
    if (!value.startsWith('/') || value.startsWith('//') || value.includes('\\')) throw new Error('工作台路径无效。');
    const result = value.startsWith(`${basePath}/`) && basePath ? value : basePath + value;
    const parsed = new URL(result, url.origin);
    if (parsed.origin !== url.origin || (basePath && !parsed.pathname.startsWith(basePath + '/'))) throw new Error('工作台路径无效。');
    return result;
  };
  return {
    gateway, basePath, assert,
    get valid() { return valid; },
    invalidate() { valid = false; try { if (new URLSearchParams(storage.getItem(key) || '').get('grant') === grant) storage.removeItem(key); } catch { /* No recovery is attempted. */ } },
    path,
    navigation(value: string) { assert(); return path(value) + (gateway ? `#${new URLSearchParams({ grant, session, api: String(UI_API_VERSION) })}` : ''); },
    stream(value: string) { assert(); const target = new URL(path(value), url.origin); if (gateway) { target.searchParams.set('relayGrant', grant); target.searchParams.set('relayApi', String(UI_API_VERSION)); } return target.pathname + target.search; },
    headers(initial?: HeadersInit) { assert(); const headers = new Headers(initial); if (gateway) { headers.set('X-Workbench-Grant', grant); headers.set('X-Workbench-Api-Version', String(UI_API_VERSION)); } return headers; },
    storageKey(value: string) { return gateway ? `relay:${session}:${gateway}:${value}` : value; }
  };
}
let pinned: ReturnType<typeof createContext> | undefined;
export function workbenchContext() { return pinned ||= createContext(new URL(location.href), sessionStorage); }
export const navigationURL = (path: string) => workbenchContext().navigation(path);
export const streamURL = (path: string) => workbenchContext().stream(path);
export const scopedStorageKey = (key: string) => workbenchContext().storageKey(key);
export function workbenchFetch(path: string, init?: RequestInit): Promise<Response> {
  const context = workbenchContext();
  if (!context.gateway) return fetch(context.path(path), init);
  return relayFetch(context, path, init);
}
async function relayFetch(context: ReturnType<typeof createContext>, path: string, init?: RequestInit) {
  context.assert();
  const response = await fetch(context.path(path), { ...init, headers: context.headers(init?.headers) });
  if (context.gateway && (response.status === 401 || response.status === 403)) {
    context.invalidate(); window.dispatchEvent(new Event(AUTH_INVALID)); throw new Error(expired);
  }
  context.assert();
  return response;
}
