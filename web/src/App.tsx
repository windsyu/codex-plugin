import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { Api, connect, loadDashboard, loadThread } from './api';
import { renderMarkdown } from './lib/markdown';
import type {
  Health,
  Item,
  ProjectSummary,
  RawEvent,
  SearchResult,
  Source,
  Thread,
  ThreadDetail,
  Turn
} from './types';

const savedToken = sessionStorage.getItem('observer-token') || '';

function formatTime(ms?: number) {
  return ms
    ? new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(ms))
    : '时间未知';
}

function badge(value: string | undefined) {
  return <span class={`badge badge-${value || 'unknown'}`}>{(value || 'unknown').replaceAll('_', ' ')}</span>;
}

function jsonText(value: unknown) {
  return value == null ? '' : typeof value === 'string' ? value : JSON.stringify(value, null, 2);
}

function unwrapString(value: unknown): string {
  if (value == null) return '';
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) {
    return value.map(unwrapString).join('\n');
  }
  if (typeof value === 'object') {
    const object = value as Record<string, unknown>;
    return object.text != null ? String(object.text) : JSON.stringify(value);
  }
  return String(value);
}

function itemBlobRefs(item: Item): { blobId: string; size: number }[] {
  const raw = item.raw as Record<string, unknown> | undefined;
  const refs = raw?.blobRefs;
  if (!Array.isArray(refs)) return [];
  return refs
    .filter((ref): ref is Record<string, unknown> => typeof ref === 'object' && ref !== null)
    .map((ref) => ({ blobId: String(ref.blobId || ''), size: Number(ref.size || 0) }))
    .filter((ref) => ref.blobId);
}

function messagePhase(item: Item) {
  const raw = item.raw as Record<string, unknown> | undefined;
  return raw?.phase as string | undefined;
}

function AuthPanel({ onConnect, error }: { onConnect: (token: string) => void; error: string }) {
  const [value, setValue] = useState('');
  return (
    <section class="auth-panel">
      <p class="eyebrow">LOCAL · READ ONLY</p>
      <label for="token">Bearer token（见配置中的 token 文件）</label>
      <div class="auth-row">
        <input
          id="token"
          type="password"
          autocomplete="off"
          value={value}
          onInput={(event) => setValue((event.target as HTMLInputElement).value)}
          placeholder="粘贴本机 token"
        />
        <button onClick={() => onConnect(value.trim())}>连接</button>
      </div>
      <p class="error" role="alert">{error}</p>
    </section>
  );
}

function ContextPanel({ thread }: { thread: Thread }) {
  const { session, runtime } = thread.context;
  const instructions = session.baseInstructions as { text?: string } | undefined;
  const instructionText = instructions?.text;
  return (
    <details class="context-panel" open>
      <summary>背景上下文</summary>
      <div class="context-grid">
        <div class="context-card">
          <h4>Session Instructions</h4>
          {instructionText ? (
            <div class="markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(instructionText) }} />
          ) : (
            <p class="thread-meta">未记录</p>
          )}
        </div>
        <div class="context-card">
          <h4>Session Metadata</h4>
          <pre>{jsonText({
            agentNickname: session.agentNickname,
            agentRole: session.agentRole,
            agentPath: session.agentPath,
            originator: session.originator,
            cliVersion: session.cliVersion,
            threadSource: session.threadSource,
            historyMode: session.historyMode,
            historyBase: session.historyBase,
            modelProvider: session.modelProvider,
            dynamicTools: session.dynamicTools,
            selectedCapabilityRoots: session.selectedCapabilityRoots,
            memoryMode: session.memoryMode,
            multiAgentVersion: session.multiAgentVersion,
            contextWindow: session.contextWindow
          })}</pre>
        </div>
        <div class="context-card">
          <h4>Runtime Context</h4>
          <pre>{jsonText(runtime)}</pre>
        </div>
      </div>
    </details>
  );
}

function ItemCard({ item, token, onBlob }: { item: Item; token: string; onBlob: (message: string) => void }) {
  const raw = item.raw as Record<string, unknown> | undefined;
  const summary = item.summaryText || unwrapString(raw);
  const phase = messagePhase(item);
  const refs = itemBlobRefs(item);
  return (
    <article class={`item item-${item.itemType}`}>
      <div class="item-heading">
        <span class="item-type">{item.itemType.replaceAll('_', ' ')}</span>
        <span class="item-heading-right">
          {phase && <span class="message-phase">{phase.replaceAll('_', ' ')}</span>}
          {badge(item.status)}
        </span>
      </div>
      {summary && (
        <div class="item-content markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(summary) }} />
      )}
      {(item.itemType === 'approval' || item.itemType === 'user_question') && (
        <p class="readonly-notice">Observer V1 为只读模式，请在原 Codex 客户端中处理该请求。</p>
      )}
      {refs.map((ref) => (
        <button
          class="blob-download"
          type="button"
          onClick={() => downloadBlob(ref.blobId, token, onBlob)}
        >
          下载已脱敏大 payload（{ref.size} bytes）
        </button>
      ))}
      <details class="item-raw">
        <summary>已脱敏 JSON / provenance</summary>
        <pre>{JSON.stringify({ raw: item.raw, provenance: item.provenance }, null, 2)}</pre>
      </details>
    </article>
  );
}

function TurnSection({
  turn,
  items,
  token,
  onBlob
}: {
  turn: Turn | null;
  items: Item[];
  token: string;
  onBlob: (message: string) => void;
}) {
  return (
    <section class="turn">
      <div class="turn-heading">
        <h3>{turn ? `Turn ${turn.turnId}` : '未归属事件'}</h3>
        {turn && badge(turn.captureCompleteness)}
      </div>
      {turn?.context && Object.keys(turn.context).length > 0 && (
        <div class="turn-context">{jsonText(turn.context)}</div>
      )}
      {items.map((item) => (
        <ItemCard key={`${item.turnScope}:${item.itemId}`} item={item} token={token} onBlob={onBlob} />
      ))}
    </section>
  );
}

function ThreadDetailView({
  detail,
  turns,
  items,
  events,
  token,
  onBack,
  onBlob
}: {
  detail: ThreadDetail;
  turns: Turn[];
  items: Item[];
  events: RawEvent[];
  token: string;
  onBack: () => void;
  onBlob: (message: string) => void;
}) {
  const { thread, relations } = detail;
  const grouped = useMemo(() => {
    const map = new Map<string, { turn: Turn | null; items: Item[] }>();
    for (const turn of turns) map.set(turn.turnId, { turn, items: [] });
    for (const item of items) {
      const key = item.turnId || item.turnScope;
      if (!map.has(key)) map.set(key, { turn: null, items: [] });
      map.get(key)!.items.push(item);
    }
    return Array.from(map.entries());
  }, [turns, items]);
  const relationLabels = [];
  if (relations.parent) relationLabels.push(`parent: ${relations.parent.name || relations.parent.threadKey}`);
  if (relations.forkedFrom) relationLabels.push(`forked from: ${relations.forkedFrom.name || relations.forkedFrom.threadKey}`);
  if (relations.children.length) relationLabels.push(`${relations.children.length} child Thread`);

  return (
    <div>
      <button class="back-button" type="button" onClick={onBack}>← 返回列表</button>
      <div class="thread-header">
        <p class="eyebrow">{thread.archived ? 'ARCHIVED THREAD' : 'DURABLE THREAD'}</p>
        <h2>{thread.name || thread.codexThreadId}</h2>
        <p class="thread-meta">
          {[thread.context.session.modelProvider, thread.context.runtime.model, thread.context.session.agentNickname,
            thread.context.session.agentRole, thread.context.runtime.cwd, thread.source].filter(Boolean).join(' · ')}
        </p>
        {relationLabels.length > 0 && <p class="thread-relations">{relationLabels.join(' · ')}</p>}
      </div>
      <div class="capture-banner">
        {badge(thread.captureCompleteness)}
        <span>{thread.completenessReasons.length ? thread.completenessReasons.join(' · ') : '持久化历史已观察到终止事件'}</span>
      </div>
      <ContextPanel thread={thread} />
      <div class="timeline">
        {grouped.length === 0 && <p class="empty-list">此 Thread 尚无可投影 Item；可在下方查看 raw event。</p>}
        {grouped.map(([key, group]) => (
          <TurnSection key={key} turn={group.turn} items={group.items} token={token} onBlob={onBlob} />
        ))}
      </div>
      <details class="diagnostics">
        <summary>原始事件与诊断</summary>
        <pre>{JSON.stringify({ diagnostics: detail.diagnostics, events }, null, 2)}</pre>
      </details>
    </div>
  );
}

function Sidebar({
  health,
  projects,
  threads,
  sources,
  filters,
  setFilters,
  onSearch,
  selected,
  onSelect
}: {
  health?: Health;
  projects: ProjectSummary[];
  threads: Thread[];
  sources: Source[];
  filters: Filters;
  setFilters: (filters: Filters) => void;
  onSearch: (query: string) => Promise<void>;
  selected?: string;
  onSelect: (threadKey: string) => void;
}) {
  const [search, setSearch] = useState('');
  const visibleProjects = useMemo(() => {
    const visibleThreadKeys = new Set(threads.map((thread) => thread.threadKey));
    return projects.filter((project) =>
      threads.some((thread) => thread.project.key === project.project.key && visibleThreadKeys.has(thread.threadKey))
    );
  }, [projects, threads]);

  function runSearch() {
    const query = search.trim();
    void onSearch(query);
  }

  return (
    <aside class="sidebar">
      <div class="sidebar-head">
        <div class="search-row">
          <input
            type="search"
            value={search}
            onInput={(event) => setSearch((event.target as HTMLInputElement).value)}
            onSearch={runSearch}
            placeholder="搜索消息和工具摘要"
          />
          <button onClick={runSearch} aria-label="搜索">搜索</button>
        </div>
      </div>
      <div class="filters" aria-label="Thread 筛选">
        <select value={filters.source} onChange={(event) => setFilters({ ...filters, source: (event.target as HTMLSelectElement).value })}>
          <option value="">全部 source</option>
          {sources.map((source) => <option value={source.sourceId}>{source.sourceId}</option>)}
        </select>
        <select value={filters.status} onChange={(event) => setFilters({ ...filters, status: (event.target as HTMLSelectElement).value })}>
          <option value="">全部状态</option>
          <option>active</option>
          <option>idle</option>
          <option>not_loaded</option>
        </select>
        <select value={filters.completeness} onChange={(event) => setFilters({ ...filters, completeness: (event.target as HTMLSelectElement).value })}>
          <option value="">全部完整性</option>
          <option>durable_complete</option>
          <option>durable_partial</option>
          <option>live_complete</option>
          <option>live_partial</option>
          <option>metadata_only</option>
          <option>ephemeral_lost</option>
        </select>
        <select value={filters.archived} onChange={(event) => setFilters({ ...filters, archived: (event.target as HTMLSelectElement).value })}>
          <option value="">当前与归档</option>
          <option value="false">仅当前</option>
          <option value="true">仅归档</option>
        </select>
      </div>
      <div class="source-summary">
        {health?.status} · {sources.length} 个 source · {threads.length} 个 Thread
      </div>
      <div class="project-tree">
        {visibleProjects.length === 0 && <p class="empty-list">尚未导入 Thread。检查 CODEX_HOME 后运行 import。</p>}
        {visibleProjects.map((project) => {
          const projectThreads = threads.filter((thread) => thread.project.key === project.project.key);
          return (
            <details class="project-group" open>
              <summary class="project-heading">
                <span class="project-title">
                  <strong>{project.project.name}</strong>
                  <small>{project.project.path}</small>
                </span>
                <span class="project-count">{projectThreads.length}</span>
              </summary>
              <div class="thread-list">
                {projectThreads.map((thread) => (
                  <button
                    type="button"
                    class={`thread-row${selected === thread.threadKey ? ' selected' : ''}`}
                    onClick={() => onSelect(thread.threadKey)}
                  >
                    <div class="thread-row-top">
                      <strong>{thread.name || thread.lastMessagePreview || thread.codexThreadId}</strong>
                      {badge(thread.captureCompleteness)}
                    </div>
                    <p>{thread.lastMessagePreview || '仅有元数据'}</p>
                    <small>{formatTime(thread.recencyAtMs)}{thread.archived ? ' · archived' : ''}</small>
                  </button>
                ))}
              </div>
            </details>
          );
        })}
      </div>
    </aside>
  );
}

interface Filters {
  source: string;
  status: string;
  completeness: string;
  archived: string;
  q: string;
}

const defaultFilters: Filters = { source: '', status: '', completeness: '', archived: '', q: '' };

async function downloadBlob(blobId: string, token: string, onBlob: (message: string) => void) {
  try {
    const response = await fetch(`/v1/blobs/${encodeURIComponent(blobId)}`, {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
      credentials: 'same-origin',
      cache: 'no-store'
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const url = URL.createObjectURL(await response.blob());
    const link = document.createElement('a');
    link.href = url;
    link.download = 'observer-redacted-blob.json';
    link.click();
    URL.revokeObjectURL(url);
  } catch (error) {
    onBlob(`Blob 下载失败：${(error as Error).message}`);
  }
}

export function App() {
  const [token, setToken] = useState(savedToken);
  const [api, setApi] = useState<Api | null>(() => (savedToken ? connect(savedToken) : null));
  const [authError, setAuthError] = useState('');
  const [health, setHealth] = useState<Health>();
  const [threads, setThreads] = useState<Thread[]>([]);
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [sources, setSources] = useState<Source[]>([]);
  const [selected, setSelected] = useState<string>();
  const [detail, setDetail] = useState<ThreadDetail>();
  const [turns, setTurns] = useState<Turn[]>([]);
  const [items, setItems] = useState<Item[]>([]);
  const [events, setEvents] = useState<RawEvent[]>([]);
  const [filters, setFilters] = useState<Filters>(defaultFilters);
  const [searchKeys, setSearchKeys] = useState<Set<string> | null>(null);
  const [blobMessage, setBlobMessage] = useState('');
  const refreshTimer = useRef<number>();

  useEffect(() => {
    const code = new URLSearchParams(window.location.hash.slice(1)).get('pair');
    if (!code) return;
    history.replaceState(null, '', `${location.pathname}${location.search}`);
    fetch('/v1/auth/pair', {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code })
    })
      .then(async (response) => {
        const body = await response.json();
        if (!response.ok) throw new Error(body?.error?.message || `HTTP ${response.status}`);
        sessionStorage.removeItem('observer-token');
        setToken('');
        setApi(connect(''));
      })
      .catch((error) => setAuthError((error as Error).message));
  }, []);

  useEffect(() => {
    if (!api) return;
    let cancelled = false;

    async function refresh() {
      if (!api || cancelled) return;
      try {
        const dashboard = await loadDashboard(api);
        if (cancelled) return;
        setHealth(dashboard.health.data);
        setThreads(dashboard.threads.data);
        setProjects(dashboard.projects.data);
        setSources(dashboard.sources.data);
        if (selected) await selectThread(api, selected, cancelled);
      } catch (error) {
        if (cancelled) return;
        const message = (error as Error).message;
        if (message.includes('UNAUTHORIZED') || message.includes('401')) {
          sessionStorage.removeItem('observer-token');
          setApi(null);
          setAuthError(message);
        } else {
          setHealth((current) => ({ ...(current as Health), status: 'degraded' } as Health));
        }
      }
    }

    refresh();
    const stream = new EventSource('/v1/stream');
    stream.addEventListener('event', () => {
      window.clearTimeout(refreshTimer.current);
      refreshTimer.current = window.setTimeout(refresh, 150);
    });
    stream.addEventListener('error', () => {
      setHealth((current) => ({ ...(current as Health), status: 'degraded' } as Health));
    });
    const interval = window.setInterval(refresh, 15000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
      window.clearTimeout(refreshTimer.current);
      stream.close();
    };
  }, [api, selected]);

  async function selectThread(apiValue: Api, threadKey: string, cancelled = false) {
    setSelected(threadKey);
    try {
      const loaded = await loadThread(apiValue, threadKey);
      if (cancelled) return;
      setDetail(loaded.detail.data);
      setTurns(loaded.turns.data);
      setItems(loaded.items.data);
      setEvents(loaded.events.data);
    } catch (error) {
      setBlobMessage((error as Error).message);
    }
  }

  function handleConnect(value: string) {
    setAuthError('');
    if (value) sessionStorage.setItem('observer-token', value);
    setToken(value);
    setApi(connect(value));
  }

  async function handleSearch(query: string) {
    if (!api) return;
    if (!query) {
      setSearchKeys(null);
      setFilters({ ...filters, q: '' });
      return;
    }
    try {
      const results = await api.getAll<SearchResult>(`/v1/search?q=${encodeURIComponent(query)}&limit=200`);
      setSearchKeys(new Set(results.data.map((result) => result.threadKey)));
      setFilters({ ...filters, q: query });
    } catch (error) {
      setBlobMessage(`搜索失败：${(error as Error).message}`);
    }
  }

  const filteredThreads = useMemo(() => {
    return threads.filter((thread) => {
      const projectMatches = !filters.source || thread.storeSourceId === filters.source;
      const statusMatches = !filters.status || thread.status === filters.status;
      const completenessMatches = !filters.completeness || thread.captureCompleteness === filters.completeness;
      const archivedMatches = !filters.archived || String(thread.archived) === filters.archived;
      const searchMatches = !searchKeys || searchKeys.has(thread.threadKey);
      return projectMatches && statusMatches && completenessMatches && archivedMatches && searchMatches;
    });
  }, [threads, filters, searchKeys]);

  if (!api) {
    return (
      <main>
        <header class="topbar">
          <div>
            <p class="eyebrow">LOCAL · READ ONLY</p>
            <h1>Codex Observer</h1>
          </div>
        </header>
        <AuthPanel onConnect={handleConnect} error={authError} />
      </main>
    );
  }

  return (
    <main>
      <header class="topbar">
        <div>
          <p class="eyebrow">LOCAL · READ ONLY</p>
          <h1>Codex Observer</h1>
        </div>
        <div class={`health health-${health?.status || 'healthy'}`}>
          {health?.status || 'loading'} · event {health?.asOfEventSeq || 0}
        </div>
      </header>
      {blobMessage && <div class="source-summary strong-warning">{blobMessage}</div>}
      <div class="workspace">
        <Sidebar
          health={health}
          projects={projects}
          threads={filteredThreads}
          sources={sources}
          filters={filters}
          setFilters={setFilters}
          onSearch={handleSearch}
          selected={selected}
          onSelect={(key) => selectThread(api, key)}
        />
        <section class="detail">
          <div class="detail-inner">
            {!detail ? (
              <div class="empty-state">
                <p class="eyebrow">THREAD → TURN → ITEM</p>
                <h2>选择一个 Thread</h2>
                <p>查看持久化时间线、完整性说明和已脱敏原始事件。</p>
              </div>
            ) : (
              <ThreadDetailView
                detail={detail}
                turns={turns}
                items={items}
                events={events}
                token={token}
                onBack={() => {
                  setSelected(undefined);
                  setDetail(undefined);
                }}
                onBlob={setBlobMessage}
              />
            )}
          </div>
        </section>
      </div>
    </main>
  );
}
