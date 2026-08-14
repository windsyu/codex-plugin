const state = { token: '', threads: [], selected: null, sources: [], stream: null, refreshTimer: null };

const $ = (id) => document.getElementById(id);

function headers() {
  return state.token ? { Authorization: `Bearer ${state.token}` } : {};
}

async function api(path) {
  const response = await fetch(path, { headers: headers(), credentials: 'same-origin', cache: 'no-store' });
  const body = await response.json();
  if (!response.ok) throw new Error(body?.error?.message || `HTTP ${response.status}`);
  return body;
}

async function apiAll(path) {
  const url = new URL(path, window.location.origin);
  const data = [];
  let envelope;
  do {
    envelope = await api(`${url.pathname}${url.search}`);
    data.push(...envelope.data);
    if (envelope.nextCursor) url.searchParams.set('cursor', envelope.nextCursor);
  } while (envelope.nextCursor);
  return { ...envelope, data };
}

async function apiAllEvents(path) {
  const url = new URL(path, window.location.origin);
  const data = [];
  let envelope;
  do {
    envelope = await api(`${url.pathname}${url.search}`);
    data.push(...envelope.data);
    const last = envelope.data.at(-1);
    if (last) url.searchParams.set('afterEventSeq', last.eventSeq);
  } while (envelope.data.length === 200);
  return { ...envelope, data };
}

function text(node, value) {
  node.textContent = value == null ? '' : String(value);
  return node;
}

function element(tag, className, value) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (value != null) text(node, value);
  return node;
}

function badge(value) {
  return element('span', `badge badge-${value || 'unknown'}`, (value || 'unknown').replaceAll('_', ' '));
}

function formatTime(ms) {
  return ms ? new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(ms)) : '时间未知';
}

async function connect() {
  state.token = $('token').value.trim();
  $('auth-error').textContent = '';
  try {
    const [health, threads, sources, settings] = await Promise.all([api('/v1/health'), apiAll('/v1/threads?limit=200'), api('/v1/sources'), api('/v1/meta/settings')]);
    if (state.token) sessionStorage.setItem('observer-token', state.token);
    $('health').className = `health health-${health.data.status}`;
    const legacy = health.data.privacy?.legacyRedactionEvents || 0;
    $('health').textContent = `${health.data.status} · event ${health.asOfEventSeq}${legacy ? ` · ${legacy} legacy privacy records` : ''}`;
    $('auth').classList.add('hidden');
    $('workspace').classList.remove('hidden');
    state.threads = threads.data;
    state.sources = sources.data;
    populateSourceFilter();
    applyFilters();
    renderSourceHealth(health.data, sources.data);
    text($('settings'), JSON.stringify(settings.data, null, 2));
    startStream();
  } catch (error) {
    $('auth-error').textContent = error.message;
  }
}

function populateSourceFilter() {
  const select = $('filter-source');
  const current = select.value;
  select.replaceChildren(new Option('全部 source', ''));
  for (const source of state.sources) select.append(new Option(source.sourceId, source.sourceId));
  select.value = current;
}

function applyFilters() {
  const source = $('filter-source').value;
  const status = $('filter-status').value;
  const completeness = $('filter-completeness').value;
  const archived = $('filter-archived').value;
  const filtered = state.threads.filter((thread) =>
    (!source || thread.storeSourceId === source) && (!status || thread.status === status) &&
    (!completeness || thread.captureCompleteness === completeness) &&
    (!archived || String(thread.archived) === archived));
  renderThreads(filtered);
  $('source-summary').textContent = `${state.sources.length} 个 source · ${filtered.length}/${state.threads.length} 个 Thread`;
}

function renderSourceHealth(health, sources) {
  const panel = $('source-health');
  panel.replaceChildren();
  if (health.privacy?.legacyRedactionEvents) panel.append(element('p', 'warning strong-warning',
    `隐私警告：${health.privacy.legacyRedactionEvents} 条历史事件使用旧脱敏规则，可能仍含敏感值。`));
  for (const source of sources) {
    const epoch = source.currentEpoch || {};
    const stale = source.status !== 'ready' && source.status !== 'online';
    panel.append(element('p', stale ? 'warning strong-warning' : 'source-line',
      `${source.kind} · ${source.status}${stale ? ' · DISCONNECTED / STALE' : ''} · events ${epoch.eventCount || 0} · gaps ${epoch.sequenceGapCount || 0}`));
  }
}

function startStream() {
  if (state.stream) state.stream.close();
  state.stream = new EventSource('/v1/stream');
  state.stream.addEventListener('event', () => {
    clearTimeout(state.refreshTimer);
    state.refreshTimer = setTimeout(refresh, 150);
  });
  state.stream.addEventListener('error', () => {
    $('health').className = 'health health-degraded';
    $('health').textContent = 'DISCONNECTED / STALE — 正在重连';
  });
}

async function refresh() {
  try {
    const [health, threads, sources] = await Promise.all([api('/v1/health'), apiAll('/v1/threads?limit=200'), api('/v1/sources')]);
    state.threads = threads.data; state.sources = sources.data;
    applyFilters(); renderSourceHealth(health.data, sources.data);
    $('health').className = `health health-${health.data.status}`;
    $('health').textContent = `${health.data.status} · event ${health.asOfEventSeq}`;
    if (state.selected) await selectThread(state.selected);
  } catch (error) {
    $('health').textContent = `DISCONNECTED / STALE — ${error.message}`;
  }
}

function renderThreads(threads) {
  const list = $('threads');
  list.replaceChildren();
  if (!threads.length) {
    list.append(element('p', 'empty-list', '尚未导入 Thread。检查 CODEX_HOME 后运行 import。'));
    return;
  }
  for (const thread of threads) {
    const button = element('button', `thread-row${state.selected === thread.threadKey ? ' selected' : ''}`);
    button.type = 'button';
    const top = element('div', 'thread-row-top');
    top.append(element('strong', '', thread.name || thread.lastMessagePreview || thread.codexThreadId), badge(thread.captureCompleteness));
    button.append(top);
    button.append(element('p', 'preview', thread.lastMessagePreview || '仅有元数据'));
    button.append(element('small', '', `${formatTime(thread.recencyAtMs)}${thread.archived ? ' · archived' : ''}`));
    button.addEventListener('click', () => selectThread(thread.threadKey));
    list.append(button);
  }
}

async function selectThread(threadKey) {
  state.selected = threadKey;
  renderThreads(state.threads);
  const encoded = encodeURIComponent(threadKey);
  const [detail, turns, items, events] = await Promise.all([
    api(`/v1/threads/${encoded}`), apiAll(`/v1/threads/${encoded}/turns?limit=200`),
    apiAll(`/v1/threads/${encoded}/items?limit=200`), apiAllEvents(`/v1/threads/${encoded}/events?limit=200`)
  ]);
  $('empty').classList.add('hidden');
  $('thread-detail').classList.remove('hidden');
  renderHeader(detail.data.thread, detail.data.relations);
  renderTimeline(turns.data, items.data);
  text($('diagnostics'), JSON.stringify(detail.data.diagnostics, null, 2));
  renderPending(detail.data.pendingRequests || []);
  text($('projection-conflicts'), JSON.stringify(detail.data.projectionConflicts || [], null, 2));
  text($('raw-events'), JSON.stringify(events.data, null, 2));
}

function renderPending(requests) {
  const panel = $('pending-requests');
  panel.replaceChildren();
  for (const request of requests.filter((request) => request.state !== 'resolved')) {
    panel.append(element('p', 'readonly-notice strong-warning',
      `PENDING ${request.requestType} · ${request.state} — Observer 不会响应，请回到 Codex 客户端处理。`));
  }
}

function renderHeader(thread, relations) {
  const header = $('thread-header');
  header.replaceChildren();
  header.append(element('p', 'eyebrow', thread.archived ? 'ARCHIVED THREAD' : 'DURABLE THREAD'));
  header.append(element('h2', '', thread.name || thread.codexThreadId));
  header.append(element('p', 'thread-meta', [thread.model, thread.modelProvider, thread.agentNickname, thread.agentRole, thread.cwdDisplay, thread.source].filter(Boolean).join(' · ')));
  const relationLabels = [];
  if (relations?.parent) relationLabels.push(`parent: ${relations.parent.name || relations.parent.codexThreadId}`);
  if (relations?.forkedFrom) relationLabels.push(`forked from: ${relations.forkedFrom.name || relations.forkedFrom.codexThreadId}`);
  if (relations?.children?.length) relationLabels.push(`${relations.children.length} child Thread`);
  if (relationLabels.length) header.append(element('p', 'thread-relations', relationLabels.join(' · ')));
  const banner = $('capture-banner');
  banner.replaceChildren(badge(thread.captureCompleteness));
  banner.append(element('span', '', thread.completenessReasons.length ? thread.completenessReasons.join(' · ') : '持久化历史已观察到终止事件'));
}

function renderTimeline(turns, items) {
  const timeline = $('timeline');
  timeline.replaceChildren();
  const grouped = new Map();
  for (const turn of turns) grouped.set(turn.turnId, { turn, items: [] });
  for (const item of items) {
    const key = item.turnId || item.turnScope;
    if (!grouped.has(key)) grouped.set(key, { turn: null, items: [] });
    grouped.get(key).items.push(item);
  }
  for (const [key, group] of grouped) {
    const section = element('section', 'turn');
    const heading = element('div', 'turn-heading');
    heading.append(element('h3', '', group.turn ? `Turn ${key}` : '未归属事件'));
    if (group.turn) heading.append(badge(group.turn.captureCompleteness));
    section.append(heading);
    for (const item of group.items) section.append(renderItem(item));
    timeline.append(section);
  }
  if (!grouped.size) timeline.append(element('p', 'empty-list', '此 Thread 尚无可投影 Item；可在下方查看 raw event。'));
}

function renderItem(item) {
  const card = element('article', `item item-${item.itemType}`);
  const heading = element('div', 'item-heading');
  heading.append(element('span', 'item-type', item.itemType.replaceAll('_', ' ')), badge(item.status));
  card.append(heading);
  if (item.summaryText) card.append(element('div', 'item-content', item.summaryText));
  if (item.itemType === 'approval' || item.itemType === 'user_question') {
    card.append(element('p', 'readonly-notice', 'Observer V1 为只读模式，请在原 Codex 客户端中处理该请求。'));
  }
  for (const blobRef of item.raw?.blobRefs || []) {
    const download = element('button', 'blob-download', `下载已脱敏大 payload（${blobRef.size} bytes）`);
    download.type = 'button';
    download.addEventListener('click', () => downloadBlob(blobRef.blobId, download));
    card.append(download);
  }
  const details = element('details', 'item-raw');
  details.append(element('summary', '', '已脱敏 JSON / provenance'));
  details.append(element('pre', '', JSON.stringify({ raw: item.raw, provenance: item.provenance }, null, 2)));
  card.append(details);
  return card;
}

async function downloadBlob(blobId, button) {
  button.disabled = true;
  try {
    const response = await fetch(`/v1/blobs/${encodeURIComponent(blobId)}`, { headers: headers(), credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const url = URL.createObjectURL(await response.blob());
    const link = document.createElement('a');
    link.href = url;
    link.download = 'observer-redacted-blob.json';
    link.click();
    URL.revokeObjectURL(url);
  } catch (error) {
    $('source-summary').textContent = `Blob 下载失败：${error.message}`;
  } finally {
    button.disabled = false;
  }
}

async function search() {
  const query = $('search').value.trim();
  if (!query) return renderThreads(state.threads);
  try {
    const results = await apiAll(`/v1/search?q=${encodeURIComponent(query)}&limit=200`);
    const keys = new Set(results.data.map((result) => result.threadKey));
    renderThreads(state.threads.filter((thread) => keys.has(thread.threadKey)));
  } catch (error) {
    $('source-summary').textContent = `搜索失败：${error.message}`;
  }
}

$('connect').addEventListener('click', connect);
$('token').addEventListener('keydown', (event) => { if (event.key === 'Enter') connect(); });
$('search-button').addEventListener('click', search);
$('search').addEventListener('search', search);
for (const id of ['filter-source', 'filter-status', 'filter-completeness', 'filter-archived']) $(id).addEventListener('change', applyFilters);

async function pairFromFragment() {
  const params = new URLSearchParams(window.location.hash.slice(1));
  const code = params.get('pair');
  if (!code) return false;
  history.replaceState(null, '', `${location.pathname}${location.search}`);
  const response = await fetch('/v1/auth/pair', { method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ code }) });
  const body = await response.json();
  if (!response.ok) throw new Error(body?.error?.message || `HTTP ${response.status}`);
  state.token = '';
  await connect();
  return true;
}

const saved = sessionStorage.getItem('observer-token');
pairFromFragment().catch((error) => { $('auth-error').textContent = error.message; }).then((paired) => {
if (!paired && saved) {
  $('token').value = saved;
  connect();
} else if (!paired) connect();
});
