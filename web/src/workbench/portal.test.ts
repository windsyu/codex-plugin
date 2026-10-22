import { afterEach, expect, it, vi } from 'vitest';
import html from '../../../relay/static/index.html?raw';
import script from '../../../relay/static/portal.js?raw';

const id = '11111111-1111-4111-8111-111111111111';
const member = { username: 'person', role: 'user', mustChangePassword: false };
const response = (data: unknown, status = 200) => ({ ok: status < 400, status, json: async () => data });
const el = (id: string) => document.getElementById(id)!;
async function settle() { for (let i = 0; i < 15; i++) await Promise.resolve(); }
async function portal(account: object | null = member, route = '/computers', scriptLocation: Pick<Location, 'origin' | 'pathname' | 'hash' | 'assign' | 'reload'> = location) {
  history.replaceState(null, '', route);
  document.body.innerHTML = new DOMParser().parseFromString(html, 'text/html').body.innerHTML;
  const fetch = vi.fn(async (path: string): Promise<ReturnType<typeof response>> => response(
    path.endsWith('/account') ? account :
    path.endsWith('/gateways') ? { gateways: [{ gatewayId: id, name: '<img src=x onerror=alert(1)>', status: 'awaitingConnection' }] } :
    path.endsWith('/admin/users') ? { users: [{ accountId: id, username: '<script>evil</script>', disabled: false, mustChangePassword: true }] } : {},
    path.endsWith('/account') && !account ? 401 : 200,
  ));
  vi.stubGlobal('fetch', fetch);
  new Function('location', script)(scriptLocation);
  await settle();
  return fetch;
}
function submit(id: string) { el(id).dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); }
function navigate(path: string) { document.querySelector<HTMLAnchorElement>('nav a[href="' + path + '"]')!.click(); }
function deleteButton() { return [...document.querySelectorAll<HTMLButtonElement>('#users button')].find(button => button.textContent === '删除账号')!; }
afterEach(() => {
  window.dispatchEvent(new Event('pagehide'));
  vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks();
  document.body.replaceChildren(); sessionStorage.clear(); history.replaceState(null, '', '/');
});
it('redirects root to login or computers without honoring arbitrary redirect input', async () => {
  await portal(null, '/?next=https://evil.test/');
  expect(location.pathname).toBe('/login');
  expect(el('signed-out').hidden).toBe(false);
  expect(el('signed-in').hidden).toBe(true);
});
it('first password change exposes only account security and logout, without privileged reads', async () => {
  const fetch = await portal({ ...member, role: 'admin', mustChangePassword: true }, '/admin/users');
  expect(location.pathname).toBe('/account/security');
  expect(el('security').hidden).toBe(false);
  expect(el('navigation').hidden).toBe(true);
  expect(el('workspace-access').hidden).toBe(true);
  expect(el('administration').hidden).toBe(true);
  expect(fetch.mock.calls.map(call => call[0])).toEqual(['/relay/v1/account']);
});
it('renders waiting state and hostile computer names safely; ordinary users cannot visit admin', async () => {
  const fetch = await portal(member, '/admin/users');
  expect(location.pathname).toBe('/computers');
  expect(el('admin-link').hidden).toBe(true);
  expect(el('gateways').textContent).toContain('<img src=x onerror=alert(1)>');
  expect(el('gateways').textContent).toContain('等待电脑连接');
  expect(document.querySelector('#gateways img')).toBeNull();
  expect(document.querySelector<HTMLButtonElement>('#gateways button')!.disabled).toBe(true);
  expect(fetch.mock.calls.some(call => call[0].includes('/admin/'))).toBe(false);
});
it('admin route is independent, renders user names as text, and does not fetch computers', async () => {
  const fetch = await portal({ ...member, role: 'admin' }, '/admin/users');
  expect(el('administration').hidden).toBe(false);
  expect(el('security').hidden).toBe(true);
  expect(el('workspace-access').hidden).toBe(true);
  expect(el('users').textContent).toContain('<script>evil</script>');
  expect(document.querySelector('#users script')).toBeNull();
  expect(fetch.mock.calls.some(call => call[0].endsWith('/gateways'))).toBe(false);
});
it('canceling account deletion keeps the account and sends no mutation', async () => {
  const fetch = await portal({ ...member, username: 'windsyu', role: 'admin' }, '/admin/users');
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  deleteButton().click(); await settle();
  expect(confirm).toHaveBeenCalledWith(expect.stringContaining('不可撤销'));
  expect(confirm).toHaveBeenCalledWith(expect.stringContaining('不删除电脑上的 Codex 历史'));
  expect(fetch.mock.calls).toHaveLength(2);
  expect(el('users').textContent).toContain('<script>evil</script>');
  expect(deleteButton().disabled).toBe(false);
});
it('confirmed deletion uses the protected DELETE API and refreshes only after success', async () => {
  const fetch = await portal({ ...member, username: 'windsyu', role: 'admin' }, '/admin/users');
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  fetch.mockImplementation(async path => response(path.endsWith('/admin/users') ? { users: [] } : null, path.endsWith('/admin/users') ? 200 : 204));
  deleteButton().click(); await settle();
  expect(fetch).toHaveBeenCalledWith('/relay/v1/admin/users/' + id, expect.objectContaining({ method: 'DELETE', body: '{}', headers: { 'Content-Type': 'application/json' } }));
  expect(el('users').textContent).toContain('尚未创建');
  expect(el('status').textContent).toContain('账号已删除');
  expect(el('account').textContent).toBe('windsyu');
});
it('failed deletion does not remove the row or report success', async () => {
  const fetch = await portal({ ...member, role: 'admin' }, '/admin/users');
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  fetch.mockImplementation(async () => response({}, 503));
  deleteButton().click(); await settle();
  expect(el('users').textContent).toContain('<script>evil</script>');
  expect(el('status').textContent).toContain('服务暂时不可用');
  expect(el('status').textContent).not.toContain('账号已删除');
  expect(deleteButton().disabled).toBe(false);
});
it('late deletion results cannot repopulate private data or report success after logout', async () => {
  const fetch = await portal({ ...member, role: 'admin' }, '/admin/users');
  vi.spyOn(window, 'confirm').mockReturnValue(true);
  let resolve!: (value: ReturnType<typeof response>) => void;
  fetch.mockImplementation(path => path.endsWith('/' + id) ? new Promise(done => { resolve = done; }) : Promise.resolve(response({})));
  deleteButton().click(); await settle();
  el('logout').click(); await settle();
  resolve(response(null, 204)); await settle();
  expect(location.pathname).toBe('/login');
  expect(el('users').textContent).toBe('');
  expect(el('status').textContent).not.toContain('账号已删除');
  expect(fetch.mock.calls.filter(call => call[0].endsWith('/admin/users'))).toHaveLength(1);
});
it.each(['/authorize#bind=A1B2C3D4E5', '/#bind=A1B2C3D4E5'])('preserves authorization through login from %s', async route => {
  const fetch = await portal(null, route);
  expect(location.hash).toBe('');
  expect(location.pathname).toBe('/login');
  expect(el('login-intent').hidden).toBe(false);
  fetch.mockImplementation(async path => response(path.endsWith('/account') ? member : {}));
  submit('login'); await settle();
  expect(location.pathname).toBe('/authorize');
  expect((document.querySelector('#bind input') as HTMLInputElement).value).toBe('A1B2C3D4E5');
  expect(el('authorization').hidden).toBe(false);
  expect(el('workspace-access').hidden).toBe(true);
});
it('polls only the visible computer route and stops on route change and logout', async () => {
  vi.useFakeTimers();
  const fetch = await portal();
  const count = () => fetch.mock.calls.filter(call => call[0].endsWith('/gateways')).length;
  expect(count()).toBe(1);
  await vi.advanceTimersByTimeAsync(4000); expect(count()).toBe(2);
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
  document.dispatchEvent(new Event('visibilitychange'));
  await vi.advanceTimersByTimeAsync(8000); expect(count()).toBe(2);
  vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
  document.dispatchEvent(new Event('visibilitychange')); await settle(); expect(count()).toBe(3);
  navigate('/account/security'); await settle();
  await vi.advanceTimersByTimeAsync(8000); expect(count()).toBe(3);
  navigate('/computers'); await settle(); expect(count()).toBe(4);
  el('logout').click(); await settle();
  await vi.advanceTimersByTimeAsync(8000); expect(count()).toBe(4);
  expect(location.pathname).toBe('/login'); expect(el('gateways').childElementCount).toBe(0);
});
it('retains a pending authorization through mandatory password change and a fresh login', async () => {
  const fetch = await portal({ ...member, mustChangePassword: true }, '/authorize#bind=A1B2C3D4E5');
  expect(location.pathname).toBe('/account/security');
  submit('change'); await settle();
  expect(location.pathname).toBe('/login');
  fetch.mockImplementation(async path => response(path.endsWith('/account') ? member : {}));
  submit('login'); await settle();
  expect(location.pathname).toBe('/authorize');
  expect((document.querySelector('#bind input') as HTMLInputElement).value).toBe('A1B2C3D4E5');
});
it('does not report successful logout if the server request fails', async () => {
  const fetch = await portal();
  fetch.mockImplementation(async () => { throw new TypeError('network unavailable'); });
  el('logout').click(); await settle();
  expect(el('gateways').textContent).toBe('');
  expect(el('status').textContent).toContain('退出请求未确认');
  expect(el('status').textContent).not.toContain('已退出登录');
});
it('discards late computer responses after logout and does not restart polling', async () => {
  vi.useFakeTimers();
  const fetch = await portal();
  let resolve!: (value: ReturnType<typeof response>) => void;
  fetch.mockImplementation(path => path.endsWith('/gateways') ? new Promise(done => { resolve = done; }) : Promise.resolve(response({})));
  el('refresh').click(); await settle();
  el('logout').click(); await settle();
  resolve(response({ gateways: [{ gatewayId: id, name: 'private old account', status: 'online' }] })); await settle();
  expect(el('gateways').textContent).toBe('');
  expect(location.pathname).toBe('/login');
  const count = fetch.mock.calls.length;
  await vi.advanceTimersByTimeAsync(8000); expect(fetch.mock.calls.length).toBe(count);
});
it('permission failures clear private data and return to login', async () => {
  const fetch = await portal();
  fetch.mockImplementation(async () => response({}, 403));
  el('refresh').click(); await settle();
  expect(location.pathname).toBe('/login');
  expect(el('gateways').textContent).toBe('');
  expect(el('status').textContent).toContain('无法执行');
});
it('unknown computer states never enable opening', async () => {
  const fetch = await portal();
  fetch.mockImplementation(async () => response({ gateways: [{ gatewayId: id, name: 'computer', status: 'futureState' }] }));
  el('refresh').click(); await settle();
  expect(el('gateways').textContent).toContain('状态未知');
  expect(document.querySelector<HTMLButtonElement>('#gateways button')!.disabled).toBe(true);
});

const portalOrigin = 'https://relay.example';
const entry = portalOrigin + '/g/' + id + '/#grant=' + id + '&session=' + id + '&api=3';
async function onlinePortal(openResponse: string | Promise<ReturnType<typeof response>>) {
  const assign = vi.fn();
  const fetch = await portal(member, '/computers', {
    origin: portalOrigin, get pathname() { return location.pathname; }, get hash() { return location.hash; },
    assign, reload: vi.fn(),
  });
  fetch.mockImplementation(async path => path.endsWith('/open')
    ? typeof openResponse === 'string' ? response({ url: openResponse }) : await openResponse
    : response({ gateways: [{ gatewayId: id, name: 'test computer', status: 'online' }] }));
  el('refresh').click(); await settle();
  const open = document.querySelector<HTMLButtonElement>('#gateways button')!;
  expect(open.disabled).toBe(false);
  return { assign, fetch, open };
}
it('opens the online computer through the real API 3 portal flow', async () => {
  const { assign, fetch, open } = await onlinePortal(entry);
  open.click(); await settle();
  expect(fetch).toHaveBeenCalledWith('/relay/v1/gateways/' + id + '/open', expect.objectContaining({
    method: 'POST', body: JSON.stringify({ uiApiVersion: 3 }),
  }));
  expect(assign).toHaveBeenCalledTimes(1);
  expect(assign).toHaveBeenCalledWith(entry);
});
it.each([
  entry.replace('api=3', 'api=2'), entry.replace('api=3', 'api=4'), entry.replace('&api=3', ''),
  entry.replace(portalOrigin, 'https://foreign.example'), entry.replace(portalOrigin, portalOrigin + ':9443'),
  entry.replace('/g/' + id + '/', '/g/22222222-2222-4222-8222-222222222222/'),
  entry.replace('/#', '/?search=private#'), entry.replace('grant=' + id, 'grant=invalid'),
  entry.replace('session=' + id, 'session=invalid'), entry.replace('grant=' + id + '&', ''),
  entry.replace('session=' + id + '&', ''), entry.replace('https://', 'https://person:secret@'),
  entry.replace('https://', 'http://'), 'javascript:alert(1)', 'not a URL',
])('rejects unsafe or incompatible portal entries: %s', async unsafe => {
  const { assign, open } = await onlinePortal(unsafe);
  open.click(); await settle();
  expect(assign).not.toHaveBeenCalled();
  expect(el('status').dataset.error).toBe('true');
});
it('does not navigate on a late open response after logout', async () => {
  let resolve!: (value: ReturnType<typeof response>) => void;
  const pending = new Promise<ReturnType<typeof response>>(done => { resolve = done; });
  const { assign, open } = await onlinePortal(pending);
  open.click(); await settle();
  el('logout').click(); await settle();
  resolve(response({ url: entry })); await settle();
  expect(assign).not.toHaveBeenCalled();
  expect(location.pathname).toBe('/login');
  expect(el('gateways').textContent).toBe('');
});
