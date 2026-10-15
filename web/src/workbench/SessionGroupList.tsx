import { useEffect, useRef, useState } from 'preact/hooks';
import { Icon } from './Icons';
import { ExchangePreview } from './ExchangePreview';
import { dateLabel, failure, projectName, readSessionGroup, sourceNames, type GroupMember, type SessionGroup } from './library';
type Source = { id: string; identity?: string };
export function SessionGroupList({ groups, revision, sources, onOpen, showProject = true }: { groups: SessionGroup[]; revision: string; sources: Source[]; onOpen: (member: GroupMember) => void; showProject?: boolean }) {
  return <div className="wb-library-session-groups">{groups.map(group => <GroupRow key={`${revision}:${group.groupId}`} group={group} revision={revision} sources={sources} onOpen={onOpen} showProject={showProject} />)}</div>;
}
function GroupRow({ group, revision, sources, onOpen, showProject }: { group: SessionGroup; revision: string; sources: Source[]; onOpen: (member: GroupMember) => void; showProject: boolean }) {
  const allowed = (member: GroupMember) => sources.some(source => source.id === member.sourceId && (!member.sourceIdentity || !source.identity || source.identity === member.sourceIdentity));
  const members = group.members.filter(allowed);
  const preferred = members.find(member => member.entryId === group.preferredEntryId);
  const [related, setRelated] = useState<SessionGroup>();
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | null>(null);
  const sourceSignature = sources.map(source => `${source.id}:${source.identity || ''}`).join('|');
  useEffect(() => { request.current?.abort(); setRelated(undefined); setError(''); setBusy(false); return () => request.current?.abort(); }, [sourceSignature]);
  if (!preferred) return null;
  const inspectRelated = async () => {
    request.current?.abort(); const controller = new AbortController(); request.current = controller;
    setBusy(true); setError('');
    try { const value = await readSessionGroup(group.groupId, preferred.entryId, revision, controller.signal); if (!controller.signal.aborted) setRelated(value); }
    catch (reason) { if (!controller.signal.aborted) setError(failure(reason)); }
    finally { if (!controller.signal.aborted) setBusy(false); }
  };
  return <article className="wb-library-session-group">
    <button data-entry={preferred.entryId} className="wb-library-entry" onClick={() => onOpen(preferred)}>
      <span className="wb-library-entry-icon"><Icon name={group.section === 'auxiliary' ? 'git' : 'chat'} /></span>
      <span className="wb-library-entry-text"><strong>{preferred.title}</strong>
        <ExchangePreview preview={preferred.preview} />
        {preferred.match && <small className="wb-library-hit">搜索命中：{preferred.match.text}</small>}
        {showProject && <span className="wb-library-entry-project">{group.projectId ? projectName(group.projectPath) : '未归类会话'}</span>}
      </span><time>{dateLabel(group.latestRecordedAt)}</time><Icon name="chevron" />
    </button>
    <details className="wb-library-entry-info"><summary aria-label={`记录信息 · ${preferred.title}`} title="查看来源与记录信息">⋯{members.length > 1 && <span>{members.length} 份</span>}</summary><div className="wb-library-group-sources">
      <p>{sourceNames[preferred.kind] || '历史记录'}{group.conversationKind === 'unknown' && group.section !== 'runs' && ' · 类型未确认'}</p>
      <p>项目路径：{group.projectPath || '未归类，工作目录可在记录详情中查看'}</p>
      {members.length > 1 && <details><summary>选择来源 · {members.length} 条记录</summary><p>每份来源独立阅读，不合并正文、用量或覆盖范围。</p>{members.map(member => <button key={member.entryId} onClick={() => onOpen(member)}>{sourceNames[member.kind] || '历史记录'} · {member.sourceId} · {member.title}{member.match && ' · 搜索命中'}</button>)}</details>}
      <button disabled={busy} onClick={() => void inspectRelated()}>查看全部关联来源</button>
      {busy && <span role="status">正在读取来源摘要…</span>}
      {error && <p role="alert">{error} 请重新读取列表后重试。</p>}
      {related && <details open><summary>全部关联来源 · {related.members.filter(allowed).length} 条记录</summary><p>此处不改变列表筛选或计数；选择另一来源从它自己的记录开始阅读。</p>{related.members.filter(allowed).map(member => <button key={member.entryId} onClick={() => onOpen({ ...member, match: undefined })}>{sourceNames[member.kind] || '历史记录'} · {member.sourceId} · {member.title}</button>)}</details>}
    </div></details>
  </article>;
}
