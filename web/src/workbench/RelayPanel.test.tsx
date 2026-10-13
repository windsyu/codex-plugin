import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { RelayPanel } from './RelayPanel';

const root = document.createElement('div');
const instanceId = 'local-instance';
const reply = (relay: Record<string, unknown>) => ({ ok: true, json: async () => ({ instanceId, relay }) });
const unbound = { state: 'disabled', enabled: false };
const bound = { state: 'online', enabled: true, gatewayId: 'gateway-1', name: 'My workstation', gatewayOrigin: 'https://one.wb.example.com' };
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); vi.useRealTimers(); });
async function show() { document.body.append(root); await act(async () => render(<RelayPanel instanceId={instanceId} open onClose={() => {}} />, root)); await act(async () => {}); }
async function click(text: string) { await act(async () => [...root.querySelectorAll('button')].find(button => button.textContent === text)!.click()); await act(async () => {}); }

it('reads only when open and sends instance-scoped bind with a safe verification link', async () => {
  const fetch = vi.fn(async (_url: string, init?: RequestInit) => init?.method ? reply({ ...unbound, binding: { userCode: 'ABCD-EFGH', verificationUrl: 'https://app.example.com/verify', expiresInSeconds: 600 } }) : reply(unbound));
  vi.stubGlobal('fetch', fetch);
  document.body.append(root); await act(async () => render(<RelayPanel instanceId={instanceId} open={false} onClose={() => {}} />, root));
  expect(fetch).not.toHaveBeenCalled();
  await show();
  const inputs = root.querySelectorAll('input');
  await act(async () => { inputs[0].value = 'https://app.example.com'; inputs[0].dispatchEvent(new Event('input', { bubbles: true })); inputs[1].value = 'My workstation'; inputs[1].dispatchEvent(new Event('input', { bubbles: true })); });
  await click('开始绑定');
  const call = fetch.mock.calls.find(([, init]) => init?.method);
  expect(call?.[0]).toBe('/workbench/v1/application/relay/bind');
  expect(JSON.parse(String(call?.[1]?.body))).toEqual({ instanceId, serverUrl: 'https://app.example.com', name: 'My workstation' });
  const link = root.querySelector('a[href="https://app.example.com/verify"]');
  expect(link?.getAttribute('target')).toBe('_blank'); expect(link?.getAttribute('rel')).toContain('noopener');
  expect(root.textContent).toContain('ABCD-EFGH');
});

it('sends enable, disable and forget once with the exact instance and shows safe status', async () => {
  let current = { ...bound };
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method) {
      current = { ...current, enabled: url.endsWith('/enable') };
      if (url.endsWith('/forget')) return reply(unbound);
    }
    return reply(current);
  });
  vi.stubGlobal('fetch', fetch); await show();
  expect(root.textContent).toContain('已连接');
  await click('关闭公网接入'); await click('开启公网接入'); await click('忘记此服务');
  const calls = fetch.mock.calls.filter(([, init]) => init?.method);
  expect(calls.map(([url]) => url)).toEqual(['/workbench/v1/application/relay/disable', '/workbench/v1/application/relay/enable', '/workbench/v1/application/relay/forget']);
  expect(calls.map(([, init]) => JSON.parse(String(init?.body)))).toEqual([{ instanceId }, { instanceId }, { instanceId }]);
  expect(root.textContent).toContain('未绑定');
});

it('keeps a failed mutation uncertain, without displaying server error payload or repeating writes', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn(async (_url: string, init?: RequestInit) => init?.method ? { ok: false, json: async () => ({ error: 'private-secret' }) } : reply(bound));
  vi.stubGlobal('fetch', fetch); await show(); await click('关闭公网接入');
  expect(root.textContent).toContain('操作结果未确认'); expect(root.textContent).not.toContain('private-secret');
  expect(root.textContent).toContain('已连接');
  await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
  expect(fetch.mock.calls.filter(([, init]) => init?.method)).toHaveLength(1);
});
