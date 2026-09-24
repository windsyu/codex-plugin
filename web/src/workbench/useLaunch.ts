import { useEffect, useRef, useState } from 'preact/hooks';
import { browserId } from './browserId';
import { launchMessage as message } from './launchMessages';
import type { LaunchPreview } from './LaunchPanel';
import type { LibraryEntry } from './library';

interface Intent { path?: string; projectId?: string; resumeEntryId?: string; sourceRevision?: string }
interface Operation { operationId: string; state: 'starting' | 'ready' | 'existing' | 'failed'; openInNewTab?: boolean; run?: { runId: string }; error?: { code: string } }
async function request<T>(path: string, signal: AbortSignal, body?: unknown): Promise<T> {
  const response = await fetch(`/workbench/v1/${path}`, { method: body ? 'POST' : 'GET', credentials: 'same-origin', cache: 'no-store', signal,
    ...(body ? { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {}) });
  const value = await response.json();
  if (!response.ok) throw Object.assign(new Error(message(value?.error?.code)), { code: value?.error?.code, definitive: response.status < 500 });
  return value;
}
export function useLaunch(instanceId: string, enter: (runId: string) => void) {
  const storageKey = `wb-launch:${instanceId}`;
  const [open, setOpen] = useState(false), [intent, setIntent] = useState<Intent>({ path: '' });
  const [preview, setPreview] = useState<LaunchPreview>(), [busy, setBusy] = useState(false), [error, setError] = useState('');
  const [readyHref, setReadyHref] = useState('');
  const popup = useRef<Window | null>(null), recovered = useRef(false);
  const closePopup = () => { try { if (popup.current && !popup.current.closed && popup.current.location.href === 'about:blank') popup.current.close(); } catch { /* Never close a tab navigated elsewhere. */ } popup.current = null; };
  const [posting, setPosting] = useState(false);
  const [pending, setPending] = useState(false), [uncertain, setUncertain] = useState(false), [poll, setPoll] = useState(0);
  const [picking, setPicking] = useState(false);
  const picker = useRef<AbortController>();
  const operation = useRef<string | null>(null), preparing = useRef<AbortController>(), active = useRef(true), entering = useRef(enter);
  entering.current = enter;
  useEffect(() => {
    active.current = true;
    try { const id = sessionStorage.getItem(storageKey); if (id) { operation.current = id; recovered.current = true; setOpen(true); setPending(true); } } catch { /* Saving is checked before starting. */ }
    return () => { active.current = false; closePopup(); preparing.current?.abort(); picker.current?.abort(); };
  }, [storageKey]);
  const forget = () => { operation.current = null; try { sessionStorage.removeItem(storageKey); } catch { /* No automatic repost even if browser storage fails. */ } };
  const result = (value: Operation) => {
    if (value.operationId !== operation.current) throw new Error('启动结果身份不匹配。');
    if (value.state === 'ready' || value.state === 'existing') {
      if (!value.run?.runId) throw new Error('启动结果缺少工作台信息。');
      setPending(false); setUncertain(false); setPreview(undefined);
      const href = `/?run=${encodeURIComponent(value.run.runId)}`;
      if (value.openInNewTab || recovered.current || popup.current) {
        setReadyHref(href);
        try { if (popup.current && !popup.current.closed && popup.current.location.href === 'about:blank') { popup.current.opener = null; popup.current.location.replace(href); } } catch { /* The explicit link remains usable. */ }
        popup.current = null;
      } else { closePopup(); forget(); entering.current(value.run.runId); }
    } else if (value.state === 'failed') { closePopup(); forget(); setPending(false); setUncertain(false); setPreview(undefined); setError(message(value.error?.code || '')); }
  };
  useEffect(() => {
    if (!pending || !operation.current) return;
    const controller = new AbortController(); let timer: number, count = 0, disposed = false;
    const update = async () => {
      try {
        const deadline = window.setTimeout(() => controller.abort(), 10000);
        let value: Operation;
        try { value = await request<Operation>(`launch-operations/${encodeURIComponent(operation.current!)}`, controller.signal); }
        finally { clearTimeout(deadline); }
        if (!active.current || disposed || controller.signal.aborted) return;
        result(value);
        if (value.state === 'starting') {
          if (++count >= 40) { setPending(false); setUncertain(true); }
          else timer = window.setTimeout(() => void update(), 500);
        }
      } catch (e) {
        if (active.current && !disposed) {
          setPending(false);
          if ((e as { code?: string }).code === 'operation_unavailable') { closePopup(); forget(); setPreview(undefined); setUncertain(false); }
          else setUncertain(true);
          setError((e as Error).name === 'AbortError' ? '查询暂时超时，可继续查询同一启动操作。' : message((e as { code?: unknown } | null)?.code));
        }
      }
    };
    void update(); return () => { disposed = true; controller.abort(); clearTimeout(timer); };
  }, [pending, poll, storageKey]);
  useEffect(() => {
    if (!preview) return;
    const timer = window.setTimeout(() => { setPreview(undefined); if (!operation.current) setError(message('target_expired')); }, preview.expiresInSeconds * 1000);
    return () => clearTimeout(timer);
  }, [preview]);
  const validate = async (next: Intent = intent) => {
    if (operation.current || picker.current) return;
    preparing.current?.abort(); const controller = new AbortController(); preparing.current = controller;
    setPreview(undefined); setError(''); setBusy(true);
    const deadline = window.setTimeout(() => controller.abort(), 10000);
    try { const value = await request<LaunchPreview>('launch-targets', controller.signal, next); if (active.current && preparing.current === controller && !controller.signal.aborted) setPreview(value); }
    catch (e) { if (active.current && preparing.current === controller) setError((e as Error).name === 'AbortError' ? '目录检查已取消或超时，请重新检查。' : message((e as { code?: unknown } | null)?.code)); }
    finally { clearTimeout(deadline); if (active.current && preparing.current === controller) { preparing.current = undefined; setBusy(false); } }
  };
  const choose = (next: Intent, check: boolean) => {
    setOpen(true); if (readyHref) { forget(); setReadyHref(''); recovered.current = false; } if (operation.current || picker.current) return;
    preparing.current?.abort(); preparing.current = undefined; setBusy(false); setIntent(next); setPreview(undefined); setError('');
    if (check) void validate(next);
  };
  const pickDirectory = async () => {
    if (readyHref) { forget(); setReadyHref(''); recovered.current = false; }
    if (operation.current || picker.current) return;
    preparing.current?.abort(); preparing.current = undefined; setBusy(false);
    const controller = new AbortController(); picker.current = controller;
    setPicking(true); setError('');
    const deadline = window.setTimeout(() => controller.abort(), 185000);
    try {
      const value = await request<{ path: string | null }>('application/pick-directory', controller.signal, { instanceId });
      if (!active.current || picker.current !== controller) return;
      if (value.path !== null) {
        if (typeof value.path !== 'string' || !value.path) throw Object.assign(new Error(), { code: 'picker_invalid_result' });
        picker.current = undefined;
        choose({ path: value.path }, true);
      }
    } catch (e) {
      if (active.current && picker.current === controller) {
        setOpen(true); setIntent({ path: '' }); setPreview(undefined);
        setError((e as Error).name === 'AbortError' ? '目录选择已超时，可以直接输入路径。' : message((e as { code?: unknown } | null)?.code));
      }
    } finally {
      clearTimeout(deadline);
      if (picker.current === controller) picker.current = undefined;
      if (active.current) setPicking(false);
    }
  };
  const start = async () => {
    if (!preview || busy || operation.current || picker.current) return;
    const id = browserId();
    try { sessionStorage.setItem(storageKey, id); } catch { setError('浏览器无法保存启动状态，请允许当前页面的本地存储后重试。'); return; }
    operation.current = id; recovered.current = false;
    if (preview.openInNewTab) { try { popup.current = window.open('about:blank', '_blank'); if (popup.current) popup.current.opener = null; } catch { popup.current = null; } }
    setPosting(true); setError(''); setUncertain(false);
    const controller = new AbortController(), deadline = window.setTimeout(() => controller.abort(), 10000);
    try {
      const value = await request<Operation>('runs', controller.signal, { instanceId, targetId: preview.targetId, mode: intent.resumeEntryId ? 'resume' : 'new', operationId: id, configRevision: preview.configRevision });
      if (active.current) { result(value); if (value.state === 'starting') setPending(true); }
    } catch (e) {
      if (active.current) {
        if ((e as { definitive?: boolean }).definitive) { closePopup(); forget(); setPreview(undefined); setError(message((e as { code?: unknown } | null)?.code)); }
        else { setUncertain(true); setError('启动响应未收到。请查询已有操作，避免重复启动。'); }
      }
    } finally { clearTimeout(deadline); if (active.current) setPosting(false); }
  };
  return { open, picking, pickDirectory, operationActive: !readyHref && (posting || pending || uncertain || !!operation.current), choose,
    openProject: (projectId: string) => choose({ projectId }, true),
    openResume: (entry: LibraryEntry) => choose({ resumeEntryId: entry.entryId, sourceRevision: entry.sourceRevision }, true),
    panel: { path: intent.path || preview?.canonicalPath || '', pathEditable: !intent.projectId && !intent.resumeEntryId, mode: intent.resumeEntryId ? 'resume' as const : 'new' as const,
      preview, readyHref, busy, picking, onPick: () => void pickDirectory(), pending: !readyHref && (posting || pending || (!!operation.current && !uncertain)), uncertain, error,
      onPathChange: (path: string) => choose({ path }, false), onValidate: () => void validate(), onStart: () => void start(),
      onCheck: () => { setError(''); setUncertain(false); setPending(true); setPoll(n => n + 1); }, onClose: () => setOpen(false), onEnter: (runId: string) => {
        if (!preview?.openInNewTab) { enter(runId); return; }
        const href = `/?run=${encodeURIComponent(runId)}`;
        // noopener may return null even when opening succeeds; always keep a usable link.
        setReadyHref(href); setPreview(undefined);
        try { window.open(href, '_blank', 'noopener'); } catch { /* Use the explicit link. */ }
      } }
  };
}
