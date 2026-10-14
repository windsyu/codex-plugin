import { workbenchFetch } from './context';
import './RelayPanel.css';
import { useEffect, useRef, useState } from 'preact/hooks';

interface Binding { userCode: string; verificationUrl: string; expiresInSeconds: number }
interface RelayStatus {
  state: string;
  enabled: boolean;
  serverUrl?: string;
  name?: string;
  accountId?: string;
  username?: string;
  gatewayId?: string;
  gatewayOrigin?: string;
  binding?: Binding;
  error?: string;
}
interface RelayResponse { instanceId: string; relay: RelayStatus }

const endpoint = '/workbench/v1/application/relay';
const httpsUrl = (raw?: string) => {
  if (!raw) return null;
  try {
    const url = new URL(raw);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch { return null; }
};
const serverOrigin = (raw: string) => {
  const href = httpsUrl(raw.trim());
  if (!href) return null;
  const url = new URL(href);
  return url.pathname === '/' && !url.search && !url.hash ? url.origin : null;
};
const stateText = (relay: RelayStatus) => {
  if (relay.state === 'authenticationRequired') return '账号登录已失效，请重新登录';
  if (relay.binding) return '等待账号绑定';
  if (relay.state === 'binding') return '正在生成绑定码';
  if (relay.state === 'bindingFailed') return '绑定失败或已过期';
  if (!relay.enabled) return relay.gatewayId ? '已关闭' : '未绑定';
  return ({ online: '已连接', connected: '已连接', connecting: '连接中', reconnecting: '正在重连', offline: '离线，等待重连', binding: '正在生成绑定码', bindingFailed: '绑定失败或已过期', enabled: '连接中', disabled: '已关闭', error: '连接异常' } as Record<string, string>)[relay.state] || '状态待确认';
};
const errorText = (code?: string) => ({
  server_revocation_unconfirmed: '本机已断开，服务器撤销未确认。请登录门户删除该工作台。',
  private_state_save_failed: '本次连接已停止，但关闭状态未保存。重启前请修复私有配置文件，并在门户撤销工作台。',
  credential_save_failed: '登录凭据未保存，请重试登录。服务器登记撤销未确认时，可在门户撤销电脑接入。'
} as Record<string, string>)[code || ''] || (code ? '请检查服务或网络。' : '');

export function RelayPanel({ instanceId, open, onClose }: { instanceId: string; open: boolean; onClose: () => void }) {
  const [relay, setRelay] = useState<RelayStatus | null>(null);
  const [serverUrl, setServerUrl] = useState('');
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const active = useRef(true);
  const popup = useRef<Window | null>(null);
  const openVerification = (status: RelayStatus) => {
    const url = httpsUrl(status.binding?.verificationUrl);
    if (!url || !popup.current) return;
    try { if (!popup.current.closed && popup.current.location.href === 'about:blank') popup.current.location.replace(url); } catch { /* The explicit verification link remains available. */ }
    popup.current = null;
  };
  useEffect(() => () => { active.current = false; }, []);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    const refresh = async () => {
      try {
        const response = await workbenchFetch(endpoint, { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
        if (!response.ok) throw new Error();
        const value = await response.json() as RelayResponse;
        if (value.instanceId !== instanceId || !value.relay || typeof value.relay.state !== 'string' || typeof value.relay.enabled !== 'boolean') throw new Error();
        if (active.current && !controller.signal.aborted) {
          setRelay(value.relay);
          openVerification(value.relay);
          setServerUrl(current => current || value.relay.serverUrl || '');
          setName(current => current || value.relay.name || '');
        }
      } catch { if (active.current && !controller.signal.aborted) setNotice('无法读取公网接入状态，请稍后重试。'); }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [instanceId, open]);
  const write = async (action: 'bind' | 'enable' | 'disable' | 'forget') => {
    if (busy || !relay) return;
    const origin = serverOrigin(serverUrl);
    if (action === 'bind' && (!origin || !name.trim())) { setNotice('请填写 HTTPS 服务根地址和工作台名称。'); return; }
    setBusy(true); setNotice('');
    if (action === 'disable' || action === 'forget') { try { if (popup.current?.location.href === 'about:blank') popup.current.close(); } catch { /* Do not close an already navigated page. */ } popup.current = null; }
    if (action === 'bind') { try { popup.current = window.open('about:blank', '_blank'); if (popup.current) popup.current.opener = null; } catch { popup.current = null; } }
    try {
      const response = await workbenchFetch(`${endpoint}/${action}`, {
        method: 'POST', credentials: 'same-origin', cache: 'no-store',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(action === 'bind' ? { instanceId, serverUrl: origin, name: name.trim() } : { instanceId })
      });
      const value = await response.json().catch(() => null) as RelayResponse | null;
      if (!response.ok || value?.instanceId !== instanceId || !value.relay) throw new Error();
      if (active.current) {
        setRelay(value.relay);
        openVerification(value.relay);
        if (action === 'forget') { setServerUrl(''); setName(''); }
      }
    } catch { try { popup.current?.close(); } catch { /* Best effort. */ } popup.current = null; if (active.current) setNotice('操作结果未确认。请刷新状态后再决定是否重试。'); }
    finally { if (active.current) setBusy(false); }
  };
  if (!open) return null;
  const verificationUrl = httpsUrl(relay?.binding?.verificationUrl);
  const gatewayUrl = httpsUrl(relay?.gatewayOrigin);
  return <section className="wb-relay-panel" aria-label="公网接入设置">
    <div className="wb-relay-heading"><strong>公网接入</strong><button type="button" onClick={onClose} aria-label="关闭公网接入设置">×</button></div>
    <div className="wb-relay-body">
      <p>在电脑保持运行时，用自己的账号从手机访问完整工作台。公网服务能够读取经它传输的内容。</p>
      <p role="status">状态：{relay ? stateText(relay) : '正在读取…'}</p>
      {relay?.error && <p className="wb-notice" role="alert">{errorText(relay.error)}</p>}
      {relay?.username && <p>登录账号：{relay.username}</p>}
      {relay?.gatewayId && <p>工作台：{relay.name || relay.gatewayId}{gatewayUrl && <> · <a href={gatewayUrl} target="_blank" rel="noopener noreferrer">打开公网入口</a></>}</p>}
      {relay?.binding && <div className="wb-relay-binding"><p>在浏览器登录账号，核对电脑名称并确认。验证成功后自动开启公网接入；若新窗口未打开，请使用下方链接。</p><strong aria-label="绑定码">{relay.binding.userCode}</strong><p>有效期约 {Math.ceil(relay.binding.expiresInSeconds / 60)} 分钟。</p>{verificationUrl && <a href={verificationUrl} target="_blank" rel="noopener noreferrer">打开账号验证页面 ↗</a>}<button disabled={busy} onClick={() => void write('disable')}>取消登录</button></div>}
      {(!relay?.gatewayId || relay.state === 'authenticationRequired') && <form onSubmit={event => { event.preventDefault(); void write('bind'); }}>
        <label>服务地址<input type="url" value={serverUrl} onInput={event => setServerUrl(event.currentTarget.value)} placeholder="https://app.example.com" autoComplete="url" required /></label>
        <label>工作台名称<input value={name} onInput={event => setName(event.currentTarget.value)} maxLength={80} required /></label>
        <button type="submit" disabled={busy || !relay || relay.state === 'binding' || !!relay.binding}>登录并开启公网接入</button>
      </form>}
      {relay?.gatewayId && <div className="wb-relay-actions">{relay.state !== 'authenticationRequired' && <button type="button" disabled={busy} onClick={() => void write(relay.enabled ? 'disable' : 'enable')}>{relay.enabled ? '关闭公网接入' : '开启公网接入'}</button>}<button type="button" disabled={busy} onClick={() => void write('forget')}>退出登录并撤销接入</button></div>}
      {notice && <p className="wb-notice" role="alert">{notice}</p>}
    <p className="wb-relay-hint">断线不会重放输入，也不会停止本机 Codex。关闭接入会保留登录；退出登录会尝试撤销服务器登记，撤销未确认时请到门户撤销电脑接入。</p>
    </div>
  </section>;
}
