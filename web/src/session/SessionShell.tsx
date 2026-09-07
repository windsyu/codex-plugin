import { lazy, Suspense } from 'preact/compat';
import { useEffect, useRef, useState } from 'preact/hooks';

import { Api, ApiError, reconnectingStream } from '../api';
import type { GatewayCommand, InputLease, ProjectSummary, SessionAttachment, SessionSource, SessionWorker, SessionWorkerSnapshot, TerminalSnapshotFrame } from '../types';

const TerminalPanel = lazy(async () => ({ default: (await import('./TerminalPanel')).TerminalPanel }));
const WORKER_KEY = 'session-runtime-worker-v1';
const ATTACHMENT_KEY = 'session-runtime-attachment-v1';

interface StoredAttachmentControl { attachmentId: string; attachmentToken: string; }
interface StoredWorkerControl { workerId: string; sourceId: string; sourceEpoch: string; intentKey: string; }

export type SessionIntent =
  | { mode: 'new'; fresh?: boolean }
  | { mode: 'resume'; storeSourceId: string; codexThreadId: string; cwd: string };

interface SessionShellProps { api: Api; intent: SessionIntent; projects?: ProjectSummary[]; onClose: () => void; }
type StartupPhase = 'discovering' | 'starting_app_server' | 'connecting_proxy' | 'starting_tui' | 'ready' | 'failed';

function errorMessage(error: unknown) {
  if (error instanceof ApiError && error.code) return `${error.code} · ${error.message}`;
  return error instanceof Error ? error.message : 'Session Runtime 请求失败';
}

function clearStoredSession() {
  sessionStorage.removeItem(WORKER_KEY);
  sessionStorage.removeItem(ATTACHMENT_KEY);
}

function readStoredAttachmentControl(): StoredAttachmentControl | undefined {
  const serialized = sessionStorage.getItem(ATTACHMENT_KEY);
  if (!serialized) return undefined;
  try {
    const value = JSON.parse(serialized) as Partial<StoredAttachmentControl>;
    if (typeof value.attachmentId !== 'string' || !/^[A-Za-z0-9._-]{1,160}$/.test(value.attachmentId)
      || typeof value.attachmentToken !== 'string' || !/^[0-9a-f]{64}$/.test(value.attachmentToken)) throw new Error('invalid');
    return { attachmentId: value.attachmentId, attachmentToken: value.attachmentToken };
  } catch { sessionStorage.removeItem(ATTACHMENT_KEY); return undefined; }
}

function readStoredWorkerControl(): StoredWorkerControl | undefined {
  const serialized = sessionStorage.getItem(WORKER_KEY);
  if (!serialized) return undefined;
  try {
    const value = JSON.parse(serialized) as Partial<StoredWorkerControl>;
    if (![value.workerId, value.sourceId, value.sourceEpoch, value.intentKey]
      .every((field) => typeof field === 'string' && field.length > 0 && field.length <= 512)) throw new Error('invalid');
    return value as StoredWorkerControl;
  } catch { clearStoredSession(); return undefined; }
}

function storeAttachmentControl(attachment: SessionAttachment) {
  sessionStorage.setItem(ATTACHMENT_KEY, JSON.stringify({
    attachmentId: attachment.attachmentId, attachmentToken: attachment.attachmentToken
  } satisfies StoredAttachmentControl));
}

function sessionIntentKey(source: SessionSource, intent: SessionIntent, cwd: string) {
  return [source.storeSourceId, source.sourceId, source.sourceEpoch, intent.mode,
    intent.mode === 'resume' ? intent.codexThreadId : '', cwd].join('\u0000');
}

function sourceForIntent(sources: SessionSource[], intent: SessionIntent) {
  return intent.mode === 'resume'
    ? sources.find((source) => source.storeSourceId === intent.storeSourceId && source.status === 'ready')
    : sources.find((source) => source.status === 'ready');
}

export function sessionMatchesIntent(worker: SessionWorker, source: SessionSource, intent: SessionIntent, cwd: string) {
  const persisted = worker.persistedWorker;
  if (!persisted || persisted.sourceId !== source.sourceId || persisted.sourceEpoch !== source.sourceEpoch
    || persisted.mode !== intent.mode || worker.cwd !== cwd) return false;
  if (['stopping', 'exited', 'failed', 'stale_epoch'].includes(worker.state)) return false;
  return worker.threadLeases?.some((lease) => ['acquiring', 'active'].includes(lease.state)
    && (intent.mode === 'new' ? Boolean(lease.reservationId || lease.codexThreadId)
      : lease.codexThreadId === intent.codexThreadId)) === true;
}

export function sessionOwnsInitialThread(worker: SessionWorker, initialThread?: { sourceId: string; sourceEpoch: string; codexThreadId: string }) {
  if (!initialThread) return true;
  return worker.persistedWorker?.sourceId === initialThread.sourceId
    && worker.persistedWorker.sourceEpoch === initialThread.sourceEpoch
    && worker.threadLeases?.some((lease) => lease.codexThreadId === initialThread.codexThreadId
      && ['acquiring', 'active'].includes(lease.state)) === true;
}

export function mergeSessionWorkerSnapshot(current: SessionWorker, snapshot: SessionWorkerSnapshot): SessionWorker {
  if (current.workerId !== snapshot.workerId) return current;
  return { ...current, ...snapshot, persistedWorker: current.persistedWorker,
    threadLeases: current.threadLeases, activeTurns: current.activeTurns };
}

function phaseLabel(phase: StartupPhase) {
  return ({ discovering: '正在选择 Session Source…', starting_app_server: '正在启动专属 App Server…',
    connecting_proxy: '正在连接 audited proxy…', starting_tui: '正在启动 Codex TUI…',
    ready: 'Codex TUI 已就绪', failed: '自动启动失败' } as Record<StartupPhase, string>)[phase];
}

export function SessionShell({ api, intent, projects = [], onClose }: SessionShellProps) {
  const [sources, setSources] = useState<SessionSource[]>([]);
  const [selectedSource, setSelectedSource] = useState<SessionSource>();
  const [cwd, setCwd] = useState(intent.mode === 'resume' && intent.cwd.startsWith('/') ? intent.cwd : '');
  const [mode, setMode] = useState<'new' | 'resume'>(intent.mode);
  const [threadId, setThreadId] = useState(intent.mode === 'resume' ? intent.codexThreadId : '');
  const [worker, setWorker] = useState<SessionWorker>();
  const [attachment, setAttachment] = useState<SessionAttachment>();
  const [lease, setLease] = useState<InputLease>();
  const [partial, setPartial] = useState(false);
  const [busy, setBusy] = useState(true);
  const [manualRetry, setManualRetry] = useState(false);
  const [choosingDirectory, setChoosingDirectory] = useState(false);
  const [phase, setPhase] = useState<StartupPhase>('discovering');
  const [error, setError] = useState('');
  const [actionResult, setActionResult] = useState('');
  const reconnectTimer = useRef<number>();
  const attaching = useRef(false);
  // The SSE subscription starts before attachment creation completes. Its callbacks must
  // compare ownership with the current attachment, not the render that opened the stream.
  const attachmentIdRef = useRef<string>();
  attachmentIdRef.current = attachment?.attachmentId;

  async function prepareAttachment(currentWorker: SessionWorker, resume = true) {
    if (attaching.current) return;
    attaching.current = true;
    try {
      const stored = resume ? readStoredAttachmentControl() : undefined;
      const response = await api.post<SessionAttachment>(`/v2/sessions/${encodeURIComponent(currentWorker.workerId)}/attach`, {
        resumeAttachmentId: stored?.attachmentId || null, resumeAttachmentToken: stored?.attachmentToken || null
      }, crypto.randomUUID());
      storeAttachmentControl(response.data);
      setAttachment(response.data); setPhase('ready'); setError('');
    } catch (cause) {
      if (resume && cause instanceof ApiError && ['ATTACHMENT_NOT_FOUND', 'ATTACHMENT_EXPIRED', 'ATTACHMENT_TOKEN_INVALID',
        'ATTACHMENT_TOKEN_REQUIRED', 'ATTACHMENT_PRINCIPAL_MISMATCH'].includes(cause.code || '')) {
        sessionStorage.removeItem(ATTACHMENT_KEY); attaching.current = false;
        return prepareAttachment(currentWorker, false);
      }
      setPhase('failed'); setError(errorMessage(cause)); setManualRetry(true);
    } finally { attaching.current = false; }
  }

  async function createSession(source: SessionSource, requestedMode: 'new' | 'resume', requestedThreadId: string, requestedCwd: string, signal?: AbortSignal) {
    if (!requestedCwd.startsWith('/')) throw new Error('cwd 必须是服务端可访问的绝对目录');
    if (requestedMode === 'resume' && !requestedThreadId.trim()) throw new Error('恢复会话需要 Codex Thread ID');
    setPhase('starting_app_server');
    const response = await api.post<SessionWorker>('/v2/sessions', {
      storeSourceId: source.storeSourceId, sourceId: source.sourceId, sourceEpoch: source.sourceEpoch,
      expectedSupervisorVersion: source.supervisorVersion, mode: requestedMode,
      codexThreadId: requestedMode === 'resume' ? requestedThreadId.trim() : null,
      cwd: requestedCwd, rows: 24, cols: 80
    }, crypto.randomUUID(), signal);
    setPhase('connecting_proxy');
    const launchedIntent: SessionIntent = requestedMode === 'new' ? { mode: 'new' } : {
      mode: 'resume', storeSourceId: source.storeSourceId, codexThreadId: requestedThreadId.trim(), cwd: requestedCwd
    };
    sessionStorage.setItem(WORKER_KEY, JSON.stringify({ workerId: response.data.workerId, sourceId: source.sourceId,
      sourceEpoch: source.sourceEpoch, intentKey: sessionIntentKey(source, launchedIntent, response.data.cwd) } satisfies StoredWorkerControl));
    sessionStorage.removeItem(ATTACHMENT_KEY);
    setCwd(response.data.cwd); setWorker(response.data); setPhase('starting_tui');
    await prepareAttachment(response.data, false);
  }

  useEffect(() => {
    const controller = new AbortController(); let disposed = false;
    async function bootstrap() {
      setBusy(true); setPhase('discovering');
      try {
        const response = await api.get<SessionSource[]>('/v2/session-sources', controller.signal);
        if (disposed) return;
        setSources(response.data);
        let source = sourceForIntent(response.data, intent);
        if (!source) throw new Error(intent.mode === 'resume' ? 'History Thread 对应的 Session Source 不可用' : '没有可用的 Session Source');
        const stored = readStoredWorkerControl();
        if (intent.mode === 'new' && !intent.fresh && stored) {
          source = response.data.find((candidate) => candidate.sourceId === stored.sourceId
            && candidate.sourceEpoch === stored.sourceEpoch && candidate.status === 'ready') || source;
        }
        const requestedCwd = intent.mode === 'resume' ? intent.cwd : source.defaultCwd;
        setSelectedSource(source); setCwd(requestedCwd); setMode(intent.mode);
        setThreadId(intent.mode === 'resume' ? intent.codexThreadId : '');
        if (stored && !(intent.mode === 'new' && intent.fresh)
          && stored.sourceId === source.sourceId && stored.sourceEpoch === source.sourceEpoch) {
          try {
            const existing = await api.get<SessionWorker>(`/v2/sessions/${encodeURIComponent(stored.workerId)}`, controller.signal);
            const restoreCwd = intent.mode === 'new' ? existing.data.cwd : requestedCwd;
            if (stored.intentKey === sessionIntentKey(source, intent, restoreCwd)
              && sessionMatchesIntent(existing.data, source, intent, restoreCwd)) {
              setCwd(restoreCwd); setWorker(existing.data); setPhase('connecting_proxy'); await prepareAttachment(existing.data); return;
            }
          } catch (cause) { if (controller.signal.aborted) return; }
        }
        if (intent.mode === 'new') { setChoosingDirectory(true); return; }
        clearStoredSession();
        await createSession(source, intent.mode, intent.mode === 'resume' ? intent.codexThreadId : '', requestedCwd, controller.signal);
      } catch (cause) {
        if (!controller.signal.aborted && !disposed) { setPhase('failed'); setError(errorMessage(cause)); setManualRetry(true); }
      } finally { if (!disposed) setBusy(false); }
    }
    void bootstrap();
    return () => { disposed = true; controller.abort(); window.clearTimeout(reconnectTimer.current); };
  }, [api, intent.mode, intent.mode === 'new' ? intent.fresh : false, intent.mode === 'resume' ? intent.storeSourceId : '', intent.mode === 'resume' ? intent.codexThreadId : '', intent.mode === 'resume' ? intent.cwd : '']);

  useEffect(() => {
    if (!worker?.workerId || ['exited', 'failed', 'stale_epoch'].includes(worker.state)) return;
    const controller = new AbortController();
    void reconnectingStream(api, `/v2/sessions/${encodeURIComponent(worker.workerId)}/events`, (event) => {
      if (event.type === 'session_state') updateWorkerView(event.data as SessionWorker);
    }, () => undefined, controller.signal).catch((cause) => { if (!controller.signal.aborted) setError(errorMessage(cause)); });
    return () => controller.abort();
  }, [worker?.workerId, worker?.state]);

  async function retry() {
    const source = selectedSource || sources.find((candidate) => candidate.status === 'ready');
    if (!source) { setError('请选择可用的 Session Source'); return; }
    setBusy(true); setManualRetry(false); setChoosingDirectory(false); setError(''); clearStoredSession();
    try { await createSession(source, mode, threadId, cwd); }
    catch (cause) { setPhase('failed'); setError(errorMessage(cause)); setManualRetry(true); }
    finally { setBusy(false); }
  }

  async function acquireInput(): Promise<string | undefined> {
    if (!worker || !attachment) return undefined;
    try {
      const response = await api.post<InputLease>(`/v2/sessions/${encodeURIComponent(worker.workerId)}/input-lease`, {
        attachmentId: attachment.attachmentId, attachmentToken: attachment.attachmentToken,
        expectedVersion: worker.inputLease.version, takeover: false
      }, crypto.randomUUID());
      setLease(response.data); setWorker((current) => current ? { ...current, inputLease: response.data } : current); setError('');
      return response.data.leaseId || undefined;
    } catch (cause) {
      if (cause instanceof ApiError && cause.code === 'INPUT_LEASE_CONFLICT') {
        setLease(undefined); setError('该 PTY 已由另一个浏览器持有输入租约；当前为实时只读附着。');
      } else setError(errorMessage(cause));
      return undefined;
    }
  }

  function reconnect() {
    if (!worker || ['stopping', 'exited', 'failed', 'stale_epoch'].includes(worker.state)) return;
    window.clearTimeout(reconnectTimer.current); reconnectTimer.current = window.setTimeout(() => void prepareAttachment(worker), 500);
  }

  async function stop() {
    if (!worker) return;
    setBusy(true);
    try {
      const activeTurns = worker.activeTurns || [];
      const response = await api.post<SessionWorker>(`/v2/sessions/${encodeURIComponent(worker.workerId)}/stop`, worker.persistedWorker ? {
        sourceEpoch: worker.persistedWorker.sourceEpoch, expectedWorkerVersion: worker.persistedWorker.version,
        activeTurnPolicy: activeTurns.length ? 'interrupt_expected' : 'reject_if_active',
        expectedActiveTurns: activeTurns.map((turn) => ({ threadId: turn.codexThreadId, turnId: turn.codexTurnId }))
      } : {}, crypto.randomUUID());
      setWorker(response.data); setAttachment(undefined); setLease(undefined); clearStoredSession();
    } catch (cause) { setError(errorMessage(cause)); } finally { setBusy(false); }
  }

  async function interrupt(thread: string, expectedTurnId: string) {
    if (!worker?.persistedWorker) return;
    setBusy(true); setError(''); setActionResult('');
    try {
      const response = await api.post<GatewayCommand>(`/v2/sessions/${encodeURIComponent(worker.workerId)}/interrupt`, {
        sourceEpoch: worker.persistedWorker.sourceEpoch, threadId: thread, expectedTurnId,
        expectedWorkerVersion: worker.persistedWorker.version
      }, crypto.randomUUID());
      setActionResult(`interrupt · ${response.data.state} · ${response.data.commandId}`);
      setWorker((await api.get<SessionWorker>(`/v2/sessions/${encodeURIComponent(worker.workerId)}`)).data);
    } catch (cause) { setError(errorMessage(cause)); } finally { setBusy(false); }
  }

  function updateLeaseFromSnapshot(next: SessionWorkerSnapshot) {
    const currentAttachmentId = attachmentIdRef.current;
    setLease((current) => {
      if (current && current.version > next.inputLease.version) return current;
      return next.inputLease.ownerAttachmentId === currentAttachmentId && next.inputLease.leaseId
        ? next.inputLease : undefined;
    });
  }
  function updateWorkerView(next: SessionWorker) { setWorker(next); updateLeaseFromSnapshot(next); }
  function updateWorkerSnapshot(next: SessionWorkerSnapshot) {
    setWorker((current) => current ? mergeSessionWorkerSnapshot(current, next) : current); updateLeaseFromSnapshot(next);
  }

  const primaryThreadId = worker?.persistedWorker?.primaryThreadId;
  const sourceIdSummary = worker?.persistedWorker?.sourceId;
  const activeThreadLeases = worker?.threadLeases?.filter((item) => item.state === 'active').length || 0;

  return <div class="session-shell-backdrop" role="presentation"><section class="session-shell" role="dialog" aria-modal="true" aria-label="Codex terminal session">
    <header class="session-shell-header"><div class="session-shell-brand"><span class="session-shell-mark" aria-hidden="true">&gt;_</span><div>
      <p class="eyebrow">CODEX · SESSION RUNTIME</p><h2>Codex Terminal</h2></div></div>
      <button class="session-history-action" type="button" onClick={onClose} aria-label="返回 History Viewer"><span aria-hidden="true">←</span> History</button></header>
    {!worker && !manualRetry && !choosingDirectory && <div class="session-create" role="status" aria-live="polite"><strong>{phaseLabel(phase)}</strong>
      <p>正在准备 Codex 终端…</p></div>}
    {!worker && (manualRetry || choosingDirectory) && <form class="session-create" onSubmit={(event) => { event.preventDefault(); void retry(); }}>
      <h2>{mode === 'new' ? '新建对话' : '恢复对话'}</h2>
      {(sources.length > 1 || manualRetry) && <label>Codex 数据源<select value={selectedSource?.sourceId || ''} onChange={(event) => {
        const source = sources.find((candidate) => candidate.sourceId === event.currentTarget.value); setSelectedSource(source);
      }}>{!sources.some((source) => source.status === 'ready') && <option value="">没有可用 source</option>}
        {sources.filter((source) => source.status === 'ready').map((source, index) => <option key={source.sourceId} value={source.sourceId}>数据源 {index + 1} · {source.storeSourceId.slice(0, 10)}</option>)}</select></label>}
      {mode === 'resume' && <label>Codex Thread ID<input value={threadId} onInput={(event) => setThreadId(event.currentTarget.value)} placeholder="thread UUID" /></label>}
      {mode === 'new' && <label>项目<select value={projects.find((item) => item.project.path === cwd)?.project.key || ''}
        onChange={(event) => {
          const project = projects.find((item) => item.project.key === event.currentTarget.value);
          setCwd(project?.project.path || '');
        }}><option value="">自定义目录</option>{projects.map(({ project }) =>
          <option key={project.key} value={project.key}>{project.name} · {project.path}</option>)}</select></label>}
      <label>工作目录（绝对路径）<input value={cwd} onInput={(event) => setCwd(event.currentTarget.value)} placeholder="/absolute/workspace" required /></label>
      <p>Codex 将在这个目录中打开，会话历史按工作目录归入项目。</p>
      <code class="session-directory-preview">{cwd || '请选择项目或填写工作目录'}</code>
      <button type="submit" disabled={busy || !cwd.startsWith('/') || !selectedSource || (mode === 'resume' && !threadId.trim())}>{busy ? '正在启动…' : manualRetry ? '重试启动' : '启动对话'}</button></form>}
    {worker && <div class="session-worker"><div class="session-toolbar"><div class="session-status-cluster">
      <span class={`badge badge-${worker.state}`} title={`Worker ${worker.workerId}`}>{worker.state.replaceAll('_', ' ')}</span>
      <span class={`session-input-state ${lease?.leaseId ? 'session-input-active' : ''}`}><i />{lease?.leaseId ? '可输入' : '只读'}</span>
      <span class="session-dimensions" aria-label={`终端尺寸 ${worker.rows} 行 ${worker.cols} 列`}>{worker.rows} × {worker.cols}</span>
      <span class="session-lease-count">{activeThreadLeases} Thread{activeThreadLeases === 1 ? '' : 's'}</span></div>
      <div class="session-toolbar-actions">{worker.activeTurns?.map((turn) => <button class="session-interrupt-action" type="button" key={`${turn.codexThreadId}:${turn.codexTurnId}`}
        onClick={() => void interrupt(turn.codexThreadId, turn.codexTurnId)} disabled={busy}>Interrupt {turn.codexTurnId.slice(0, 8)}</button>)}
        <button class="session-stop-action" type="button" aria-label="停止 Worker" onClick={() => void stop()} disabled={busy || ['stopping','exited'].includes(worker.state)}><span aria-hidden="true">■</span> 停止</button></div></div>
      <div class="session-context-strip" aria-label="Session context"><span class="session-context-item session-context-cwd"><small>cwd</small><code title={worker.cwd}>{worker.cwd}</code></span>
        {primaryThreadId && <span class="session-context-item"><small>thread</small><code title={primaryThreadId}>{primaryThreadId}</code></span>}
        {sourceIdSummary && <span class="session-context-item"><small>source</small><code title={sourceIdSummary}>{sourceIdSummary}</code></span>}</div>
      {partial && <div class="terminal-partial" role="alert">terminal_partial：checkpoint 无法证明完整画面；请使用 History Viewer 核对持久历史。</div>}
      {attachment && <Suspense fallback={<div class="terminal-loading" role="status">正在加载 xterm…</div>}><TerminalPanel key={attachment.descriptor}
        workerId={worker.workerId} attachment={attachment} leaseId={lease?.leaseId} onConnected={acquireInput} onDisconnected={reconnect}
        onWorker={updateWorkerSnapshot} onSnapshot={(snapshot: TerminalSnapshotFrame) => setPartial(!snapshot.complete)} onError={setError} /></Suspense>}
      {!attachment && !['stopping','exited','failed'].includes(worker.state) && <div class="terminal-loading" role="status">{phaseLabel(phase)}</div>}
    </div>}
    {error && <div class="notice notice-warning" role="alert">{error}</div>}{actionResult && <div class="notice" role="status">{actionResult}</div>}
  </section></div>;
}
