import { useEffect, useRef, useState } from 'preact/hooks';
import { lazy, Suspense } from 'preact/compat';

import { Api, ApiError, reconnectingStream } from '../api';
import type { ControllerSource, GatewayCommand, InputLease, SessionAttachment, SessionWorker, SessionWorkerSnapshot, TerminalSnapshotFrame } from '../types';

const TerminalPanel = lazy(async () => ({ default: (await import('./TerminalPanel')).TerminalPanel }));

const WORKER_KEY = 'session-kernel-preview-worker';
const ATTACHMENT_KEY = 'session-kernel-preview-attachment';

interface StoredAttachmentControl {
  attachmentId: string;
  attachmentToken: string;
}

interface SessionShellProps {
  api: Api;
  defaultCwd: string;
  initialThread?: { sourceId: string; sourceEpoch: string; codexThreadId: string };
  onClose: () => void;
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : 'Session Kernel 请求失败';
}

function readStoredAttachmentControl(): StoredAttachmentControl | undefined {
  const serialized = sessionStorage.getItem(ATTACHMENT_KEY);
  if (!serialized) return undefined;
  try {
    const value = JSON.parse(serialized) as Partial<StoredAttachmentControl>;
    if (typeof value.attachmentId !== 'string' || !/^[A-Za-z0-9._-]{1,160}$/.test(value.attachmentId)
      || typeof value.attachmentToken !== 'string' || !/^[0-9a-f]{64}$/.test(value.attachmentToken)) {
      throw new Error('invalid stored attachment control');
    }
    return { attachmentId: value.attachmentId, attachmentToken: value.attachmentToken };
  } catch {
    sessionStorage.removeItem(ATTACHMENT_KEY);
    return undefined;
  }
}

function storeAttachmentControl(attachment: SessionAttachment) {
  sessionStorage.setItem(ATTACHMENT_KEY, JSON.stringify({
    attachmentId: attachment.attachmentId,
    attachmentToken: attachment.attachmentToken
  } satisfies StoredAttachmentControl));
}

export function sessionOwnsInitialThread(worker: SessionWorker, initialThread?: SessionShellProps['initialThread']) {
  if (!initialThread) return true;
  return worker.persistedWorker?.sourceId === initialThread.sourceId
    && worker.persistedWorker.sourceEpoch === initialThread.sourceEpoch
    && worker.threadLeases?.some((lease) => lease.codexThreadId === initialThread.codexThreadId
      && ['acquiring', 'active', 'releasing'].includes(lease.state)) === true;
}

export function mergeSessionWorkerSnapshot(current: SessionWorker, snapshot: SessionWorkerSnapshot): SessionWorker {
  if (current.workerId !== snapshot.workerId) return current;
  return {
    ...current,
    ...snapshot,
    persistedWorker: current.persistedWorker,
    threadLeases: current.threadLeases,
    activeTurns: current.activeTurns
  };
}

export function SessionShell({ api, defaultCwd, initialThread, onClose }: SessionShellProps) {
  const [cwd, setCwd] = useState(defaultCwd.startsWith('/') ? defaultCwd : '');
  const [sources, setSources] = useState<ControllerSource[]>([]);
  const [sourceId, setSourceId] = useState(initialThread?.sourceId || '');
  const [mode, setMode] = useState<'new' | 'resume'>(initialThread ? 'resume' : 'new');
  const [threadId, setThreadId] = useState(initialThread?.codexThreadId || '');
  const [worker, setWorker] = useState<SessionWorker>();
  const [attachment, setAttachment] = useState<SessionAttachment>();
  const [lease, setLease] = useState<InputLease>();
  const [partial, setPartial] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [actionResult, setActionResult] = useState('');
  const reconnectTimer = useRef<number>();
  const attaching = useRef(false);

  async function prepareAttachment(currentWorker: SessionWorker, resume = true) {
    if (attaching.current) return;
    attaching.current = true;
    try {
      const stored = resume ? readStoredAttachmentControl() : undefined;
      const response = await api.post<SessionAttachment>(
        `/v2/sessions/${encodeURIComponent(currentWorker.workerId)}/attach`,
        {
          resumeAttachmentId: stored?.attachmentId || null,
          resumeAttachmentToken: stored?.attachmentToken || null
        },
        crypto.randomUUID()
      );
      storeAttachmentControl(response.data);
      setAttachment(response.data);
      setError('');
    } catch (cause) {
      if (resume && cause instanceof ApiError && [
        'ATTACHMENT_NOT_FOUND',
        'ATTACHMENT_EXPIRED',
        'ATTACHMENT_TOKEN_INVALID',
        'ATTACHMENT_TOKEN_REQUIRED',
        'ATTACHMENT_PRINCIPAL_MISMATCH'
      ].includes(cause.code || '')) {
        sessionStorage.removeItem(ATTACHMENT_KEY);
        attaching.current = false;
        return prepareAttachment(currentWorker, false);
      }
      setError(errorMessage(cause));
    } finally {
      attaching.current = false;
    }
  }

  useEffect(() => {
    api.get<ControllerSource[]>('/v2/control/sources')
      .then((response) => {
        const ready = response.data.filter((source) => source.state === 'ready');
        setSources(ready);
        setSourceId((current) => current || ready[0]?.sourceId || '');
        if (!ready.length) setError('没有 ready 的 App Server source；Gateway 不会代为启动 App Server。');
      })
      .catch((cause) => setError(errorMessage(cause)));
    const workerId = sessionStorage.getItem(WORKER_KEY);
    if (!workerId) return;
    setBusy(true);
    api.get<SessionWorker>(`/v2/sessions/${encodeURIComponent(workerId)}`)
      .then((response) => {
        if (['exited', 'failed', 'stale_epoch'].includes(response.data.state)) {
          sessionStorage.removeItem(WORKER_KEY);
          sessionStorage.removeItem(ATTACHMENT_KEY);
          return;
        }
        if (!sessionOwnsInitialThread(response.data, initialThread)) {
          sessionStorage.removeItem(WORKER_KEY);
          sessionStorage.removeItem(ATTACHMENT_KEY);
          setError('上次终端会话不拥有当前 Thread；请选择恢复以连接正确的 Session Worker。');
          return;
        }
        setWorker(response.data);
        return prepareAttachment(response.data);
      })
      .catch(() => {
        sessionStorage.removeItem(WORKER_KEY);
        sessionStorage.removeItem(ATTACHMENT_KEY);
      })
      .finally(() => setBusy(false));
    return () => window.clearTimeout(reconnectTimer.current);
  }, []);

  useEffect(() => {
    if (!worker?.workerId || ['exited', 'failed', 'stale_epoch'].includes(worker.state)) return;
    const controller = new AbortController();
    void reconnectingStream(
      api,
      `/v2/sessions/${encodeURIComponent(worker.workerId)}/events`,
      (event) => {
        if (event.type === 'session_state') updateWorkerView(event.data as SessionWorker);
      },
      () => undefined,
      controller.signal
    ).catch((cause) => {
      if (!controller.signal.aborted) setError(errorMessage(cause));
    });
    return () => controller.abort();
  }, [worker?.workerId, worker?.state]);

  async function createSession() {
    if (!cwd.startsWith('/')) { setError('cwd 必须是服务端可访问的绝对目录'); return; }
    const source = sources.find((candidate) => candidate.sourceId === sourceId);
    if (!source) { setError('请选择 ready 的 App Server source'); return; }
    if (mode === 'resume' && !threadId.trim()) { setError('恢复会话需要 Codex Thread ID'); return; }
    setBusy(true); setError('');
    try {
      const response = await api.post<SessionWorker>('/v2/sessions', {
        sourceId: source.sourceId,
        sourceEpoch: source.sourceEpoch,
        expectedSupervisorVersion: source.supervisorVersion,
        mode,
        codexThreadId: mode === 'resume' ? threadId.trim() : null,
        cwd,
        rows: 24,
        cols: 80
      }, crypto.randomUUID());
      sessionStorage.setItem(WORKER_KEY, response.data.workerId);
      sessionStorage.removeItem(ATTACHMENT_KEY);
      setWorker(response.data);
      await prepareAttachment(response.data, false);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function acquireInput(): Promise<string | undefined> {
    if (!worker || !attachment) return undefined;
    try {
      const response = await api.post<InputLease>(
        `/v2/sessions/${encodeURIComponent(worker.workerId)}/input-lease`,
        {
          attachmentId: attachment.attachmentId,
          attachmentToken: attachment.attachmentToken,
          expectedVersion: worker.inputLease.version,
          takeover: false
        },
        crypto.randomUUID()
      );
      setLease(response.data);
      setWorker((current) => current ? { ...current, inputLease: response.data } : current);
      setError('');
      return response.data.leaseId || undefined;
    } catch (cause) {
      if (cause instanceof ApiError && cause.code === 'INPUT_LEASE_CONFLICT') {
        setLease(undefined);
        setError('该 PTY 已由另一个浏览器持有输入租约；当前为实时只读附着。');
      } else setError(errorMessage(cause));
      return undefined;
    }
  }

  function reconnect() {
    if (!worker || ['stopping', 'exited', 'failed', 'stale_epoch'].includes(worker.state)) return;
    window.clearTimeout(reconnectTimer.current);
    reconnectTimer.current = window.setTimeout(() => void prepareAttachment(worker), 500);
  }

  async function stop() {
    if (!worker) return;
    setBusy(true);
    try {
      const activeTurns = worker.activeTurns || [];
      const response = await api.post<SessionWorker>(`/v2/sessions/${encodeURIComponent(worker.workerId)}/stop`, worker.persistedWorker ? {
        sourceEpoch: worker.persistedWorker.sourceEpoch,
        expectedWorkerVersion: worker.persistedWorker.version,
        activeTurnPolicy: activeTurns.length ? 'interrupt_expected' : 'reject_if_active',
        expectedActiveTurns: activeTurns.map((turn) => ({
          threadId: turn.codexThreadId,
          turnId: turn.codexTurnId
        }))
      } : {}, crypto.randomUUID());
      setWorker(response.data); setAttachment(undefined); setLease(undefined);
      sessionStorage.removeItem(WORKER_KEY); sessionStorage.removeItem(ATTACHMENT_KEY);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  async function interrupt(threadId: string, expectedTurnId: string) {
    if (!worker?.persistedWorker) return;
    setBusy(true); setError(''); setActionResult('');
    try {
      const response = await api.post<GatewayCommand>(
        `/v2/sessions/${encodeURIComponent(worker.workerId)}/interrupt`,
        {
          sourceEpoch: worker.persistedWorker.sourceEpoch,
          threadId,
          expectedTurnId,
          expectedWorkerVersion: worker.persistedWorker.version
        },
        crypto.randomUUID()
      );
      setActionResult(`interrupt · ${response.data.state} · ${response.data.commandId}`);
      const refreshed = await api.get<SessionWorker>(`/v2/sessions/${encodeURIComponent(worker.workerId)}`);
      setWorker(refreshed.data);
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setBusy(false); }
  }

  function updateLeaseFromSnapshot(next: SessionWorkerSnapshot) {
    if (next.inputLease.ownerAttachmentId === attachment?.attachmentId && next.inputLease.leaseId) setLease(next.inputLease);
    else if (next.inputLease.ownerAttachmentId && next.inputLease.ownerAttachmentId !== attachment?.attachmentId) setLease(undefined);
  }

  function updateWorkerView(next: SessionWorker) {
    setWorker(next);
    updateLeaseFromSnapshot(next);
  }

  function updateWorkerSnapshot(next: SessionWorkerSnapshot) {
    setWorker((current) => current ? mergeSessionWorkerSnapshot(current, next) : current);
    updateLeaseFromSnapshot(next);
  }

  const primaryThreadId = worker?.persistedWorker?.primaryThreadId;
  const sourceIdSummary = worker?.persistedWorker?.sourceId;
  const activeThreadLeases = worker?.threadLeases?.filter((item) => item.state === 'active').length || 0;

  return <div class="session-shell-backdrop" role="presentation">
    <section class="session-shell" role="dialog" aria-modal="true" aria-label="Codex terminal session">
      <header class="session-shell-header">
        <div class="session-shell-brand"><span class="session-shell-mark" aria-hidden="true">&gt;_</span><div>
          <p class="eyebrow">CODEX · LIVE SESSION</p><h2>Codex Terminal</h2>
        </div></div>
        <button class="session-history-action" type="button" onClick={onClose} aria-label="返回 History Viewer"><span aria-hidden="true">←</span> History</button>
      </header>
      {!worker && <form class="session-create" onSubmit={(event) => { event.preventDefault(); void createSession(); }}>
        <label>App Server source<select value={sourceId} onChange={(event) => setSourceId(event.currentTarget.value)}>
          {!sources.length && <option value="">没有 ready source</option>}
          {sources.map((source) => <option key={source.sourceId} value={source.sourceId}>{source.sourceId} · {source.sourceEpoch.slice(0, 8)}</option>)}
        </select></label>
        <fieldset><legend>会话方式</legend>
          <label><input type="radio" name="session-mode" checked={mode === 'new'} onChange={() => setMode('new')} />新建 Thread</label>
          <label><input type="radio" name="session-mode" checked={mode === 'resume'} onChange={() => setMode('resume')} />恢复 Thread</label>
        </fieldset>
        {mode === 'resume' && <label>Codex Thread ID<input value={threadId} onInput={(event) => setThreadId(event.currentTarget.value)} placeholder="thread UUID" /></label>}
        <label>cwd<input value={cwd} onInput={(event) => setCwd(event.currentTarget.value)} placeholder="/absolute/workspace" /></label>
        <p>Gateway 使用固定的 <code>codex --remote</code> / <code>codex resume --remote</code> 模板连接已有 App Server；浏览器不能提供 executable、argv、配置覆盖或环境变量。</p>
        <button type="submit" disabled={busy || !cwd.startsWith('/') || !sourceId || (mode === 'resume' && !threadId.trim())}>{busy ? '正在连接…' : mode === 'resume' ? '恢复原生 TUI' : '启动原生 TUI'}</button>
      </form>}
      {worker && <div class="session-worker">
        <div class="session-toolbar">
          <div class="session-status-cluster">
            <span class={`badge badge-${worker.state}`} title={`Worker ${worker.workerId}`}>{worker.state.replaceAll('_', ' ')}</span>
            <span class={`session-input-state ${lease?.leaseId ? 'session-input-active' : ''}`}><i />{lease?.leaseId ? '可输入' : '只读'}</span>
            <span class="session-dimensions" aria-label={`终端尺寸 ${worker.rows} 行 ${worker.cols} 列`}>{worker.rows} × {worker.cols}</span>
            <span class="session-lease-count">{activeThreadLeases} Thread{activeThreadLeases === 1 ? '' : 's'}</span>
          </div>
          <div class="session-toolbar-actions">{worker.activeTurns?.map((turn) => <button class="session-interrupt-action" type="button" key={`${turn.codexThreadId}:${turn.codexTurnId}`}
            onClick={() => void interrupt(turn.codexThreadId, turn.codexTurnId)} disabled={busy}>
            Interrupt {turn.codexTurnId.slice(0, 8)}
          </button>)}
          <button class="session-stop-action" type="button" aria-label="停止 Worker" onClick={() => void stop()} disabled={busy || ['stopping','exited'].includes(worker.state)}><span aria-hidden="true">■</span> 停止</button></div>
        </div>
        <div class="session-context-strip" aria-label="Session context">
          <span class="session-context-item session-context-cwd"><small>cwd</small><code title={worker.cwd}>{worker.cwd}</code></span>
          {primaryThreadId && <span class="session-context-item"><small>thread</small><code title={primaryThreadId}>{primaryThreadId}</code></span>}
          {sourceIdSummary && <span class="session-context-item"><small>source</small><code title={sourceIdSummary}>{sourceIdSummary}</code></span>}
        </div>
        {partial && <div class="terminal-partial" role="alert">terminal_partial：checkpoint 无法证明完整画面；请使用 History Viewer 核对持久历史。</div>}
        {attachment && <Suspense fallback={<div class="terminal-loading" role="status">正在加载 xterm…</div>}>
          <TerminalPanel key={attachment.descriptor} workerId={worker.workerId} attachment={attachment} leaseId={lease?.leaseId}
            onConnected={acquireInput} onDisconnected={reconnect} onWorker={updateWorkerSnapshot}
            onSnapshot={(snapshot: TerminalSnapshotFrame) => setPartial(!snapshot.complete)} onError={setError} />
        </Suspense>}
        {!attachment && !['stopping','exited','failed'].includes(worker.state) && <div class="terminal-loading" role="status">正在准备短期 terminal attachment…</div>}
      </div>}
      {error && <div class="notice notice-warning" role="alert">{error}</div>}
      {actionResult && <div class="notice" role="status">{actionResult}</div>}
    </section>
  </div>;
}
