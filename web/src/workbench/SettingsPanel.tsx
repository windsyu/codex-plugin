import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { HistoryStorage, CleanupPreviewPanel } from './HistoryStorage';

export interface WorkbenchConfig {
  schemaVersion: 1;
  access?: { port: number };
  launch: { openBrowser: boolean; codexBin: string; profile: string | null; providerProfile: string };
  storage: { dataDir: string | null };
  history: { cleanup: { enabled: boolean; retention: { enabled: boolean; days: number } } };
}
export interface Settings {
  schemaVersion: number; revision: string | null; saved: WorkbenchConfig | null; effective: WorkbenchConfig; defaults: WorkbenchConfig;
  cliOverrides: string[]; configPath: string; effectiveDataDir: string; restartRequired: string[];
  errors: { code: string; field: string; line?: number; column?: number }[];
  capabilities: { manualCleanup: boolean; retention: boolean };
}
const fieldNames: Record<string, string> = {
  'access.port': '手机接入端口', 'launch.openBrowser': '启动时自动打开浏览器', 'launch.codexBin': 'Codex 程序位置', 'launch.profile': 'Codex 启动配置',
  'launch.providerProfile': '模型连接方式', 'storage.dataDir': '历史保存位置', 'history.cleanup.enabled': '允许清理历史记录',
  'history.cleanup.retention.enabled': '自动清理过期记录', 'history.cleanup.retention.days': '保留天数', '$': '配置文件', 'schemaVersion': '配置版本'
};
function errorText(code: string, field?: string) {
  const messages: Record<string, string> = {
    config_changed: '配置文件已被修改。请重新加载后再保存，当前草稿已保留。',
    config_busy: '配置正在处理，请稍后重试。', config_io_error: '无法读写配置文件，请检查文件权限和磁盘。',
    config_save_unconfirmed: '尚未确认保存结果，请重新加载核对文件；不会自动重发保存。',
    stale_run: '工作台已重新启动，请刷新页面。', cleanup_unavailable: '历史清理暂不可用，当前保留全部记录。',
    unsupported_config_version: '暂不支持此配置版本，请检查配置文件。',
    executable_unavailable: '请填写可用的已安装 Codex CLI 绝对路径，或 codex。',
    unsafe_data_directory: '请选择独立的历史目录，不能与配置、原生会话或项目根目录重叠。',
    invalid_field: '填写的内容不符合要求，请检查后再保存。', invalid_config: '配置格式或字段不符合要求，请检查 JSON 文件。',
    config_too_large: '配置文件超过大小限制。'
  };
  return `${field && field !== '$' ? `${fieldNames[field] || '配置字段'}：` : ''}${messages[code] || '配置操作未成功，请重新加载后重试。'}`;
}
function configShape(value: unknown): value is WorkbenchConfig {
  const c = value as WorkbenchConfig | null;
  return c?.schemaVersion === 1 && typeof c.launch?.openBrowser === 'boolean' && typeof c.launch?.codexBin === 'string'
    && (c.launch.profile === null || typeof c.launch.profile === 'string') && typeof c.launch.providerProfile === 'string'
    && !!c.storage && (c.storage.dataDir === null || typeof c.storage.dataDir === 'string')
    && typeof c.history?.cleanup?.enabled === 'boolean' && typeof c.history.cleanup.retention?.enabled === 'boolean'
    && Number.isInteger(c.history.cleanup.retention.days);
}
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
class SettingsError extends Error {
  constructor(readonly code: string, field?: string) { super(errorText(code, field)); }
}

export function SettingsPanel({ epoch, open, managementAvailable = false, onClose, onDirtyChange, onViewHistory }: {
  epoch: string; open: boolean; managementAvailable?: boolean; onClose: (restoreFocus: boolean) => void; onDirtyChange: (dirty: boolean) => void; onViewHistory?: () => void;
}) {
  const [previewDays, setPreviewDays] = useState<number | null>(null);
  const [info, setInfo] = useState<Settings | null>(null);
  const [draft, setDraft] = useState<WorkbenchConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [stale, setStale] = useState(false);
  const [message, setMessage] = useState('');
  const section = useRef<HTMLElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const current = useRef({ info, draft, saving }); current.current = { info, draft, saving };
  const closeAction = useRef(onClose); closeAction.current = onClose;
  const request = useRef(0);
  const readController = useRef<AbortController | null>(null);
  const alive = useRef(true);
  const dirty = !!draft && !same(draft, info?.saved);
  useEffect(() => { onDirtyChange(dirty); }, [dirty, onDirtyChange]);
  useEffect(() => () => { alive.current = false; readController.current?.abort(); request.current++; }, []);
  const payload = useCallback(async (response: Response) => {
    if (!response.ok) {
      const data = await response.json().catch(() => null);
      const reason = data?.error;
      throw new SettingsError(reason?.code || '', reason?.field);
    }
    const body = await response.json();
    const value = body.settings as Settings;
    if (body.currentRunEpoch !== epoch || value?.schemaVersion !== 1 || !configShape(value.effective) || !configShape(value.defaults)
      || (value.saved !== null && !configShape(value.saved)) || !Array.isArray(value.errors) || !Array.isArray(value.cliOverrides)
      || !Array.isArray(value.restartRequired) || typeof value.capabilities?.manualCleanup !== 'boolean'
      || typeof value.capabilities.retention !== 'boolean' || typeof value.configPath !== 'string' || typeof value.effectiveDataDir !== 'string'
      || (value.revision !== null && !/^[a-f0-9]{64}$/.test(value.revision))) throw new Error('配置响应无法识别，请刷新页面。');
    return value;
  }, [epoch]);
  const load = useCallback(async (replaceDraft = false) => {
    if (current.current.saving) return;
    const sequence = ++request.current;
    readController.current?.abort();
    const controller = new AbortController(); readController.current = controller;
    setLoading(true);
    try {
      const value = await payload(await fetch('/workbench/v1/settings', { credentials: 'same-origin', signal: controller.signal }));
      if (!alive.current || controller.signal.aborted || sequence !== request.current) return;
      const previous = current.current;
      const edited = previous.draft && !same(previous.draft, previous.info?.saved);
      if (edited && !replaceDraft) {
        if (previous.info?.revision !== value.revision || value.errors.length) {
          setStale(true); setMessage('配置文件已变化或无法读取，当前草稿已保留。重新加载会放弃草稿。');
        }
      } else {
        setInfo(value); setDraft(value.saved); setStale(false); setMessage('');
      }
    } catch (error) {
      if (!controller.signal.aborted && alive.current && sequence === request.current) setMessage((error as Error).message);
    } finally {
      if (alive.current && sequence === request.current) setLoading(false);
    }
  }, [payload]);
  useLayoutEffect(() => { if (open) closeButton.current?.focus({ preventScroll: true }); }, [open]);
  useEffect(() => {
    if (!open) { readController.current?.abort(); return; }
    void load();
    const timer = window.setInterval(() => void load(), 5000);
    const outside = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && !section.current?.contains(target) && !document.getElementById('wb-settings-button')?.contains(target)) closeAction.current(false);
    };
    document.addEventListener('pointerdown', outside);
    return () => { window.clearInterval(timer); readController.current?.abort(); document.removeEventListener('pointerdown', outside); };
  }, [open, load]);
  function edit(change: (value: WorkbenchConfig) => void) {
    if (!draft) return;
    const next = structuredClone(draft); change(next); setDraft(next); setMessage('');
  }
  async function save() {
    if (!draft || !info?.revision || stale || saving) return;
    readController.current?.abort(); request.current++;
    current.current.saving = true; setSaving(true); setMessage('');
    try {
      const value = await payload(await fetch('/workbench/v1/settings', {
        method: 'PUT', credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'If-Match': `"${info.revision}"` },
        body: JSON.stringify({ currentRunEpoch: epoch, config: draft })
      }));
      if (alive.current) { setInfo(value); setDraft(value.saved); setMessage('已保存'); setStale(false); }
    } catch (error) {
      if (alive.current) { setMessage((error as Error).message); setStale(error instanceof SettingsError && ['config_changed', 'config_save_unconfirmed', 'config_io_error', 'stale_run'].includes(error.code)); }
    } finally {
      current.current.saving = false;
      if (alive.current) setSaving(false);
    }
  }
  const label = (name: string) => <span>{fieldNames[name]}{info?.cliOverrides.includes(name) && <small className="wb-settings-override">本次由启动参数覆盖</small>}</span>;
  return <section id="wb-settings-panel" ref={section} className="wb-settings-panel" aria-label="工作台设置" hidden={!open} onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(true); }
  }}>
    <div className="wb-panel-heading"><strong>工作台设置</strong><button ref={closeButton} type="button" onClick={() => onClose(true)}>收起</button></div>
    <div className="wb-settings-body">
      {!info && <p role="status">{loading ? '正在读取配置…' : '配置暂时不可用。'}</p>}
      {info && <>
        <p className="wb-subtle">使用同一套设置的项目共享这些偏好，每个工作台只清理当前项目。</p>
        {info.errors.map(error => <p className="wb-notice" role="alert" key={`${error.field}:${error.code}`}>{errorText(error.code, error.field)}{error.line != null ? `（第 ${error.line} 行）` : ''} 请修复文件后重新加载。</p>)}
        {!!info.errors.length && <p className="wb-subtle">需要修复的配置文件：<code>{info.configPath}</code></p>}
      </>}
      {draft && info && <form id="wb-settings-form" onSubmit={event => { event.preventDefault(); void save(); }}>
        <fieldset disabled={saving}><legend>历史清理</legend>
          <label className="wb-settings-toggle">允许清理历史记录<input aria-label="允许清理历史记录" aria-describedby="wb-cleanup-help" type="checkbox" checked={draft.history.cleanup.enabled} disabled={!info.capabilities.manualCleanup} onChange={event => edit(c => { c.history.cleanup.enabled = event.currentTarget.checked; if (!c.history.cleanup.enabled) c.history.cleanup.retention.enabled = false; })} /></label>
          <p id="wb-cleanup-help" className="wb-settings-help">关闭时保留全部记录。开启并保存后，可在历史列表中删除单条记录，或批量选择后确认删除。</p>
          <label className="wb-settings-toggle">自动清理过期记录<input aria-label="自动清理过期记录" aria-describedby="wb-retention-help" type="checkbox" disabled={!info.capabilities.retention || !draft.history.cleanup.enabled} checked={draft.history.cleanup.retention.enabled} onChange={event => edit(c => { c.history.cleanup.retention.enabled = event.currentTarget.checked; })} /></label>
          <p id="wb-retention-help" className="wb-settings-help">{!draft.history.cleanup.enabled && '需先允许清理历史记录。'}开启后，超过保留天数的记录会自动删除，无需逐次确认。</p>
          <label>保留天数<input aria-label="保留天数" aria-describedby="wb-retention-days-help" type="number" min={1} max={3650} step={1} disabled={!draft.history.cleanup.retention.enabled || !info.capabilities.retention} value={draft.history.cleanup.retention.days} onInput={event => edit(c => { c.history.cleanup.retention.days = Number(event.currentTarget.value); })} /></label>
          <p id="wb-retention-days-help" className="wb-settings-help">按每次运行结束的时间计算，可设置 1–3650 天。</p>
          <p className="wb-subtle">{!info.capabilities.manualCleanup ? '历史清理暂不可用，当前保留全部记录。' : '正在运行的记录和 Codex 原生会话会保留。'}</p>
          {draft.history.cleanup.retention.enabled && <p className="wb-notice">保存后会检查已有到期记录，之后在工作台运行期间每小时检查一次。无法确认结束时间或未正常结束的记录会保留。</p>}
          {managementAvailable && <button type="button" onClick={() => setPreviewDays(draft.history.cleanup.retention.days)}>预览到期记录</button>}
          {previewDays != null && <CleanupPreviewPanel epoch={epoch} retentionDays={previewDays} onClose={() => setPreviewDays(null)} />}
        </fieldset>
        <fieldset disabled={saving}><legend>历史记录与日志</legend>
          <dl className="wb-settings-locations">
            <dt>历史记录文件夹</dt><dd><code>{info.effectiveDataDir.replace(/\/$/, '')}/runs</code></dd>
          </dl>
          <p className="wb-settings-help">工作台历史按每次运行分文件夹保存。</p>
          <details className="wb-settings-paths"><summary>本次运行日志的位置</summary><p><code>{info.effectiveDataDir.replace(/\/$/, '')}/runs/{epoch}</code></p><p className="wb-subtle">本次运行已保存的对话和日志在此文件夹中，日志会分段保存。</p></details>
          {managementAvailable && <><HistoryStorage key={info.revision || 'invalid'} epoch={epoch} active={open} />{onViewHistory && <button type="button" onClick={onViewHistory}>查看历史</button>}</>}
        </fieldset>
        <fieldset disabled={saving}><legend>启动偏好 <small>下次启动生效</small></legend>
          <label className="wb-settings-toggle">{label('launch.openBrowser')}<input type="checkbox" aria-label="启动时自动打开浏览器" checked={draft.launch.openBrowser} onChange={event => edit(c => { c.launch.openBrowser = event.currentTarget.checked; })} /></label>
          {draft.launch.openBrowser !== info.effective.launch.openBrowser && <p className="wb-subtle">本次启动：{info.effective.launch.openBrowser ? '自动打开浏览器' : '不自动打开浏览器'}。</p>}
          <details className="wb-settings-paths"><summary>高级启动设置</summary>
            <p className="wb-subtle">通常无需修改。模型、登录和权限仍在右侧 Codex 终端中设置。</p>
            <label>{label('launch.codexBin')}<input aria-label="Codex 程序位置" value={draft.launch.codexBin} onInput={event => edit(c => { c.launch.codexBin = event.currentTarget.value; })} required placeholder="codex 或已安装程序的完整路径" /></label>
            <p className="wb-settings-help">用于选择已安装的 Codex 程序。本次使用：{info.effective.launch.codexBin}。</p>
            <label>{label('launch.profile')}<input aria-label="Codex 启动配置" value={draft.launch.profile || ''} maxLength={128} pattern={'[A-Za-z0-9_\\-]+'} placeholder="留空使用默认配置" onInput={event => edit(c => { c.launch.profile = event.currentTarget.value || null; })} /></label>
            <p className="wb-settings-help">填写已在 Codex 中创建的配置名称，留空使用默认配置。本次使用：{info.effective.launch.profile || '默认配置'}。</p>
            <label>{label('storage.dataDir')}<input aria-label="历史保存位置" value={draft.storage.dataDir || ''} placeholder="留空使用默认位置" onInput={event => edit(c => { c.storage.dataDir = event.currentTarget.value || null; })} /></label>
            <p className="wb-settings-help">下次启动后，新记录将保存在所填位置的 runs 文件夹中。留空使用默认位置；已有记录不会搬迁。</p>
          <label>手机接入端口<input aria-label="手机接入端口" type="number" min={0} max={65535} step={1} value={draft.access?.port ?? 0} onInput={event => edit(c => { c.access = { port: Number(event.currentTarget.value) }; })} /></label><p className="wb-settings-help">0 自动选择，或填写 1024–65535。保存后下次开启手机接入生效，不会自动开放网络。</p></details>
        </fieldset>
        {!!info.restartRequired.length && <p className="wb-notice">已保存，待下次启动生效：{info.restartRequired.map(field => fieldNames[field] || field).join('、')}。</p>}
      </form>}
      <button type="button" disabled={saving || loading} onClick={() => void load(true)}>{dirty ? '重新加载（放弃草稿）' : '重新加载'}</button>
    </div>
    {(message || (draft && info)) && <div className="wb-settings-footer">
      {message && <p className="wb-notice" role="status">{message}</p>}
      {draft && info && <div className="wb-settings-actions"><button type="submit" form="wb-settings-form" disabled={saving || !dirty || stale || !info.revision}>{saving ? '正在保存…' : '保存'}</button>
        <button type="button" disabled={saving || !dirty} onClick={() => { setDraft(info.saved); setMessage(''); }}>取消修改</button>
        <button type="button" disabled={saving} onClick={() => { setDraft(structuredClone(info.defaults)); setMessage('默认值已填入，保存后才生效。'); }}>恢复默认值</button>
        <span className="wb-subtle">{dirty ? '未保存' : '已保存'}</span>
      </div>}
    </div>}
  </section>;
}
