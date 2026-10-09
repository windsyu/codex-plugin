import { useCallback, useEffect, useRef, useState } from 'preact/hooks';
import './TelegramPanel.css';

type Candidate = { generation: string; botId: number; userId: number; chatId: number };
type Target = { runEpoch: string; threadId: string; transportEpoch: string; subscriptionGeneration: string };
type Channel = {
  binding: { phase: 'unconfigured' | 'idle' | 'pairing' | 'awaitingConfirmation' | 'active' | 'expired'; generation: string; botId: number | null; candidate: Candidate | null; active: (Candidate & { target: Target }) | null };
  configured: boolean; configuring: boolean; queued: number;
  recovered: { received: number; submitting: number; closed: number };
  lastTask: { id: string; phase: string; nativeConfirmed: boolean; nativeCompleted: boolean; delivery: string; reason: string | null } | null;
  failure: string | null;
};
type Source = { revision: string; transportEpoch: string | null; phase: string; targets: { threadId: string; busy: boolean }[]; failure: string | null };
type Run = { runId: string; supportsChannel: boolean; source: Source | null };
type Selection = { runId: string; threadId: string; sourceRevision: string };
const base = '/workbench/v1/application';
const uuid = (v: unknown): v is string => typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v) && v !== '00000000-0000-0000-0000-000000000000';
const record = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
const count = (v: unknown): v is number => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
const positive = (v: unknown): v is number => count(v) && v > 0;
const text = (v: unknown): v is string => typeof v === 'string' && v.length <= 256;
const nullableText = (v: unknown) => v === null || text(v);
function candidate(v: unknown): v is Candidate { return record(v) && uuid(v.generation) && positive(v.botId) && positive(v.userId) && positive(v.chatId); }
function target(v: unknown): v is Target { return record(v) && uuid(v.runEpoch) && uuid(v.threadId) && uuid(v.transportEpoch) && uuid(v.subscriptionGeneration); }
function channelShape(v: unknown): v is Channel {
  if (!record(v) || !record(v.binding) || !record(v.recovered)) return false;
  const b = v.binding; const r = v.recovered; const task = v.lastTask;
  return ['unconfigured', 'idle', 'pairing', 'awaitingConfirmation', 'active', 'expired'].includes(String(b.phase)) && uuid(b.generation)
    && (b.botId === null || positive(b.botId)) && (b.candidate === null || candidate(b.candidate))
    && (b.active === null || (candidate(b.active) && target((b.active as Candidate & { target: unknown }).target)))
    && (b.phase !== 'awaitingConfirmation' || candidate(b.candidate)) && (b.phase !== 'active' || b.active !== null)
    && typeof v.configured === 'boolean' && typeof v.configuring === 'boolean' && count(v.queued)
    && count(r.received) && count(r.submitting) && count(r.closed) && nullableText(v.failure)
    && (task === null || (record(task) && uuid(task.id) && text(task.phase) && typeof task.nativeConfirmed === 'boolean' && typeof task.nativeCompleted === 'boolean' && text(task.delivery) && nullableText(task.reason)));
}
function runsShape(v: unknown): v is Run[] {
  return Array.isArray(v) && v.length <= 16 && v.every(r => record(r) && uuid(r.runId) && typeof r.supportsChannel === 'boolean'
    && (r.source === null || (record(r.source) && uuid(r.source.revision) && (r.source.transportEpoch === null || uuid(r.source.transportEpoch)) && text(r.source.phase) && nullableText(r.source.failure)
      && Array.isArray(r.source.targets) && r.source.targets.length <= 32 && r.source.targets.every(t => record(t) && uuid(t.threadId) && typeof t.busy === 'boolean'))));
}
class PanelError extends Error { constructor(readonly kind: 'unsupported' | 'stale' | 'invalid' | 'rejected') { super(kind); } }
const phaseText: Record<string, string> = { unconfigured: '未配置', idle: '未激活', pairing: '等待私聊配对命令', awaitingConfirmation: '等待本机确认', active: '已激活', expired: '配对已过期' };
const deliveryText: Record<string, string> = { confirmed: '已投递', partialUnknown: '部分投递，后续结果未知，不自动补发', noReply: '无可发送的最终回复', none: '尚未投递', notSent: '未发送', pending: '等待投递', sending: '发送中', sent: '已投递', complete: '已投递', delivered: '已投递', unknown: '投递结果未知，不自动重发', partial: '部分投递，不自动补发', rejected: '投递被拒绝', failed: '投递失败', withheld: '未满足外发条件' };
const taskText: Record<string, string> = { waiting: '等待原生会话可提交', abandoned: '任务已停止等待', uncertain: '原生提交结果未知，不自动重试，请在本机核对', rejected: '原生提交被拒绝或未发送', received: '已接收', queued: '排队中', submitting: '正在提交', running: '原生执行中', completed: '处理结束', closed: '记录已关闭', failed: '处理失败', withheld: '回复已保留在本机' };

export function TelegramPanel({ instanceId, open = true }: { instanceId: string; open?: boolean }) {
  const [channel, setChannel] = useState<Channel | null>(null); const [runs, setRuns] = useState<Run[]>([]);
  const [token, setToken] = useState(''); const [command, setCommand] = useState<{ generation: string; value: string; seconds: number } | null>(null);
  const [selection, setSelection] = useState<Selection | null>(null); const [consent, setConsent] = useState(false); const [consentIdentity, setConsentIdentity] = useState<string | null>(null);
  const [busy, setBusy] = useState(false); const [blocked, setBlocked] = useState(false); const [message, setMessage] = useState('');
  const [unsupported, setUnsupported] = useState(false); const [stale, setStale] = useState(false);
  const live = useRef(true); const writing = useRef(false); const paused = useRef(false); const reading = useRef(false);
  const sequence = useRef(0); const controller = useRef<AbortController | null>(null); const tokenInput = useRef<HTMLInputElement>(null);
  const identity = useRef(instanceId); identity.current = instanceId;
  function errorMessage(error: unknown, mutation = false) {
    if (error instanceof PanelError && error.kind === 'stale') return '应用已重新启动，请刷新整个页面；当前操作不会重发。';
    if (error instanceof PanelError && error.kind === 'unsupported') return '当前版本尚未开放 Telegram 配置。';
    if (mutation) return '操作结果尚未确认。请手动刷新渠道状态核对；不会自动重发。';
    return '渠道状态无法读取或格式无法识别，请手动刷新。';
  }
  async function body(response: Response) {
    if (response.status === 404) throw new PanelError('unsupported');
    const value: unknown = await response.json();
    if (!response.ok) {
      if (record(value) && record(value.error) && value.error.code === 'stale_instance') throw new PanelError('stale');
      throw new PanelError('rejected');
    }
    if (!record(value)) throw new PanelError('invalid');
    if (value.instanceId !== instanceId) throw new PanelError('stale');
    return value;
  }
  const load = useCallback(async (manual = false) => {
    if (!open || writing.current || reading.current || (!manual && paused.current)) return;
    reading.current = true; const ticket = ++sequence.current; const expected = instanceId;
    const abort = new AbortController(); controller.current = abort;
    let timedOut = false; const deadline = window.setTimeout(() => { timedOut = true; abort.abort(); }, 15000);
    try {
      const [status, candidates] = await Promise.all([
        fetch(`${base}/telegram`, { credentials: 'same-origin', signal: abort.signal }).then(body),
        fetch(`${base}/channel-runs`, { credentials: 'same-origin', signal: abort.signal }).then(body)
      ]);
      if (!channelShape(status.channel) || !runsShape(candidates.runs) || typeof candidates.telegramImplemented !== 'boolean') throw new PanelError('invalid');
      if (!candidates.telegramImplemented) throw new PanelError('unsupported');
      if (!live.current || abort.signal.aborted || ticket !== sequence.current || identity.current !== expected) return;
      const nextChannel = status.channel; const nextRuns = candidates.runs;
      setChannel(nextChannel); setRuns(nextRuns); setMessage(''); setUnsupported(false); setStale(false); setBlocked(false); paused.current = false;
      setCommand(previous => nextChannel.binding.phase === 'pairing' && previous?.generation === nextChannel.binding.generation ? previous : null);
      setSelection(previous => previous && nextRuns.some(r => r.runId === previous.runId && r.supportsChannel && r.source?.phase === 'observing' && !!r.source.transportEpoch && !r.source.failure && r.source.revision === previous.sourceRevision && r.source.targets.some(t => t.threadId === previous.threadId)) ? previous : null);
    } catch (error) {
      if (live.current && (!abort.signal.aborted || timedOut) && ticket === sequence.current) {
        paused.current = true; setBlocked(true); setMessage(errorMessage(error));
        setUnsupported(error instanceof PanelError && error.kind === 'unsupported'); setStale(error instanceof PanelError && error.kind === 'stale');
      }
    } finally { window.clearTimeout(deadline); reading.current = false; }
  }, [instanceId, open]);
  useEffect(() => {
    live.current = true;
    if (!open) { setToken(''); setCommand(null); return; }
    void load(); const timer = window.setInterval(() => void load(), 1000);
    return () => { live.current = false; window.clearInterval(timer); controller.current?.abort(); sequence.current++; };
  }, [load, open]);
  async function mutate(action: string, extra: Record<string, unknown> = {}) {
    if (writing.current || paused.current || stale || unsupported) return;
    writing.current = true; setBusy(true); setMessage(''); controller.current?.abort(); sequence.current++;
    const expected = instanceId; const abort = new AbortController(); const deadline = window.setTimeout(() => abort.abort(), 15000);
    try {
      const value = await body(await fetch(`${base}/telegram/${action}`, { method: 'POST', credentials: 'same-origin', signal: abort.signal, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ instanceId, ...extra }) }));
      if (!live.current || identity.current !== expected) return;
      if (action === 'pair') {
        const pair = value.pairing;
        if (!record(pair) || !uuid(pair.generation) || typeof pair.command !== 'string' || !/^\/bind [0-9a-f-]{36}$/i.test(pair.command) || !positive(pair.expiresInSeconds) || pair.expiresInSeconds > 3600) throw new PanelError('invalid');
        setCommand({ generation: pair.generation, value: pair.command, seconds: pair.expiresInSeconds });
      } else {
        if (!channelShape(value.channel)) throw new PanelError('invalid');
        setChannel(value.channel); setCommand(null); setConsent(false);
      }
      setMessage(action === 'revoke' ? '已撤销渠道授权。' : action === 'forget' ? '已移除保存的凭证并撤销授权。' : '请求已确认。');
    } catch (error) {
      if (live.current && identity.current === expected) { paused.current = true; setBlocked(true); setMessage(errorMessage(error, true)); setStale(error instanceof PanelError && error.kind === 'stale'); }
    } finally { window.clearTimeout(deadline); writing.current = false; if (live.current) setBusy(false); }
  }
  const available = runs.flatMap(run => run.supportsChannel && run.source?.phase === 'observing' && run.source.transportEpoch && !run.source.failure ? run.source.targets.map(t => ({ ...t, runId: run.runId, sourceRevision: run.source!.revision })) : []);
  const disabled = busy || blocked || stale || unsupported || !!channel?.configuring;
  const pending = channel?.binding.candidate;
  const candidateIdentity = pending ? JSON.stringify(pending) : null;
  const confirmed = consent && consentIdentity === candidateIdentity;
  return <section className="telegram-panel" aria-label="Telegram 私聊渠道">
    <h3>Telegram 私聊渠道 <small>实验功能</small></h3>
    <p>仅连接你在本机明确选择的已有会话。普通运行不支持：请启用渠道运行模式并新启动应用和运行，现有运行不会自动转换；目前不支持命名 Codex 启动配置。</p>
    <p>当前支持私聊文本与最终回复。需要审批或补充输入时请回到本机 Codex；远端交互尚未完整支持。</p>
    {message && <p role="status">{message}</p>}
    <button type="button" disabled={busy || stale} onClick={() => void load(true)}>刷新渠道状态</button>
    {!unsupported && <>
      <form onSubmit={event => { event.preventDefault(); const value = token; setToken(''); if (tokenInput.current) tokenInput.current.value = ''; void mutate('configure', { token: value }); }}>
        <label>Bot token<input ref={tokenInput} aria-label="Telegram Bot token" type="password" autoComplete="off" spellcheck={false} maxLength={256} value={token} disabled={disabled || !channel} onInput={event => setToken(event.currentTarget.value)} /></label>
        <button type="submit" disabled={disabled || !channel || !token.trim()}>保存并验证 Bot</button>
      </form>
      <p>凭证仅写入本机；提交后清空输入。不会自动加载凭证或发送配对命令。应用重启后需要重新激活。</p>
      <button type="button" disabled={disabled || !channel} onClick={() => void mutate('configure')}>加载已保存凭证并验证</button>
      {channel && <>
        <p>渠道状态：{phaseText[channel.binding.phase]}{channel.configuring ? ' · 正在验证凭证' : ''}{channel.binding.botId !== null ? ` · Bot ID ${channel.binding.botId}` : ''}</p>
        {channel.failure && <p role="alert">渠道遇到错误，当前状态需要本机核对。</p>}
        <div className="telegram-actions">
          <button type="button" disabled={disabled || !channel.configured} onClick={() => { setConsent(false); setSelection(null); void mutate('pair'); }}>生成私聊配对命令</button>
          <button type="button" disabled={disabled || channel.binding.phase === 'unconfigured'} onClick={() => void mutate('revoke')}>撤销渠道授权</button>
          <button type="button" disabled={disabled} onClick={() => void mutate('forget')}>移除保存的 Bot 凭证</button>
        </div>
        {command && <div className="telegram-command"><p>在目标 Telegram 私聊中手动发送以下命令（有效期 {command.seconds} 秒，以服务器为准）：</p><code>{command.value}</code></div>}
        {pending && channel.binding.phase === 'awaitingConfirmation' && <fieldset disabled={disabled}>
          <legend>本机确认私聊与会话</legend><p>用户 ID：{pending.userId} · 私聊 ID：{pending.chatId} · Bot ID：{pending.botId}</p>
          <label>目标会话<select aria-label="Telegram 目标会话" value={selection ? `${selection.runId}/${selection.threadId}` : ''} onChange={event => {
            const chosen = available.find(t => `${t.runId}/${t.threadId}` === event.currentTarget.value); setSelection(chosen ? { runId: chosen.runId, threadId: chosen.threadId, sourceRevision: chosen.sourceRevision } : null); setConsent(false);
          }}><option value="">请明确选择运行和会话</option>{available.map(t => <option key={`${t.runId}/${t.threadId}`} value={`${t.runId}/${t.threadId}`}>运行 {t.runId} · 会话 {t.threadId}{t.busy ? '（忙碌，将等待）' : '（空闲）'}</option>)}</select></label>
          {!available.length && <p>当前没有可绑定的已订阅会话。请在渠道运行模式的本机 Codex 中打开会话。</p>}
          <label className="telegram-consent"><input type="checkbox" checked={confirmed} onChange={event => { setConsent(event.currentTarget.checked); setConsentIdentity(candidateIdentity); }} />我确认上述私聊身份与会话。回复使用同一会话上下文，可能引用本机历史内容。</label>
          <button type="button" disabled={!selection || !confirmed} onClick={() => { if (selection && confirmed) void mutate('confirm', { candidate: pending, ...selection }); }}>确认并激活私聊</button>
        </fieldset>}
        {channel.binding.active && <p className="telegram-identity">已绑定用户 {channel.binding.active.userId} / 私聊 {channel.binding.active.chatId}<br />运行 {channel.binding.active.target.runEpoch}<br />会话 {channel.binding.active.target.threadId}</p>}
        <dl><dt>等待任务</dt><dd>{channel.queued}</dd><dt>恢复发现的记录（不会自动重放）</dt><dd>已接收 {channel.recovered.received} · 已尝试提交 {channel.recovered.submitting} · 已关闭 {channel.recovered.closed}</dd></dl>
        {channel.lastTask && <div aria-label="最近渠道任务"><p>{taskText[channel.lastTask.phase] || '任务状态暂无法识别'}</p><dl>
          <dt>原生提交确认</dt><dd>{channel.lastTask.nativeConfirmed ? '已确认' : '未确认'}</dd><dt>原生整轮完成</dt><dd>{channel.lastTask.nativeCompleted ? '已完成' : '未确认完成'}</dd>
          <dt>Telegram 投递</dt><dd>{deliveryText[channel.lastTask.delivery] || '投递状态暂无法识别'}</dd>
        </dl>{channel.lastTask.reason && <p>任务需要本机核对；未自动重试。</p>}</div>}
      </>}
    </>}
  </section>;
}
