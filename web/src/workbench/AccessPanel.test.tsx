import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import QRCode from 'qrcode';
import jsQR from 'jsqr';
import { AccessPanel, type AccessStatus } from './AccessPanel';
vi.mock('qrcode', async original => ({ default: { ...(await original<{default:typeof QRCode}>()).default, toCanvas: vi.fn().mockResolvedValue(undefined) } }));
const root = document.createElement('div');
let info: AccessStatus, fetcher: ReturnType<typeof vi.fn>, pairPath: string;
const token = 'a'.repeat(64);
const scopedPath = '/?run=11111111-1111-4111-8111-111111111111';
const response = (body: unknown, status = 200) => ({ ok: status < 400, status, json: async () => structuredClone(body) });
const button = (name: string) => [...root.querySelectorAll('button')].find(b => b.getAttribute('aria-label') === name || b.textContent === name)!;
const click = async (element: HTMLElement) => { await act(async () => element.click()); await act(async () => {}); };
async function show(open = true) { await act(async () => render(<><textarea aria-label="native" /><AccessPanel epoch="run" open={open} onClose={() => {}} /></>, root)); await act(async () => {}); }
beforeEach(() => {
  pairPath = '/'; history.replaceState(null, '', '/');
  document.body.append(root);
  info = { runEpoch: 'run', revision: 1, state: 'ready', addresses: [{ id: 'ip', label: '局域网', origin: 'http://192.168.1.2:5000' }, { id: 'dns', label: 'Tailscale', origin: 'http://machine.tail-test.ts.net:5000' }], devices: [], notice: null, error: null, pairingId: 'fixed-code' };
  fetcher = vi.fn(async (_url: string, opts?: RequestInit) => {
    if (_url.endsWith('/pairing')) {
      expect(opts?.method).toBeUndefined();
      return response({ runEpoch: 'run', revision: info.revision, pairingId: info.pairingId, links: info.addresses.map(a => ({ addressId: a.id, url: `${a.origin}${pairPath}#pair=${token}` })) });
    }
    if (opts?.method === 'POST') {
      expect(new Headers(opts.headers).get('If-Match')).toBe(`"${info.revision}"`);
      info.revision++;
      if (_url.endsWith('/enable')) info.state = 'ready';
      if (_url.endsWith('/disable')) { info.state = 'off'; info.devices = []; }
    }
    return response(info);
  });
  vi.stubGlobal('fetch', fetcher);
});
afterEach(() => { render(null, root); root.remove(); history.replaceState(null, '', '/'); vi.unstubAllGlobals(); vi.clearAllMocks(); vi.useRealTimers(); });
it.each(['/', scopedPath])('selects a QR and copies its full URL without changing the code or mounted terminal (%s)', async path => {
  pairPath = path; history.replaceState(null, '', path);
  await show(); await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull()); const native = root.querySelector('textarea')!; native.value = '中文草稿';
  const calls = fetcher.mock.calls.length;
  const qrButtons = [...root.querySelectorAll('button')].filter(b => b.textContent === '二维码');
  await vi.waitFor(() => expect(qrButtons[1].disabled).toBe(false));
  await click(qrButtons[1]);
  expect(qrButtons[1].getAttribute('aria-pressed')).toBe('true');
  expect(fetcher).toHaveBeenCalledTimes(calls);
  expect(QRCode.toCanvas).toHaveBeenLastCalledWith(expect.any(HTMLCanvasElement), `http://machine.tail-test.ts.net:5000${path}#pair=${token}`, expect.objectContaining({ margin: 4 }));
  await click(button('复制Tailscale配对链接'));
  expect((root.querySelector('[aria-label="手动复制配对链接"]') as HTMLTextAreaElement).value).toBe(`http://machine.tail-test.ts.net:5000${path}#pair=${token}`);
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal('navigator', { clipboard: { writeText } });
  await click(button('复制局域网配对链接'));
  expect(writeText).toHaveBeenCalledWith(`http://192.168.1.2:5000${path}#pair=${token}`);
  await show(false); await show(true);
  expect(root.querySelector('textarea')).toBe(native); expect(native.value).toBe('中文草稿');
  expect(fetcher.mock.calls.every(([,options]) => !options?.method)).toBe(true);
});
it('retains the QR after pairing and elapsed time, and retrieves the same code after remount', async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  await show(); await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull());
  info.devices = [{ id: 'browser', label: '手机浏览器 1' }]; info.revision++;
  vi.setSystemTime(Date.now() + 24 * 60 * 60 * 1000);
  await show(false); await show(true);
  expect(root.querySelector('canvas')).not.toBeNull(); expect(button('断开').disabled).toBe(false);
  expect(root.textContent).toContain('本次启动期间有效，可重复扫码');
  expect(root.textContent).not.toMatch(/重新生成|有效期|已过期|链接已使用/);
  await act(async () => render(null, root)); await show();
  await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull());
  expect(QRCode.toCanvas).toHaveBeenLastCalledWith(expect.any(HTMLCanvasElement), `http://192.168.1.2:5000/#pair=${token}`, expect.anything());
  expect(fetcher.mock.calls.filter(([url]) => url.endsWith('/pairing'))).toHaveLength(2);
  expect(fetcher.mock.calls.every(([,opt]) => !opt?.method)).toBe(true);
});
it.each(['/', scopedPath])('only enables on request and uses the fixed code for new addresses and reopened access (%s)', async path => {
  pairPath = path; history.replaceState(null, '', path);
  info.state = 'off'; await show(); expect(fetcher.mock.calls.every(([,opt]) => !opt?.method)).toBe(true);
  expect(fetcher.mock.calls.some(([url]) => url.endsWith('/pairing'))).toBe(false);
  await click(button('开启设备访问'));
  await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull());
  info.addresses.push({ id: 'second', label: '局域网 2', origin: 'http://192.168.2.2:5000' });
  await show(false); await show(true);
  const before = fetcher.mock.calls.length;
  const buttons = [...root.querySelectorAll('button')].filter(b => b.textContent === '二维码');
  await vi.waitFor(() => expect(buttons[2].disabled).toBe(false)); await click(buttons[2]);
  expect(fetcher).toHaveBeenCalledTimes(before);
  expect(QRCode.toCanvas).toHaveBeenLastCalledWith(expect.any(HTMLCanvasElement), `http://192.168.2.2:5000${path}#pair=${token}`, expect.anything());
  await click(button('关闭设备访问')); await vi.waitFor(() => expect(root.querySelector('canvas')).toBeNull());
  info.addresses[2].origin = 'http://192.168.2.2:6000';
  await click(button('开启设备访问'));
  await vi.waitFor(() => expect(QRCode.toCanvas).toHaveBeenLastCalledWith(expect.any(HTMLCanvasElement), `http://192.168.2.2:6000${path}#pair=${token}`, expect.anything()));
});
it('reports revision conflicts without replaying a mutation', async () => {
  await show(); await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull()); fetcher.mockImplementation(async (_url, opt) => opt?.method ? response({ error: { code: 'access_changed' } }, 412) : response(info));
  await click(button('关闭设备访问'));
  expect(root.textContent).toContain('状态已变化'); expect(fetcher.mock.calls.filter(([,opt]) => opt?.method)).toHaveLength(1);
});
it('does not publish a sharing code returned for a different run', async () => {
  fetcher.mockImplementation(async url => url.endsWith('/pairing') ? response({ runEpoch: 'other', pairingId: info.pairingId, links: [] }) : response(info));
  await show();
  await vi.waitFor(() => expect(root.textContent).toContain('工作台已重新启动')); expect(root.querySelector('canvas')).toBeNull();
});
it('reports an older backend instead of waiting indefinitely for a pairing code', async () => {
  fetcher.mockImplementation(async () => response({ ...info, pairingId: undefined }));
  await show();
  await vi.waitFor(() => expect(root.textContent).toContain('请退出当前工作台并启动新版程序'));
  expect(root.textContent).not.toContain('正在读取配对码');
  expect(root.querySelector('canvas')).toBeNull();
});
it('lets a slow pairing request finish across multiple status polls', async () => {
  let finish!: () => void;
  const slow = new Promise<void>(resolve => { finish = resolve; });
  const normal = fetcher.getMockImplementation()!;
  fetcher.mockImplementation(async (url, options) => {
    if (url.endsWith('/pairing')) await slow;
    return normal(url, options);
  });
  await show();
  await vi.waitFor(() => expect(fetcher.mock.calls.some(([url]) => url.endsWith('/pairing'))).toBe(true));
  const signal = fetcher.mock.calls.find(([url]) => url.endsWith('/pairing'))![1]!.signal!;
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 2200)); });
  const wasAborted = signal.aborted;
  await act(async () => finish());
  expect(wasAborted).toBe(false);
  await vi.waitFor(() => expect(root.querySelector('canvas')).not.toBeNull());
  expect(fetcher.mock.calls.filter(([url]) => url.endsWith('/pairing'))).toHaveLength(1);
});
it('generates a QR that decodes to the full long MagicDNS pairing URL', () => {
  const url = `http://workstation.long-tailnet-name.ts.net:54321${scopedPath}#pair=${token}`;
  const modules = QRCode.create(url, { errorCorrectionLevel: 'M' }).modules;
  const scale = 5, margin = 4, width = (modules.size + margin * 2) * scale;
  const rgba = new Uint8ClampedArray(width * width * 4).fill(255);
  for (let y = 0; y < modules.size; y++) for (let x = 0; x < modules.size; x++) if (modules.get(y, x))
    for (let dy = 0; dy < scale; dy++) for (let dx = 0; dx < scale; dx++) { const pos = (((y + margin) * scale + dy) * width + (x + margin) * scale + dx) * 4; rgba[pos] = rgba[pos + 1] = rgba[pos + 2] = 0; }
  expect(jsQR(rgba, width, width)?.data).toBe(url);
});
it('focuses the access panel immediately on opening without stealing focus on later updates', () => {
  const close = vi.fn();
  const draw = (open: boolean) => render(<><textarea aria-label="native" /><AccessPanel epoch="run" open={open} onClose={close} /></>, root);
  draw(false);
  const native = root.querySelector('textarea')!; native.focus();
  draw(true);
  expect(document.activeElement).toBe(button('收起'));
  document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  expect(close).toHaveBeenCalledOnce();
  native.focus(); draw(true);
  expect(document.activeElement).toBe(native);
});
