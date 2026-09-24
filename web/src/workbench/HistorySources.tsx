import { useRef, useState } from 'preact/hooks';
export type HistorySource = { id: string; kind: 'native'; codexHome: string } | { id: string; kind: 'workbench'; dataDirectory: string } | { id: string; kind: 'observer'; database: string; blobDirectory?: string | null; nativeHome?: string | null };
export interface LibraryConfig { enabled: boolean; cacheLimitMiB: number; sources: HistorySource[] }
export const libraryDefaults = (): LibraryConfig => ({ enabled: true, cacheLimitMiB: 512, sources: [] });
const sourcePath = (source: HistorySource) => source.kind === 'native' ? source.codexHome : source.kind === 'workbench' ? source.dataDirectory : source.database;
export function HistorySources({ value, onChange }: { value: LibraryConfig; onChange: (value: LibraryConfig) => void }) {
  const [adding, setAdding] = useState(false);
  const [kind, setKind] = useState<'observer' | 'native' | 'workbench'>('observer');
  const [path, setPath] = useState('');
  const [configPath, setConfigPath] = useState('');
  const [preview, setPreview] = useState<HistorySource | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const previewSequence = useRef(0);
  function add(source: HistorySource) {
    if (value.sources.length >= 8 || value.sources.some(item => item.id === source.id || (item.kind === source.kind && sourcePath(item) === sourcePath(source)))) { setError('该来源已经接入，或已达到 8 个额外来源的上限。'); return; }
    onChange({ ...value, sources: [...value.sources, source] }); setAdding(false); setPreview(null); setPath(''); setConfigPath(''); setError('');
  }
  async function loadPreview() {
    const sequence = ++previewSequence.current; setBusy(true); setError(''); setPreview(null);
    try {
      const response = await fetch('/workbench/v1/library/preview', { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ path: configPath }) });
      const body = await response.json();
      if (!response.ok || body.source?.kind !== 'observer' || typeof body.source.database !== 'string') throw new Error('无法读取这份旧配置中的历史路径。请检查完整路径，或直接填写历史数据库位置。');
      if (sequence === previewSequence.current) setPreview(body.source);
    } catch (e) { if (sequence === previewSequence.current) setError((e as Error).message); }
    finally { if (sequence === previewSequence.current) setBusy(false); }
  }
  return <fieldset><legend>全部历史的来源</legend>
    <label className="wb-settings-toggle">汇总已接入的历史<input type="checkbox" aria-label="汇总已接入的历史" checked={value.enabled} onChange={event => onChange({ ...value, enabled: event.currentTarget.checked })} /></label>
    <p className="wb-settings-help">自动读取本机 Codex 原生会话和当前工作台记录。关闭后暂停汇总，原始记录保留。</p>
    <ul className="wb-source-drafts">{value.sources.map(source => <li key={source.id}><div><strong>{{ native: '原生会话', workbench: '工作台记录', observer: '旧版历史' }[source.kind]}</strong><code>{sourcePath(source)}</code></div><button type="button" onClick={() => onChange({ ...value, sources: value.sources.filter(item => item.id !== source.id) })}>移除来源</button></li>)}</ul>
    {!!value.sources.length && <p className="wb-settings-help">移除并保存后，不再读取该来源；不会删除原始历史。</p>}
    {!adding ? <button type="button" disabled={value.sources.length >= 8} onClick={() => setAdding(true)}>接入旧历史</button> : <div className="wb-source-add">
      <label>历史类型<select aria-label="历史类型" value={kind} onChange={event => { setKind(event.currentTarget.value as typeof kind); setPreview(null); previewSequence.current++; setBusy(false); }}><option value="observer">旧版历史数据库</option><option value="native">其它 Codex 原生目录</option><option value="workbench">其它工作台历史目录</option></select></label>
      <label>历史文件位置<input aria-label="历史文件位置" value={path} placeholder={kind === 'observer' ? '历史数据库的完整路径' : '历史目录的完整路径'} onInput={event => setPath(event.currentTarget.value)} /></label>
      <button type="button" disabled={!path.startsWith('/')} onClick={() => { const id = `source-${crypto.randomUUID()}`; add(kind === 'observer' ? { id, kind, database: path } : kind === 'native' ? { id, kind, codexHome: path } : { id, kind, dataDirectory: path }); }}>加入待保存来源</button>
      {kind === 'observer' && <details><summary>从旧版配置中读取位置</summary><p className="wb-settings-help">仅提取历史路径，预览确认后加入；不导入登录、网络或其它设置。</p><label>旧版配置文件<input aria-label="旧版配置文件" value={configPath} onInput={event => { setConfigPath(event.currentTarget.value); setPreview(null); previewSequence.current++; setBusy(false); }} placeholder="observer.toml 的完整路径" /></label><button type="button" disabled={busy || !configPath.startsWith('/')} onClick={() => void loadPreview()}>{busy ? '读取中…' : '读取位置'}</button>
        {preview && <div className="wb-source-preview"><strong>将接入的历史数据库</strong><code>{sourcePath(preview)}</code>{preview.kind === 'observer' && preview.blobDirectory && <p>附件：<code>{preview.blobDirectory}</code></p>}<button type="button" onClick={() => add(preview)}>确认加入待保存来源</button></div>}</details>}
      <button type="button" onClick={() => { setAdding(false); setPreview(null); previewSequence.current++; setBusy(false); }}>取消接入</button>
    </div>}
    {error && <p className="wb-notice" role="alert">{error}</p>}
    <details className="wb-settings-paths"><summary>历史搜索缓存</summary><label>缓存空间上限（MiB）<input aria-label="缓存空间上限（MiB）" type="number" min={64} max={2048} step={1} value={value.cacheLimitMiB} onInput={event => onChange({ ...value, cacheLimitMiB: Number(event.currentTarget.value) })} /></label><p className="wb-settings-help">用于加快历史查询，可随时重建；达到上限时明确提示搜索覆盖不完整，原始历史不变。</p></details>
    <p className="wb-subtle">来源变更在点击下方“保存”后生效。全部历史只供阅读，清理仍在对应项目工作台进行。</p>
  </fieldset>;
}
