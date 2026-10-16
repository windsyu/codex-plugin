import './directoryBrowser.css';
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { AUTH_INVALID, workbenchFetch } from './context';

export interface DirectoryListing {
  instanceId: string;
  currentPath: string;
  parentPath: string | null;
  homePath: string;
  roots: { name: string; path: string }[];
  entries: { name: string; path: string; isSymlink: boolean }[];
  canSelect: boolean;
  selectionError?: string | null;
  nextCursor?: string | null;
  truncated: boolean;
}

interface Props {
  instanceId: string;
  initialPath: string;
  onClose: () => void;
  onSelect: (path: string) => void;
}
interface Query { path: string | null; filter: string; showHidden: boolean; cursor: string | null; revision: number }
interface BrowseError { message: string; code?: string }
const messages: Record<string, string> = {
  directory_busy: '电脑正在读取其他目录，请稍后重试。',
  directory_unavailable: '目录不存在或无法读取，请检查路径，或选择其他目录。',
  directory_forbidden: '没有读取此目录的权限，请选择其他目录。',
  invalid_directory_path: '目录路径无效，请输入绝对路径或以 ~/ 开头的路径。',
  absolute_path_required: '请输入绝对路径或以 ~/ 开头的路径。',
  invalid_directory_filter: '筛选条件过长或无效，请缩短目录名称。',
  invalid_directory_cursor: '目录列表已过期，请重新载入当前目录。',
  directory_cursor_expired: '目录列表已过期，请重新载入当前目录。',
  directory_changed: '目录发生了变化，请重新载入当前目录。',
  stale_instance: '电脑上的工作台已重启，请重新打开工作台。',
};
const safeMessage = (code?: string) => code && Object.hasOwn(messages, code) ? messages[code] : '未能读取目录，电脑可能已离线。请检查连接后重试。';
const fallbackCodes = new Set(['directory_unavailable', 'directory_forbidden', 'invalid_directory_path', 'absolute_path_required']);

function breadcrumbs(path: string) {
  let current = '';
  return [{ name: '/', path: '/' }, ...path.split('/').filter(Boolean).map(name => {
    current += `/${name}`;
    return { name, path: current };
  })];
}

export function DirectoryBrowser({ instanceId, initialPath, onClose, onSelect }: Props) {
  const [query, setQuery] = useState<Query>({ path: initialPath.trim() || null, filter: '', showHidden: false, cursor: null, revision: 0 });
  const [pathDraft, setPathDraft] = useState(initialPath);
  const [filterDraft, setFilterDraft] = useState('');
  const [listing, setListing] = useState<DirectoryListing>();
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<BrowseError>();
  const [notice, setNotice] = useState('');
  const [blocked, setBlocked] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const controller = useRef<AbortController>();
  const invalid = useRef(false);
  const generation = useRef(0);
  const firstRequest = useRef(true);
  const selected = useRef(false);
  const opener = useRef<HTMLElement | null>(null);
  const shortcuts = useRef<Pick<DirectoryListing, 'homePath' | 'roots'>>();
  const instance = useRef(instanceId);

  const changeQuery = (next: Partial<Query>) => {
    if (invalid.current) return;
    generation.current++;
    controller.current?.abort();
    setListing(undefined); setError(undefined); setBusy(true);
    setQuery(previous => ({ ...previous, ...next, revision: previous.revision + 1 }));
  };
  const navigate = (path: string | null) => {
    firstRequest.current = false;
    setPathDraft(path || ''); setFilterDraft(''); setNotice('');
    changeQuery({ path, filter: '', cursor: null });
  };
  useLayoutEffect(() => {
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const element = dialog.current;
    if (element && !element.open) element.showModal?.();
    // Focus a button so opening the picker never summons a mobile keyboard.
    closeButton.current?.focus({ preventScroll: true });
    return () => {
      element?.close?.();
      const restore = () => {
        const target = opener.current;
        if (!target?.isConnected) return;
        if (target instanceof HTMLButtonElement && target.disabled) target.closest('dialog')?.querySelector<HTMLElement>('button:not(:disabled)')?.focus({ preventScroll: true });
        else target.focus({ preventScroll: true });
      };
      restore();
      // Preact may remove this dialog before re-enabling its opener.
      queueMicrotask(restore);
    };
  }, []);
  useLayoutEffect(() => {
    if (instance.current === instanceId) return;
    instance.current = instanceId; generation.current++; controller.current?.abort();
    invalid.current = false; selected.current = false; firstRequest.current = true; shortcuts.current = undefined;
    setListing(undefined); setNotice(''); setError(undefined); setBlocked(false); setBusy(true);
    setPathDraft(initialPath); setFilterDraft('');
    setQuery(previous => ({ path: initialPath.trim() || null, filter: '', showHidden: false, cursor: null, revision: previous.revision + 1 }));
  }, [instanceId]);
  useEffect(() => {
    const revoke = () => {
      invalid.current = true; generation.current++; controller.current?.abort(); shortcuts.current = undefined;
      setListing(undefined); setPathDraft(''); setFilterDraft(''); setNotice(''); setBusy(false); setBlocked(true);
      setError({ message: '登录或工作台授权已失效，请返回门户重新登录并打开工作台。' });
    };
    window.addEventListener(AUTH_INVALID, revoke);
    return () => { invalid.current = true; generation.current++; controller.current?.abort(); window.removeEventListener(AUTH_INVALID, revoke); };
  }, []);
  useEffect(() => {
    if (invalid.current || (filterDraft === query.filter && !controller.current?.signal.aborted)) return;
    const timer = window.setTimeout(() => changeQuery({ filter: filterDraft, cursor: null }), 200);
    return () => clearTimeout(timer);
  }, [filterDraft, query.filter]);
  useEffect(() => {
    if (invalid.current) return;
    const request = new AbortController(); controller.current = request;
    const requestGeneration = ++generation.current;
    let timedOut = false;
    const deadline = window.setTimeout(() => { timedOut = true; request.abort(); }, 12000);
    const current = () => !invalid.current && controller.current === request && generation.current === requestGeneration;
    const load = async () => {
      try {
        const response = await workbenchFetch('/workbench/v1/application/directories', {
          method: 'POST', credentials: 'same-origin', cache: 'no-store', signal: request.signal,
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ instanceId, path: query.path, filter: query.filter, showHidden: query.showHidden, cursor: query.cursor }),
        });
        const value = await response.json();
        if (!current() || request.signal.aborted) return;
        if (!response.ok) {
          const code = typeof value?.error?.code === 'string' ? value.error.code : undefined;
          if (response.status === 401 || (response.status === 403 && code !== 'directory_forbidden')) {
            window.dispatchEvent(new Event(AUTH_INVALID)); return;
          }
          if (firstRequest.current && query.path && fallbackCodes.has(code || '')) {
            firstRequest.current = false;
            setNotice('原目录无法访问，已返回电脑的主目录。'); setPathDraft('');
            changeQuery({ path: null, cursor: null }); return;
          }
          throw { code, message: safeMessage(code) } satisfies BrowseError;
        }
        if (value?.instanceId !== instanceId) throw { code: 'stale_instance', message: messages.stale_instance } satisfies BrowseError;
        if (typeof value.currentPath !== 'string' || typeof value.homePath !== 'string' || !Array.isArray(value.roots) || !Array.isArray(value.entries)) throw new Error();
        firstRequest.current = false; shortcuts.current = { homePath: value.homePath, roots: value.roots };
        setListing(value as DirectoryListing); setPathDraft(value.currentPath); setError(undefined);
      } catch (cause) {
        if (!current()) return;
        if (request.signal.aborted && !timedOut) return;
        const code = (cause as BrowseError)?.code;
        firstRequest.current = false;
        setListing(undefined);
        setError({ code, message: timedOut ? '读取目录超时，电脑可能已离线。请检查连接后重试。' : safeMessage(code) });
        if (code === 'stale_instance') { invalid.current = true; shortcuts.current = undefined; setPathDraft(''); setFilterDraft(''); setNotice(''); setBusy(false); setBlocked(true); }
      } finally {
        clearTimeout(deadline);
        if (current()) setBusy(false);
      }
    };
    void load();
    return () => { request.abort(); clearTimeout(deadline); };
  }, [instanceId, query]);

  const activeListing = listing?.instanceId === instanceId ? listing : undefined;
  const pendingFilter = filterDraft !== query.filter;
  const canSelect = !!activeListing?.canSelect && pathDraft.trim() === activeListing.currentPath && !busy && !pendingFilter && !blocked;
  const choose = () => {
    if (!canSelect || !activeListing || invalid.current || selected.current) return;
    selected.current = true; onSelect(activeListing.currentPath);
  };
  const shortcut = shortcuts.current;
  return <dialog ref={dialog} className="wb-directory-browser" aria-labelledby="wb-directory-title" aria-describedby="wb-directory-description"
    onCancel={event => { event.preventDefault(); event.stopPropagation(); onClose(); }}
    onKeyDown={event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
      if (event.key !== 'Tab') return;
      const controls = Array.from(dialog.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), [tabindex="0"]') || []);
      const first = controls[0], last = controls[controls.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }}>
    <header className="wb-directory-heading"><h2 id="wb-directory-title">浏览电脑目录</h2><button ref={closeButton} type="button" onClick={onClose}>取消</button></header>
    <div className="wb-directory-body">
      <p id="wb-directory-description" className="wb-directory-description">选择已连接电脑上的项目目录。选择后会检查目录，不会立即启动项目。</p>
      {!blocked && <form className="wb-directory-path" onSubmit={event => { event.preventDefault(); if (pathDraft.trim()) navigate(pathDraft.trim()); }}>
        <label>目录路径<input name="directory-path" aria-label="浏览目录路径" autoComplete="off" spellcheck={false} value={instance.current === instanceId ? pathDraft : ''} onInput={event => setPathDraft(event.currentTarget.value)} placeholder="例如 ~/projects…" /></label>
        <button type="submit" disabled={!pathDraft.trim()}>前往</button>
      </form>}
      <div className="wb-directory-selection"><span>{activeListing && !blocked ? activeListing.currentPath : '选择目录后继续检查'}</span><button type="button" className="wb-directory-select" disabled={!canSelect} onClick={choose}>选择此目录</button></div>
      {!blocked && <>
        <nav className="wb-directory-shortcuts" aria-label="目录快捷入口">
          <button type="button" onClick={() => navigate(shortcut?.homePath || null)}>主目录</button>
          {(shortcut?.roots || []).filter(root => root.path !== shortcut?.homePath).map(root => <button key={root.path} type="button" onClick={() => navigate(root.path)} title={root.path}>{root.name}</button>)}
        </nav>
        {activeListing && <nav className="wb-directory-breadcrumbs" aria-label="当前目录"><ol>{breadcrumbs(activeListing.currentPath).map((part, index, all) => <li key={part.path}><button type="button" onClick={() => navigate(part.path)} aria-current={index === all.length - 1 ? 'location' : undefined} title={part.path}>{part.name}</button></li>)}</ol></nav>}
        <div className="wb-directory-tools"><button type="button" onClick={() => activeListing?.parentPath && navigate(activeListing.parentPath)} disabled={!activeListing?.parentPath}>上一级</button>
          <label className="wb-directory-filter">筛选当前层<input name="directory-filter" aria-label="筛选当前层目录" autoComplete="off" spellcheck={false} value={filterDraft} onInput={event => {
            generation.current++; controller.current?.abort(); setListing(undefined); setBusy(true); setFilterDraft(event.currentTarget.value);
          }} placeholder="目录名称…" /></label>
          <label className="wb-directory-hidden"><input type="checkbox" checked={query.showHidden} onChange={event => changeQuery({ showHidden: event.currentTarget.checked, cursor: null })} />显示隐藏目录</label>
        </div>
      </>}
      {notice && !blocked && <p className="wb-directory-notice" role="status">{notice}</p>}
      {error && <div className="wb-directory-error" role="alert"><p>{error.message}</p>{!blocked && <button type="button" onClick={() => changeQuery({ cursor: null })}>重新载入目录</button>}</div>}
      <div className="wb-directory-results" aria-busy={busy || pendingFilter} aria-live="polite">
        {(busy || pendingFilter) && !blocked && <p className="wb-directory-state" role="status">正在读取目录…</p>}
        {activeListing && !busy && !pendingFilter && <>
          {activeListing.entries.length ? <ul className="wb-directory-list" aria-label="子目录">{activeListing.entries.map(entry => <li key={entry.path}><button type="button" onClick={() => navigate(entry.path)} title={entry.path}>
            <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true"><path d="M2 5a1 1 0 0 1 1-1h5l2 2h7a1 1 0 0 1 1 1v9H2Z" /></svg>
            <span>{entry.name}</span>{entry.isSymlink && <small>链接</small>}<span className="wb-directory-arrow" aria-hidden="true">›</span>
          </button></li>)}</ul> : <p className="wb-directory-state">{query.filter ? '当前层没有匹配的子目录。' : '此目录下没有可显示的子目录。'}</p>}
          {activeListing.truncated && <p className="wb-directory-notice">目录较大或部分内容无法读取，列表不完整。可输入更具体的路径或名称筛选。</p>}
          {query.cursor && <button className="wb-directory-more" type="button" onClick={() => changeQuery({ path: activeListing.currentPath, cursor: null })}>返回第一页</button>}
          {activeListing.nextCursor && <button className="wb-directory-more" type="button" onClick={() => changeQuery({ path: activeListing.currentPath, cursor: activeListing.nextCursor || null })}>下一页目录</button>}
          {!activeListing.canSelect && <p className="wb-directory-notice">此目录不能用于启动项目，请选择其他目录。{activeListing.selectionError === 'absolute_path_required' ? '需要绝对路径。' : ''}</p>}
        </>}
      </div>
    </div>
  </dialog>;
}
