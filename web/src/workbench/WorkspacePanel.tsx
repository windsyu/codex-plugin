import { useCallback, useEffect, useState } from 'preact/hooks';
import { FileIcon, Icon } from './Icons';
import { endpoint, useWorkspaceQuery, type Directory, type FileSelection, type GitLog, type GitStatus, type QueryMeta, type SearchResult, type WorkspaceTab } from './workspace';

const labels = { files: '项目文件', search: '搜索代码', git: 'Git 变更' };
const changeLabel: Record<string, string> = { M: '修改', A: '新增', D: '删除', R: '重命名', C: '复制', U: '冲突', '?': '未跟踪', T: '类型变化' };
const changeTone: Record<string, string> = { M: 'modified', A: 'added', D: 'deleted', R: 'renamed', C: 'added', U: 'conflict', '?': 'added' };
export function ReadStamp({ data, compact = false }: { data: QueryMeta | null; compact?: boolean }) {
  if (!data) return null;
  const time = new Date(data.readAt);
  return <p className="wb-read-stamp">读取于 {Number.isNaN(time.getTime()) ? '未知时间' : time.toLocaleTimeString('zh-CN', { hour12: false })}{!compact && ` · ${data.elapsedMs} ms`}{data.truncated && <strong> · 结果已截断</strong>}</p>;
}
function QueryState({ query }: { query: { error: string; loading: boolean; data?: unknown; cancel: () => void } }) {
  return <>{query.loading && !query.data && <p className="wb-query-progress" role="status">读取中 <button onClick={query.cancel}>取消</button></p>}{query.error && <p className="wb-notice" role="status">{query.error}{!!query.data && ' 以下保留上次成功读取的结果。'}</p>}</>;
}
export function WorkspacePanel({ tab, active, projectName, selection = null, onClose, onOpen }: { tab: WorkspaceTab; active: boolean; projectName?: string | null; selection?: FileSelection | null; onClose: () => void; onOpen: (file: FileSelection) => void }) {
  return <section className="wb-workspace-panel" hidden={!active} aria-label={labels[tab]}>
    <div className="wb-panel-heading"><strong>{labels[tab]}</strong><button className="wb-icon-button" onClick={onClose} aria-label="收起项目面板" title="收起项目面板"><Icon name="close" /></button></div>
    <div className="wb-workspace-body">
      <div className="wb-workspace-view" hidden={tab !== 'files'}><Files active={active && tab === 'files'} projectName={projectName} currentPath={selection?.path} onOpen={onOpen} /></div>
      <div className="wb-workspace-search" hidden={tab !== 'search'}><Search active={active && tab === 'search'} onOpen={onOpen} /></div>
      <div className="wb-workspace-view" hidden={tab !== 'git'}><Git active={active && tab === 'git'} selection={selection} onOpen={onOpen} /></div>
    </div>
  </section>;
}
function Files({ active, projectName, currentPath, onOpen }: { active: boolean; projectName?: string | null; currentPath?: string; onOpen: (file: FileSelection) => void }) {
  const [selected, setSelected] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [refreshVersion, setRefreshVersion] = useState(0);
  const [read, setRead] = useState<{ path: string; data: QueryMeta | null } | null>(null);
  const onRead = useCallback((path: string, data: QueryMeta | null) => setRead({ path, data }), []);
  const toggle = (path: string) => {
    const next = new Set(expanded);
    if (next.has(path)) {
      for (const item of next) if (item === path || item.startsWith(`${path}/`)) next.delete(item);
      setSelected(path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '');
    } else { next.add(path); setSelected(path); }
    setExpanded(next);
  };
  return <>
    <div className="wb-explorer-toolbar">
      <span className="wb-project-name" title={projectName || '项目根目录'}><Icon name="folderOpen" />{projectName || '项目根目录'}</span>
      <button className="wb-icon-button" aria-label="刷新文件" title="刷新当前目录" onClick={() => setRefreshVersion(v => v + 1)}><Icon name="refresh" /></button>
      <button className="wb-icon-button" aria-label="收起目录" title="收起所有目录" onClick={() => { setExpanded(new Set()); setSelected(''); }}><Icon name="collapse" /></button>
    </div>
    <div className="wb-workspace-scroll wb-file-tree" role="tree" aria-label="项目文件树"><DirectoryBranch path="" level={0} active={active} selected={selected} currentPath={currentPath} expanded={expanded} refreshVersion={refreshVersion} select={setSelected} toggle={toggle} onRead={onRead} onOpen={onOpen} /></div>
    <div className="wb-workspace-footnote">
      <span className="wb-directory-scope" title={selected || '项目根目录'}>当前读取：{selected || '项目根目录'}</span>
      <ReadStamp compact data={read?.path === selected ? read.data : null} />
      <details><summary>只读浏览 · 阅读说明</summary><p>凭证文件已隐藏，链接不可打开。其他已展开目录保留上次结果；展开目录或点击刷新可重新读取。</p></details>
    </div>
  </>;
}
function DirectoryBranch({ path, level, active, selected, currentPath, expanded, refreshVersion, select, toggle, onRead, onOpen }: {
  path: string; level: number; active: boolean; selected: string; currentPath?: string; expanded: Set<string>; refreshVersion: number; select: (path: string) => void; toggle: (path: string) => void; onRead: (path: string, data: QueryMeta | null) => void; onOpen: (file: FileSelection) => void;
}) {
  const [cursor, setCursor] = useState<string>();
  const query = useWorkspaceQuery<Directory>(endpoint('files', { path, cursor }), active && selected === path, !cursor, refreshVersion);
  useEffect(() => { if (selected === path) onRead(path, query.data); }, [path, selected, query.data, onRead]);
  return <div className={level ? 'wb-tree-branch' : undefined} style={{ '--wb-guide-left': `${Math.min(level - 1, 12) * 14 + 14}px` }} role={level ? 'group' : undefined}>
    <QueryState query={query} />
    {query.data?.entries.map(entry => <div key={entry.path}>
      <button role="treeitem" aria-level={level + 1} aria-selected={entry.path === currentPath} aria-expanded={entry.kind === 'directory' ? expanded.has(entry.path) : undefined} className="wb-file-row" style={{ paddingLeft: `${Math.min(level, 12) * 14 + 8}px` }} disabled={entry.kind === 'unavailable'} title={entry.kind === 'unavailable' ? `${entry.path} · 链接或不可读取的文件` : entry.path} onClick={() => entry.kind === 'directory' ? toggle(entry.path) : onOpen({ path: entry.path })}>
        <span className="wb-tree-chevron">{entry.kind === 'directory' && <Icon name="chevron" />}</span>
        {entry.kind === 'directory' ? <Icon name={expanded.has(entry.path) ? 'folderOpen' : 'folder'} className="wb-folder-icon" /> : <FileIcon name={entry.name} unavailable={entry.kind === 'unavailable'} />}
        <span className="wb-tree-name">{entry.name}</span>
      </button>
      {entry.kind === 'directory' && expanded.has(entry.path) && <DirectoryBranch path={entry.path} level={level + 1} active={active} selected={selected} currentPath={currentPath} expanded={expanded} refreshVersion={refreshVersion} select={select} toggle={toggle} onRead={onRead} onOpen={onOpen} />}
    </div>)}
    {query.data?.entries.length === 0 && <p className="wb-subtle">没有可阅读的文件。</p>}
    {cursor && <button onClick={() => { select(path); setCursor(undefined); }}>回到第一页</button>}
    {query.data?.nextCursor && <button onClick={() => { select(path); setCursor(query.data!.nextCursor!); }}>下一页文件</button>}
  </div>;
}
function Search({ active, onOpen }: { active: boolean; onOpen: (file: FileSelection) => void }) {
  const [text, setText] = useState(''); const [regex, setRegex] = useState(false); const [sensitive, setSensitive] = useState(false);
  const [url, setUrl] = useState<string | null>(null);
  const query = useWorkspaceQuery<SearchResult>(url, active);
  const groups = new Map<string, SearchResult['hits']>();
  query.data?.hits.forEach(hit => { const group = groups.get(hit.path) || []; group.push(hit); groups.set(hit.path, group); });
  return <>
    <form className="wb-search-form" onSubmit={e => { e.preventDefault(); setUrl(endpoint('search', { q: text, regex: String(regex), caseSensitive: String(sensitive) })); query.refresh(); }}>
      <label>搜索当前项目<input aria-label="搜索内容" value={text} onInput={e => setText(e.currentTarget.value)} placeholder="文字或代码…" maxLength={4096} /></label>
      <div className="wb-search-options"><label><input type="checkbox" checked={regex} onChange={e => setRegex(e.currentTarget.checked)} />正则</label><label><input type="checkbox" checked={sensitive} onChange={e => setSensitive(e.currentTarget.checked)} />区分大小写</label></div>
      <button disabled={!text.trim() || query.loading}>搜索</button>
    </form>
    <QueryState query={query} />
    {query.data && <p className="wb-subtle">{query.data.hits.length} 处匹配 · 检查了 {query.data.scannedFiles} 个文件</p>}
    {[...groups].map(([path, hits]) => <section className="wb-search-group" key={path}><strong>{path}</strong>{hits.map((hit, i) => <button key={i} className="wb-search-hit" onClick={() => onOpen({ path, line: hit.line })}><span>{hit.line}</span><code>{hit.text}{hit.truncated ? '…' : ''}</code></button>)}</section>)}
    {query.data?.truncated && <p className="wb-notice">已到达{query.data.limitReason === 'time_limit' ? ' 5 秒时限' : '搜索上限'}，请缩小搜索内容。</p>}
    <ReadStamp data={query.data} /><p className="wb-subtle">遵守项目忽略规则；跳过凭证、链接、二进制与超过 1 MiB 的文件。结果最多 200 处。</p>
  </>;
}
function Git({ active, selection, onOpen }: { active: boolean; selection: FileSelection | null; onOpen: (file: FileSelection) => void }) {
  const [section, setSection] = useState<'changes' | 'log'>('changes'); const [cursor, setCursor] = useState<string>();
  const [changePages, setChangePages] = useState({ staged: 0, working: 0 });
  const status = useWorkspaceQuery<GitStatus>(endpoint('git/status'), active && section === 'changes', true);
  const log = useWorkspaceQuery<GitLog>(endpoint('git/log', { cursor }), active && section === 'log', !cursor);
  const query = section === 'changes' ? status : log;
  return <>
    <div className="wb-git-toolbar"><div className="wb-git-tabs" aria-label="Git 阅读范围"><button aria-pressed={section === 'changes'} onClick={() => setSection('changes')}>变更</button><button aria-pressed={section === 'log'} onClick={() => setSection('log')}>提交记录</button></div><button className="wb-icon-button" aria-label="刷新 Git" title="刷新 Git" onClick={() => { setCursor(undefined); query.refresh(); }}><Icon name="refresh" /></button></div>
    {status.data && <div className="wb-git-branch"><Icon name="git" /><span title={status.data.branch || '分离的 HEAD'}>{status.data.branch || '分离的 HEAD'}</span><small>当前分支</small></div>}
    <div className="wb-workspace-scroll wb-git-content">
      <QueryState query={query} />
      {section === 'changes' ? <>
      {status.data && (['staged', 'working'] as const).map(scope => {
        const entries = status.data?.entries.filter(entry => scope === 'staged' ? ![' ', '?'].includes(entry.index) : entry.working !== ' ') || [];
        const page = Math.min(changePages[scope], Math.max(0, Math.ceil(entries.length / 200) - 1));
        return <section className="wb-git-group" key={scope}>
          <h3>{scope === 'staged' ? '已暂存' : '未暂存与未跟踪'}<span className="wb-count">{entries.length}</span></h3>
          {entries.slice(page * 200, (page + 1) * 200).map(entry => {
            const slash = entry.path.lastIndexOf('/');
            const name = entry.path.slice(slash + 1), directory = slash < 0 ? '项目根目录' : entry.path.slice(0, slash);
            const change = scope === 'staged' ? entry.index : entry.working;
            const selected = selection?.path === entry.path && selection.scope === scope;
            const label = changeLabel[change] || '变化';
            return <div className={`wb-git-row ${selected ? 'selected' : ''}`} key={entry.path}>
              <button aria-current={selected ? 'true' : undefined} aria-label={`查看${scope === 'staged' ? '已暂存' : '未暂存'}差异 ${entry.path}（${label}）`} title={`${entry.path}${entry.originalPath ? `\n原路径：${entry.originalPath}` : ''}`} onClick={() => onOpen({ path: entry.path, scope })}>
                <FileIcon name={name} /><span className="wb-git-path"><span className="wb-git-name">{name}</span><small>{directory}</small>{entry.originalPath && <small className="wb-git-original">原路径：{entry.originalPath}</small>}</span><span className={`wb-git-mark is-${changeTone[change] || 'unknown'}`}>{label}</span>
              </button>
              <button className="wb-icon-button wb-git-open" title="查看当前文件" aria-label={`打开文件 ${entry.path}`} onClick={() => onOpen({ path: entry.path })}><Icon name="open" /></button>
            </div>;
          })}
          {status.data && !entries.length && <p className="wb-group-empty">{scope === 'staged' ? '暂无已暂存的文件' : '工作区没有未暂存的变更'}</p>}
          {entries.length > 200 && <div className="wb-workspace-actions"><button disabled={!page} onClick={() => setChangePages({ ...changePages, [scope]: page - 1 })}>上一页变更</button><span>{page + 1} / {Math.ceil(entries.length / 200)}</span><button disabled={(page + 1) * 200 >= entries.length} onClick={() => setChangePages({ ...changePages, [scope]: page + 1 })}>下一页变更</button></div>}
        </section>;
      })}
      {!!status.data?.omitted && <p className="wb-subtle">有不可阅读的路径已省略。</p>}
    </> : <>
      {log.data?.commits.map(commit => <article className="wb-git-commit" key={commit.id}><strong>{commit.subject}</strong><p><code>{commit.shortId}</code><span title={commit.author}>{commit.author}</span></p><time dateTime={commit.date}>{new Date(commit.date).toLocaleString('zh-CN')}</time></article>)}
      {log.data?.commits.length === 0 && <p className="wb-subtle">当前目录尚无提交记录。</p>}
      {cursor && <button onClick={() => setCursor(undefined)}>回到第一页</button>}{log.data?.nextCursor && <button onClick={() => setCursor(log.data!.nextCursor!)}>更早提交</button>}
    </>}
    </div>
    <div className="wb-workspace-footnote"><ReadStamp compact data={query.data} /><details><summary>只读浏览 · 阅读说明</summary><p>仅显示当前项目目录。点击变更查看差异；行末按钮打开当前文件。结果是读取时的状态，与单次工具修改记录可能不同。</p></details></div>
  </>;
}
