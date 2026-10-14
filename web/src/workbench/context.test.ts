import { beforeEach, expect, it, vi } from 'vitest';
import { createContext } from './context';
const gateway = '11111111-1111-4111-8111-111111111111';
const grant = '22222222-2222-4222-8222-222222222222';
const session = '33333333-3333-4333-8333-333333333333';
const entry = (g = grant) => new URL(`https://relay.example/g/${gateway}/#grant=${g}&session=${session}&api=1`);
beforeEach(() => { sessionStorage.clear(); });
it('preserves local and legacy domain routes and storage', () => {
  const context = createContext(new URL('https://old.example/?run=abc'), sessionStorage);
  expect(context.path('/workbench/v1/application')).toBe('/workbench/v1/application');
  expect(context.navigation('/?run=abc')).toBe('/?run=abc');
  expect(context.stream('/workbench/v1/live/events?after=1')).toBe('/workbench/v1/live/events?after=1');
  expect(context.storageKey('wb-launch:instance')).toBe('wb-launch:instance');
});
it('prefixes REST and navigation and carries only query auth on streams', () => {
  const context = createContext(entry(), sessionStorage);
  expect(context.path('/workbench/v1/application')).toBe(`/g/${gateway}/workbench/v1/application`);
  expect(context.headers().get('X-Workbench-Grant')).toBe(grant);
  expect(context.headers().get('X-Workbench-Api-Version')).toBe('1');
  for (const route of ['live/events', 'terminal']) {
    const url = new URL(context.stream(`/workbench/v1/${route}?epoch=run`), entry());
    expect(url.pathname).toBe(`/g/${gateway}/workbench/v1/${route}`);
    expect(url.searchParams.get('relayGrant')).toBe(grant);
    expect(url.searchParams.get('relayApi')).toBe('1');
    expect(url.searchParams.get('epoch')).toBe('run');
  }
  expect(context.navigation('/?run=abc')).toBe(`/g/${gateway}/?run=abc#grant=${grant}&session=${session}&api=1`);
  expect(context.storageKey('wb-launch:instance')).toBe(`relay:${session}:${gateway}:wb-launch:instance`);
});
it('pins a page grant when another page stores a newer grant and does not delete the newer one', () => {
  const old = createContext(entry(), sessionStorage);
  const newer = createContext(entry('44444444-4444-4444-8444-444444444444'), sessionStorage);
  expect(old.headers().get('X-Workbench-Grant')).toBe(grant);
  old.invalidate();
  expect(() => old.headers()).toThrow('失效');
  expect(() => old.navigation('/')).toThrow('失效');
  const refreshed = createContext(new URL(`https://relay.example/g/${gateway}/`), sessionStorage);
  expect(refreshed.headers().get('X-Workbench-Grant')).toBe(newer.headers().get('X-Workbench-Grant'));
});
it('fails closed for missing, malformed or incompatible authorization', () => {
  for (const fragment of ['', '#grant=bad', `#grant=${grant}&session=${session}&api=2`]) {
    expect(() => createContext(new URL(`https://relay.example/g/${gateway}/${fragment}`), sessionStorage).assert()).toThrow('失效');
  }
});
it('blocks cross-origin and traversal paths', () => {
  const context = createContext(entry(), sessionStorage);
  for (const route of ['https://evil.example/', '//evil.example/', '/../relay/v1', '/%2e%2e/relay/v1', '/\\evil.example']) expect(() => context.path(route)).toThrow();
});
it('invalidates a pinned grant on denial and never replays a mutation', async () => {
  vi.resetModules(); history.replaceState(null, '', entry().pathname + entry().hash);
  const fetch = vi.fn(async () => ({ status: 401 })); vi.stubGlobal('fetch', fetch);
  const { workbenchFetch } = await import('./context');
  try {
    await expect(workbenchFetch('/workbench/v1/runs', { method: 'POST' })).rejects.toThrow('失效');
    await expect(workbenchFetch('/workbench/v1/runs', { method: 'POST' })).rejects.toThrow('失效');
    expect(fetch).toHaveBeenCalledTimes(1);
  } finally { vi.unstubAllGlobals(); history.replaceState(null, '', '/'); }
});
