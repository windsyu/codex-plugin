'use strict';
(() => {
  const uiApiVersion = 3;
  const element = id => document.getElementById(id);
  const routes = { '/login': 'signed-out', '/computers': 'workspace-access', '/authorize': 'authorization', '/account/security': 'security', '/admin/users': 'administration' };
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
  const stale = Symbol('stale');
  let account = null, epoch = 0, pageVersion = 0, usersVersion = 0, timer, pendingList = null, disposed = false;
  let pendingCode = '';
  try { pendingCode = sessionStorage.getItem('relay.pendingCode') || ''; } catch { /* Storage may be disabled. */ }
  const fragment = new URLSearchParams(location.hash.slice(1));
  if (fragment.has('bind')) pendingCode = fragment.get('bind').trim().toUpperCase();
  if (!/^[A-F0-9]{10}$/.test(pendingCode)) pendingCode = '';
  function rememberCode(code) {
    pendingCode = code;
    try { if (code) sessionStorage.setItem('relay.pendingCode', code); else sessionStorage.removeItem('relay.pendingCode'); } catch { /* Keep the current-tab intent in memory. */ }
    element('bind').elements.userCode.value = code;
    element('login-intent').hidden = !code;
  }
  rememberCode(pendingCode);
  const initialPath = fragment.has('bind') && pendingCode ? '/authorize' : location.pathname;
  history.replaceState(null, '', initialPath);
  const message = (text, error = false) => { element('status').textContent = text; element('status').dataset.error = String(error); };
  function failed(error) {
    if (error === stale || disposed) return;
    const descriptions = { 400: '输入无效。请检查用户名、密码要求或一次性码是否正确。', 401: '登录已失效，或账号密码不正确。请重新登录。', 403: '当前账号无法执行此操作。请重新登录以确认账号状态。', 404: '账号、电脑或一次性码不存在，也可能已过期。请刷新列表，或在电脑上重新发起登录。', 409: '操作冲突，名称可能已被使用，或电脑状态已改变。请刷新后重试。', 429: '操作过于频繁，请稍后再试。', 502: '电脑连接暂时中断，请检查电脑上的公网接入状态。', 503: '服务暂时不可用，请稍后重试。' };
    message(descriptions[error?.status] || '请求未完成，请检查网络后重试。', true);
  }
  function stopPolling() { clearTimeout(timer); timer = undefined; }
  function clearPrivate() {
    element('gateways').replaceChildren(); element('users').replaceChildren();
    document.querySelectorAll('input[type=password]').forEach(input => { input.value = ''; });
  }
  function signedOut(clearIntent = false) {
    epoch++; account = null; pendingList = null; stopPolling(); clearPrivate();
    if (clearIntent) rememberCode('');
    navigate('/login', true);
  }
  async function api(path, body, method) {
    const started = epoch;
    let response;
    try {
      response = await fetch('/relay/v1/' + path, {
        method: method || (body ? 'POST' : 'GET'), credentials: 'same-origin', cache: 'no-store',
        headers: body ? { 'Content-Type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined
      });
    } catch (error) {
      if (started !== epoch || disposed) throw stale;
      throw error;
    }
    if (started !== epoch || disposed) throw stale;
    if (!response.ok) {
      if ((response.status === 401 || response.status === 403) && path !== 'login') signedOut();
      throw Object.assign(new Error('request_failed'), { status: response.status });
    }
    const data = response.status === 204 ? null : await response.json();
    if (started !== epoch || disposed) throw stale;
    return data;
  }
  function node(tag, text, className) {
    const result = document.createElement(tag);
    if (text !== undefined) result.textContent = text;
    if (className) result.className = className;
    return result;
  }
  function button(label, action, className) {
    const result = node('button', label, className); result.type = 'button';
    result.onclick = async () => {
      const started = epoch;
      result.disabled = true;
      try { await action(); } catch (error) { if (started === epoch || error?.status === 401 || error?.status === 403) failed(error); }
      finally { result.disabled = false; }
    };
    return result;
  }
  function form(id, action) {
    element(id).addEventListener('submit', async event => {
      event.preventDefault();
      const target = event.currentTarget, submit = target.querySelector('button'), started = epoch;
      if (submit.disabled) return;
      submit.disabled = true;
      try { if (await action(Object.fromEntries(new FormData(target))) !== false) target.reset(); }
      catch (error) { if (started === epoch || error?.status === 401 || error?.status === 403) failed(error); }
      finally { submit.disabled = false; }
    });
  }
  function openURL(raw, gatewayId) {
    const url = new URL(raw);
    if (url.protocol !== 'https:' || url.username || url.password || url.origin !== location.origin) throw new Error('unsafe_url');
    const auth = new URLSearchParams(url.hash.slice(1));
    if (url.pathname !== '/g/' + gatewayId + '/' || url.search || !uuid.test(auth.get('grant') || '') || !uuid.test(auth.get('session') || '') || auth.get('api') !== String(uiApiVersion)) throw new Error('invalid_entry');
    return url.href;
  }
  function schedulePoll() {
    stopPolling();
    if (account && !account.mustChangePassword && location.pathname === '/computers' && !document.hidden && !disposed)
      timer = setTimeout(() => void gateways(), 4000);
  }
  async function gateways() {
    if (!account || account.mustChangePassword || location.pathname !== '/computers' || document.hidden || disposed) return;
    if (pendingList) return pendingList;
    const version = pageVersion, started = epoch;
    const task = (async () => {
      try {
        const data = await api('gateways');
        if (version !== pageVersion || started !== epoch) return;
        const items = data.gateways.map(gateway => {
          if (!uuid.test(gateway.gatewayId)) throw new Error('invalid_gateway');
          const row = node('li', undefined, 'computer-row'), info = node('div', undefined, 'computer-info');
          const state = gateway.status || (gateway.online ? 'online' : 'offline');
          const labels = { online: '在线 · 可以连接', offline: '离线 · 请保持电脑运行', awaitingConnection: '等待电脑连接', reauthenticationRequired: '电脑需要重新登录', versionIncompatible: '版本不兼容 · 请升级电脑客户端' };
          const stateNode = node('div', Object.hasOwn(labels, state) ? labels[state] : '状态未知 · 请刷新后重试', 'state'); stateNode.dataset.state = state;
          info.append(node('h2', gateway.name, 'computer-name'), stateNode);
          const open = button('打开工作台', async () => {
            const result = await api('gateways/' + gateway.gatewayId + '/open', { uiApiVersion });
            location.assign(openURL(result.url, gateway.gatewayId));
          }, 'primary');
          open.disabled = state !== 'online';
          const actions = node('div', undefined, 'row-actions');
          actions.append(open, button('撤销接入', async () => {
            if (!confirm('撤销“' + gateway.name + '”的接入？本机 Codex 将继续运行。')) return;
            await api('gateways/' + gateway.gatewayId, null, 'DELETE'); await gateways();
          }, 'quiet danger'));
          const icon = node('span', '▣', 'computer-icon'); icon.setAttribute('aria-hidden', 'true');
          row.append(icon, info, actions); return row;
        });
        if (!items.length) { const empty = node('li', undefined, 'empty'); empty.append(node('strong', '还没有接入的电脑'), node('p', '在电脑的公网接入设置中登录此账号，随后即可在这里打开工作台。')); items.push(empty); }
        element('gateways').replaceChildren(...items);
      } catch (error) { if (version === pageVersion || error?.status === 401 || error?.status === 403) failed(error); }
      finally { if (pendingList === task) pendingList = null; if (started === epoch) schedulePoll(); }
    })();
    pendingList = task;
    return task;
  }
  async function users() {
    const version = pageVersion, requestVersion = ++usersVersion;
    const data = await api('admin/users');
    if (version !== pageVersion || requestVersion !== usersVersion) return;
    const items = data.users.map(user => {
      if (!uuid.test(user.accountId)) throw new Error('invalid_account');
      const row = node('li'), heading = node('div', undefined, 'user-heading');
      const info = node('div'); info.append(node('h2', user.username), node('p', (user.disabled ? '已禁用' : '可登录') + (user.mustChangePassword ? ' · 待修改临时密码' : ''), 'muted'));
      const actions = node('div', undefined, 'user-actions');
      actions.append(button(user.disabled ? '启用账号' : '禁用账号', async () => {
        if (!user.disabled && !confirm('禁用“' + user.username + '”并撤销其所有登录？')) return;
        await api('admin/users/' + user.accountId + '/' + (user.disabled ? 'enable' : 'disable'), {}); await users();
      }, user.disabled ? 'quiet' : 'quiet danger'), button('删除账号', async () => {
        if (!confirm('永久删除账号“' + user.username + '”？此操作不可撤销，将删除其中转电脑登记并撤销所有网页与电脑登录；不删除电脑上的 Codex 历史，本机 CLI 将继续运行。')) return;
        const started = epoch, currentPage = pageVersion;
        await api('admin/users/' + user.accountId, {}, 'DELETE');
        if (started !== epoch || currentPage !== pageVersion) return;
        await users();
        if (started === epoch && currentPage === pageVersion) message('账号已删除，所有网页与电脑接入已撤销。本机 Codex 历史未删除。');
      }, 'quiet danger'));
      heading.append(info, actions);
      const reset = node('form', undefined, 'reset-form'), label = node('label', '新临时密码'), input = node('input'), submit = node('button', '重置密码并退出所有登录');
      input.type = 'password'; input.autocomplete = 'new-password'; input.minLength = 15; input.maxLength = 128; input.required = true;
      label.append(input); reset.append(label, submit);
      reset.onsubmit = async event => {
        event.preventDefault(); if (submit.disabled) return; submit.disabled = true;
        try { await api('admin/users/' + user.accountId + '/password', { temporaryPassword: input.value }); input.value = ''; message('临时密码已重置，所有网页与电脑登录已撤销。'); await users(); } catch (error) { failed(error); } finally { submit.disabled = false; }
      };
      row.append(heading, reset); return row;
    });
    if (!items.length) items.push(node('li', '尚未创建用户账号。', 'empty'));
    element('users').replaceChildren(...items);
  }
  function navigate(raw, replace = false) {
    const url = new URL(raw, location.origin);
    let path = url.origin === location.origin && Object.hasOwn(routes, url.pathname) && !url.search && !url.hash ? url.pathname : '/computers';
    if (!account) path = '/login';
    else if (account.mustChangePassword) path = '/account/security';
    else if (path === '/login') path = pendingCode ? '/authorize' : '/computers';
    else if (path === '/admin/users' && account.role !== 'admin') path = '/computers';
    stopPolling(); pageVersion++; pendingList = null;
    history[replace ? 'replaceState' : 'pushState'](null, '', path);
    element('loading').hidden = true;
    element('signed-in').hidden = !account; element('identity').hidden = !account;
    element('navigation').hidden = !account || account.mustChangePassword;
    element('account').textContent = account?.username || '';
    element('admin-link').hidden = !account || account.role !== 'admin' || account.mustChangePassword;
    element('forced-change').hidden = !account?.mustChangePassword;
    for (const [route, id] of Object.entries(routes)) element(id).hidden = route !== path;
    for (const link of document.querySelectorAll('nav a')) {
      if (link.getAttribute('href') === path) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
    }
    element('gateways').replaceChildren(); element('users').replaceChildren();
    document.title = ({ '/login': '登录', '/computers': '我的电脑', '/authorize': '授权电脑', '/account/security': '账号安全', '/admin/users': '账号管理' }[path]) + ' · Codex 公网工作台';
    if (path === '/computers') void gateways();
    if (path === '/admin/users') void users().catch(failed);
  }
  async function refreshAccount(path) {
    account = await api('account');
    navigate(path, true);
  }
  form('login', async data => {
    await api('login', data); epoch++; clearPrivate();
    await refreshAccount(pendingCode ? '/authorize' : '/computers'); message('已登录。');
  });
  form('change', async data => {
    await api('password', data); signedOut();
    message('密码已修改，所有网页与电脑已退出。请使用新密码重新登录。');
  });
  form('create-user', async data => {
    await api('admin/users', data); await users(); message('账号已创建。请安全传递临时密码，首次登录必须修改密码。');
  });
  form('bind', async data => {
    data.userCode = data.userCode.trim().toUpperCase();
    if (!/^[A-F0-9]{10}$/.test(data.userCode)) throw Object.assign(new Error('invalid_code'), { status: 400 });
    rememberCode(data.userCode);
    const preview = await api('bindings/preview', data);
    if (!confirm('以“' + account.username + '”登录电脑“' + preview.name + '”？此账号可操作完整工作台，包括终端、文件和历史。')) return false;
    await api('bindings/approve', data); rememberCode(''); navigate('/computers');
    message('电脑已获授权，正在等待电脑连接。');
  });
  element('logout').onclick = async () => {
    const logout = element('logout'); logout.disabled = true;
    stopPolling(); epoch++; pendingList = null; clearPrivate();
    try { await api('logout', {}); signedOut(true); message('已退出登录。'); }
    catch (error) {
      signedOut(true);
      if (error?.status === 401 || error?.status === 403) failed(error);
      else message('退出请求未确认，当前页面已清空。请检查网络并刷新页面，再次退出以撤销服务器登录。', true);
    }
    finally { logout.disabled = false; }
  };
  element('refresh').onclick = () => void gateways();
  document.querySelectorAll('a[data-route]').forEach(link => link.addEventListener('click', event => {
    if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault(); message(''); navigate(link.getAttribute('href'));
  }));
  const onPop = () => navigate(location.pathname, true);
  const onVisibility = () => { if (document.hidden) stopPolling(); else void gateways(); };
  const onPageHide = () => { disposed = true; epoch++; stopPolling(); window.removeEventListener('popstate', onPop); document.removeEventListener('visibilitychange', onVisibility); };
  window.addEventListener('popstate', onPop);
  document.addEventListener('visibilitychange', onVisibility);
  window.addEventListener('pagehide', onPageHide, { once: true });
  window.addEventListener('pageshow', event => {
    // A cached page must re-check its cookie before showing account data again.
    if (event.persisted) location.reload();
  });
  void refreshAccount(initialPath).catch(error => {
    if (error === stale) return;
    signedOut(); if (error?.status !== 401) failed(error);
  });
})();
