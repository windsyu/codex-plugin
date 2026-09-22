import { endpoint, useWorkspaceQuery, type FileContent, type FileSelection } from './workspace';
import { ReadStamp } from './WorkspacePanel';
import { FileDocument } from './FileDocument';

export function FileReading({ selection, onOpen, onClose, blocked }: { selection: FileSelection; onOpen: (file: FileSelection) => void; onClose: () => void; blocked: boolean }) {
  const { path, scope } = selection;
  const query = useWorkspaceQuery<FileContent>(endpoint(scope ? 'git/diff' : 'file', { path, scope }), true, true);
  return <section className="wb-file-reading" aria-label="文件阅读" inert={blocked}>
    <div className="wb-file-title"><code>{path}</code><button onClick={onClose} aria-label="关闭文件阅读">×</button></div>
    <div className="wb-file-toolbar"><div className="wb-workspace-actions"><button aria-pressed={!scope} onClick={() => onOpen({ path })}>当前文件</button><button aria-pressed={scope === 'working'} onClick={() => onOpen({ path, scope: 'working' })}>未暂存</button><button aria-pressed={scope === 'staged'} onClick={() => onOpen({ path, scope: 'staged' })}>已暂存</button><button onClick={query.refresh}>重新读取</button></div>
      <ReadStamp data={query.data} />
    </div>
    {query.loading && !query.data && <p className="wb-query-progress" role="status">读取中 <button onClick={query.cancel}>取消</button></p>}
    {query.error && <p className="wb-notice" role="status">{query.error}{query.data && ' 以下保留上次成功读取的内容。'}</p>}
    {query.data?.truncated && <p className="wb-notice">内容已截断，仅显示捕获到的部分。</p>}
    {scope && <p className="wb-file-explanation">{scope === 'staged' ? 'HEAD → 暂存区' : '暂存区 → 当前文件'} · 文件内容 Diff。权限与重命名请看左侧 Git 状态；这是当前工作区，不是工具调用时的快照。</p>}
    {scope && query.data?.empty && <p className="wb-file-explanation">两份文件内容相同。</p>}
    {query.data && <FileDocument key={`${path}:${scope || 'file'}`} content={{ text: (scope ? query.data.patch : query.data.text) || '', revision: query.data.revision, targetLine: selection.line }} diff={!!scope} onOpenLine={line => onOpen({ path, line })} />}
  </section>;
}
