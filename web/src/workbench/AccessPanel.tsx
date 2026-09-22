import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import QRCode from 'qrcode';

interface Address { id: string; label: string; origin: string }
export interface AccessStatus {
  runEpoch: string; revision: number; state: 'off' | 'starting' | 'ready' | 'stopping' | 'error';
  addresses: Address[]; devices: { id: string; label: string }[]; notice: string | null; error: string | null;
  pairingId: string;
}
interface Pairing { runEpoch: string; revision: number; pairingId: string; links: { addressId: string; url: string }[] }
const messages: Record<string, string> = {
  version_mismatch: '页面与程序版本不一致，请退出当前工作台并启动新版程序。',
  access_changed: '接入状态已变化，请重试。', stale_run: '工作台已重新启动，请刷新页面。',
  local_management_required: '请在电脑本机页面管理接入。', no_device_address: '暂未发现可用地址，请检查网络后重试。',
  listen_failed: '端口无法监听，请检查是否被占用，或在设置中改为自动选择。',
  config_unavailable: '配置暂不可用，请在设置中检查并重新加载。', device_limit: '已达到 8 个浏览器，请先断开不再使用的连接。',
  revocation_pending: '访问资格已撤销，正在等待终端完成清理。请刷新状态。',
};
export function AccessPanel({ epoch, open, onClose }: { epoch: string; open: boolean; onClose: () => void }) {
  const [info, setInfo] = useState<AccessStatus | null>(null);
  const [pairing, setPairing] = useState<Pairing | null>(null);
  const pairingRef = useRef(pairing); pairingRef.current = pairing;
  const [selected, setSelected] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [loadError, setLoadError] = useState('');
  const [manual, setManual] = useState('');
  const infoRef = useRef(info); infoRef.current = info;
  const pending = useRef(false), alive = useRef(true);
  const request = useRef(0), controller = useRef<AbortController | null>(null);
  const closeButton = useRef<HTMLButtonElement>(null), canvas = useRef<HTMLCanvasElement>(null), copyBox = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { alive.current = true; return () => { alive.current = false; controller.current?.abort(); }; }, []);
  async function payload(response: Response) {
    if (response.status === 404) throw new Error(messages.version_mismatch);
    const value = await response.json();
    if (!response.ok) throw new Error(messages[value.error?.code] || '操作未成功，请检查连接并重试。');
    if (value.runEpoch !== epoch) throw new Error(messages.stale_run);
    return value;
  }
  async function read() {
    // A status poll must not cancel a pairing request on a slow connection.
    if (pending.current || (controller.current && !controller.current.signal.aborted)) return;
    const abort = new AbortController(); controller.current = abort;
    const id = ++request.current;
    try {
      const value = await payload(await fetch('/workbench/v1/access', { credentials: 'same-origin', cache: 'no-store', signal: abort.signal }));
      if (!alive.current || id !== request.current) return;
      if (typeof value.pairingId !== 'string') throw new Error(messages.version_mismatch);
      infoRef.current = value; setInfo(value);
      if (value.state === 'ready' && value.addresses.length && pairingRef.current?.pairingId !== value.pairingId) {
        const code = await payload(await fetch('/workbench/v1/access/pairing', { credentials: 'same-origin', cache: 'no-store', signal: abort.signal }));
        if (alive.current && id === request.current) { pairingRef.current = code; setPairing(code); }
      }
      if (alive.current && id === request.current) setLoadError('');
    } catch (error) { if (alive.current && !abort.signal.aborted && id === request.current) setLoadError((error as Error).message); }
    finally { if (controller.current === abort) controller.current = null; }
  }
  async function action(name: 'enable' | 'disable' | 'revoke', id?: string) {
    const current = infoRef.current;
    if (!current || pending.current) return;
    pending.current = true; setBusy(true); setMessage(''); setManual('');
    controller.current?.abort(); request.current++;
    try {
      const path = name === 'revoke' ? `devices/${encodeURIComponent(id!)}?runEpoch=${encodeURIComponent(epoch)}` : name;
      await payload(await fetch(`/workbench/v1/access/${path}`, {
        method: name === 'revoke' ? 'DELETE' : 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', 'If-Match': `"${current.revision}"` },
        ...(name === 'revoke' ? {} : { body: JSON.stringify({ runEpoch: epoch }) }),
      }));
    } catch (error) { if (alive.current) setMessage((error as Error).message); }
    finally {
      pending.current = false;
      if (alive.current) { setBusy(false); await read(); }
    }
  }
  useLayoutEffect(() => { if (open) closeButton.current?.focus({ preventScroll: true }); }, [open, epoch]);
  useEffect(() => {
    if (!open) { controller.current?.abort(); request.current++; return; }
    void read();
    const poll = window.setInterval(() => { void read(); }, 2000);
    return () => { clearInterval(poll); controller.current?.abort(); request.current++; };
  }, [open, epoch]);
  const address = info?.addresses.find(a => a.id === selected) || info?.addresses[0];
  const valid = !!pairing && info?.state === 'ready' && info.pairingId === pairing.pairingId;
  // The run's reusable code also covers newly discovered addresses. Selecting
  // an address only changes the URL, never its code or the connected browsers.
  const suffix = pairing?.links[0]?.url.split('#')[1];
  const link = (a: Address) => valid && suffix ? `${a.origin}/#${suffix}` : '';
  const url = address ? link(address) : '';
  useEffect(() => {
    if (!open || !url || !canvas.current) return;
    void QRCode.toCanvas(canvas.current, url, { errorCorrectionLevel: 'M', margin: 4, scale: 5, color: { dark: '#000000', light: '#ffffff' } })
      .catch(() => { if (alive.current) setMessage('二维码生成失败，请复制配对链接。'); });
  }, [open, url]);
  useEffect(() => { if (manual) { copyBox.current?.focus(); copyBox.current?.select(); } }, [manual]);
  async function copy(a: Address) {
    const text = link(a); if (!text) return;
    try { if (!navigator.clipboard?.writeText) throw new Error(); await navigator.clipboard.writeText(text); setMessage('配对链接已复制'); setManual(''); }
    catch { setManual(text); setMessage('请复制下面选中的配对链接。'); }
  }
  return <section id="wb-access-panel" className="wb-access-panel" hidden={!open} aria-label="手机接入" onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
  }}>
    <div className="wb-panel-heading"><strong>手机接入</strong><button ref={closeButton} onClick={onClose}>收起</button></div>
    <div className="wb-access-body">
      <p className="wb-subtle">手机打开同一个工作台，继续阅读和操作当前 CLI。</p>
      {!info && !loadError && <p role="status">正在读取接入状态…</p>}
      {loadError && <p className="wb-notice" role="alert">{loadError}</p>}
      {info && <>
        {(info.state === 'off' || info.state === 'error') && <button className="wb-access-primary" disabled={busy} onClick={() => void action('enable')}>开启设备访问</button>}
        {info.state === 'starting' && <p role="status">正在获取可用地址…</p>}
        {info.state === 'stopping' && <p role="status">正在关闭设备访问…</p>}
        {info.state === 'ready' && <>
          <h3>可用地址</h3>
          {info.addresses.map(a => <div className={`wb-access-address ${address?.id === a.id ? 'selected' : ''}`} key={a.id}>
            <div><strong>{a.label}</strong><code>{a.origin}</code></div>
            <div className="wb-access-row-actions"><button disabled={!valid} onClick={() => void copy(a)} aria-label={`复制${a.label}配对链接`}>复制</button><button disabled={!valid} aria-pressed={address?.id === a.id} onClick={() => { setSelected(a.id); setManual(''); }}>二维码</button></div>
          </div>)}
          {!info.addresses.length && <p role="status">未发现可用地址，请检查电脑网络。地址会自动更新。</p>}
          {url && <div className="wb-access-qr"><canvas ref={canvas} role="img" aria-label={`${address?.label}配对二维码`} /><span>本次启动期间有效，可重复扫码</span></div>}
          {!valid && !loadError && !!info.addresses.length && <p className="wb-subtle" role="status">正在读取配对码…</p>}
          {info.notice === 'tailscale_unavailable' && <p className="wb-subtle">Tailscale 地址暂不可用。局域网内可直接使用 IP 地址。</p>}
          <p className="wb-subtle">局域网需在同一可互访网络；Tailscale 地址需手机已连接 Tailscale。</p>
          <h3>已配对浏览器 <span>{info.devices.length}</span></h3>
          {!info.devices.length && <p className="wb-subtle">等待手机扫码</p>}
          {info.devices.map(device => <div className="wb-access-device" key={device.id}><span>{device.label}</span><button disabled={busy} onClick={() => void action('revoke', device.id)}>断开</button></div>)}
          <button className="wb-access-disable" disabled={busy} onClick={() => void action('disable')}>关闭设备访问</button>
        </>}
        {info.error && <p className="wb-notice" role="alert">{messages[info.error] || '接入已关闭，请重新开启。'}</p>}
      </>}
      <p className="wb-access-help">仅向可信设备分享，可操作当前 CLI。局域网 HTTP 不加密；开启会接受本机网卡连接。</p>
      {message && <p role="status">{message}</p>}
      {manual && valid && <textarea ref={copyBox} className="wb-access-copy" readOnly aria-label="手动复制配对链接" value={manual} />}
    </div>
  </section>;
}
