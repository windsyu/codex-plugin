(() => {
  const $ = id => document.getElementById(id);
  const cards = new Map();
  const responses = new Map();
  const issueKeys = new Set();
  let epoch = null, sequence = 0, source = null, retry = null, connecting = false, following = true;
  const statuses = { receiving: '本次模型响应接收中', completed: '本次模型响应已结束 · 不代表任务结束', failed: '本次模型响应失败', incomplete: '本次模型响应不完整' };
  const issues = { observation_gap: '观察副本有缺口，原生转发继续', interrupted: '捕获流提前结束', incomplete_frame: '末尾事件不完整', invalid_json: '事件无法解析，已省略原始内容', invalid_web_socket: 'WebSocket 观察帧无法解析', unsupported_compression: '当前观察器不支持此压缩格式', unsupported_content: '当前内容类型尚未接入', too_large: '观察事件超过大小限制', capacity: '观察或阅读缓存达到上限', unknown_event: '遇到未知事件，已保留安全诊断', missing_identity: '内容无法确认归属', conflicting_identity: '响应身份存在冲突', omitted_by_policy: '部分内容按当前正文捕获范围省略' };
  const keyOf = key => JSON.stringify(key);
  const responseOf = (requestId, responseId) => JSON.stringify([requestId, responseId]);
  function statusFor(item) { return responses.get(responseOf(item.key.requestId, item.key.responseId)); }
  function updateStatus(card) { const status = statusFor(card.item); card.status.textContent = status ? statuses[status] : '已捕获正文 · 响应状态未确认'; }
  function showItem(item) {
    if (item.kind !== 'message' || item.author?.role !== 'assistant') return;
    const key = item.itemKey;
    item = { ...item, key: item.evidence[0].key, text: item.content.map(part => part.text).join('\n') };
    let card = cards.get(key);
    if (!card) {
      const article = document.createElement('article');
      article.dataset.requestId = item.key.requestId;
      const meta = document.createElement('div'); meta.className = 'meta';
      const badge = document.createElement('span'); badge.className = 'badge'; badge.textContent = '◉ Codex';
      const identity = document.createElement('span'); identity.textContent = `请求 ${item.key.requestId.slice(0, 8)} · ${item.key.wireItemId}`;
      const text = document.createElement('pre'); text.className = 'text';
      const truncated = document.createElement('div'); truncated.className = 'truncated';
      const status = document.createElement('div'); status.className = 'response-status';
      meta.append(badge, identity); article.append(meta, text, truncated, status); $('messages').append(article);
      card = { item, article, text, truncated, status }; cards.set(key, card);
    }
    card.item = item; card.text.textContent = item.text;
    card.truncated.textContent = item.truncated ? '正文预览已截断，当前没有完整保存副本。' : '';
    updateStatus(card); $('empty')?.remove();
  }
  function diagnostic(entry) {
    const key = JSON.stringify([entry.requestId, entry.code]);
    if (issueKeys.has(key)) return;
    issueKeys.add(key);
    const li = document.createElement('li'); li.textContent = `${issues[entry.code] || '未知捕获状态'} · ${entry.requestId.slice(0, 8)} / ${entry.captureSeq}`;
    $('issues').append(li); $('notice').hidden = false;
    $('notice').textContent = '当前阅读内容不完整。详情见下方捕获记录；原生 CLI 保持独立运行。';
    $('capture').textContent = '捕获：存在缺口或省略';
  }
  function renderSnapshot(snapshot) {
    if (snapshot.schemaVersion !== 2) throw new Error('incompatible reading snapshot');
    epoch = snapshot.runEpoch; sequence = snapshot.viewSeq;
    cards.clear(); responses.clear(); issueKeys.clear(); $('messages').replaceChildren(); $('issues').replaceChildren();
    $('notice').hidden = snapshot.capture === 'ok';
    $('capture').textContent = snapshot.capture === 'ok' ? '捕获：正文观察正常' : '捕获：内容不完整';
    for (const response of snapshot.responses) responses.set(responseOf(response.requestId, response.responseId), response.status);
    for (const item of snapshot.items) showItem(item);
    for (const issue of snapshot.diagnostics) diagnostic(issue);
    if (!cards.size) { const empty = document.createElement('p'); empty.id = 'empty'; empty.textContent = '等待模型正文…'; $('messages').append(empty); }
  }
  function follow() { if (following) requestAnimationFrame(() => window.scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' })); else $('follow').hidden = false; }
  function consume(event) {
    const value = JSON.parse(event.data);
    if (value.runEpoch !== epoch || value.viewSeq > sequence + 1) return reconnect();
    if (value.viewSeq <= sequence) return;
    if (value.kind === 'item.patch') {
      if (value.field === 'text') {
        const previous = cards.get(value.itemKey)?.item;
        if (!previous || previous.revision !== value.baseRevision || value.revision !== value.baseRevision + 1 || !previous.content.some(part => part.contentKey === value.contentKey)) return reconnect();
        showItem({ ...previous, revision: value.revision, content: previous.content.map(part => part.contentKey === value.contentKey ? { ...part, text: part.text + value.append } : part), truncated: value.truncated });
      }
    } else if (value.kind === 'item.replace') showItem(value.item);
    else if (value.kind === 'request.state') {
      responses.set(responseOf(value.response.requestId, value.response.responseId), value.response.status);
      for (const card of cards.values()) updateStatus(card);
    } else if (['request.metadata', 'user.capture', 'tool.context', 'native.command', 'native.file_change'].includes(value.kind)) { /* R0 remains a response-only diagnostic page. */ }
    else if (value.kind === 'capture.gap') diagnostic(value.diagnostic);
    else return reconnect();
    sequence = value.viewSeq; document.body.dataset.viewSeq = String(sequence); follow();
  }
  function reconnect() {
    source?.close(); source = null; $('connection').textContent = '重新连接中';
    if (!retry) retry = setTimeout(() => { retry = null; connect(); }, 300);
  }
  async function connect() {
    if (connecting) return;
    connecting = true;
    try {
      const response = await fetch('/workbench/v1/live/snapshot', { credentials: 'same-origin', cache: 'no-store' });
      if (response.status === 401) { $('connection').textContent = '需要本次运行的配对入口'; return; }
      if (!response.ok) throw new Error('snapshot unavailable');
      renderSnapshot(await response.json()); document.body.dataset.viewSeq = String(sequence);
      source = new EventSource(`/workbench/v1/live/events?epoch=${encodeURIComponent(epoch)}&after=${sequence}`);
      source.onopen = () => { $('connection').textContent = '已连接'; };
      source.addEventListener('view', event => { try { consume(event); } catch { reconnect(); } });
      source.addEventListener('snapshot_required', reconnect); source.onerror = reconnect;
    } catch { reconnect(); } finally { connecting = false; }
  }
  window.addEventListener('scroll', () => { following = window.scrollY + window.innerHeight >= document.documentElement.scrollHeight - 70; if (following) $('follow').hidden = true; }, { passive: true });
  $('follow').addEventListener('click', () => { following = true; $('follow').hidden = true; follow(); });
  (async () => {
    const pair = new URLSearchParams(location.hash.slice(1)).get('pair');
    history.replaceState(null, '', location.pathname);
    if (pair) {
      const result = await fetch('/workbench/v1/pair', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token: pair }), credentials: 'same-origin' });
      if (!result.ok) { $('connection').textContent = '配对失败，请使用本次运行的入口'; return; }
    }
    await connect();
  })().catch(() => { $('connection').textContent = '连接失败'; });
})();
