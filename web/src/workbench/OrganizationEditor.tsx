import { useEffect, useRef, useState } from 'preact/hooks';
import { failure, libraryRead, projectName, readOrganization, saveOrganization, unassignedReasonText, type LibraryEntry, type LibraryPage, type OrganizationAction, type OrganizationResponse, type Project } from './library';
export function OrganizationEditor({ entry, instanceId, onClose, onSaved }: { entry: LibraryEntry; instanceId: string; onClose: () => void; onSaved: (result: OrganizationResponse) => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const request = useRef<AbortController | null>(null);
  const [result, setResult] = useState<OrganizationResponse>();
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  const [action, setAction] = useState<OrganizationAction>('project');
  const [target, setTarget] = useState<Project>();
  const [confirmed, setConfirmed] = useState(false);
  const [projectQ, setProjectQ] = useState('');
  const [search, setSearch] = useState('');
  const [projectPage, setProjectPage] = useState<LibraryPage<Project>>();
  const [projectCursor, setProjectCursor] = useState<string>();
  const [projectError, setProjectError] = useState('');
  const [loadingProjects, setLoadingProjects] = useState(false);
  const [projectReload, setProjectReload] = useState(0);
  const canAdjust = !!result?.canAdjust && !!entry.sourceIdentity && result.organization.overrideState !== 'unavailable';
  const close = () => { request.current?.abort(); dialog.current?.close?.(); onClose(); };
  const load = async (afterUncertainSave = false) => {
    request.current?.abort(); const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(''); setConfirmed(false);
    try {
      const value = await readOrganization(entry.entryId, controller.signal);
      if (controller.signal.aborted) return;
      setResult(value); setProjectCursor(undefined);
      if (afterUncertainSave) { setMessage('已重新核对当前归类，请检查结果；没有自动重试保存。'); onSaved(value); }
    } catch (reason) { if (!controller.signal.aborted) { setResult(undefined); setError(`用户归类暂不可用。${failure(reason)}`); } }
    finally { if (!controller.signal.aborted) setBusy(false); }
  };
  useEffect(() => {
    const element = dialog.current;
    if (typeof element?.showModal === 'function') element.showModal(); else element?.setAttribute('open', '');
    element?.querySelector<HTMLButtonElement>('button')?.focus();
    void load();
    return () => { request.current?.abort(); };
  }, [entry.entryId, entry.sourceIdentity, entry.sourceRevision]);
  useEffect(() => {
    if (!canAdjust || action !== 'project') return;
    const controller = new AbortController(); setLoadingProjects(true); setProjectError(''); setProjectPage(undefined);
    const params = new URLSearchParams({ limit: '30' }); if (search) params.set('projectQ', search); if (projectCursor) params.set('cursor', projectCursor);
    void libraryRead<Project>('projects', params, controller.signal).then(value => { if (!controller.signal.aborted) setProjectPage(value); }).catch(reason => { if (!controller.signal.aborted) setProjectError(failure(reason)); }).finally(() => { if (!controller.signal.aborted) setLoadingProjects(false); });
    return () => controller.abort();
  }, [canAdjust, action, search, projectCursor, projectReload, result?.revision]);
  const save = async () => {
    if (!result || !canAdjust || !confirmed || busy || (action === 'project' && !target?.projectId)) return;
    request.current?.abort(); const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(''); setMessage('');
    try {
      const value = await saveOrganization(entry, instanceId, result.organizationRevision, action, target?.projectId || undefined, controller.signal);
      if (controller.signal.aborted) return;
      setResult(value); setProjectCursor(undefined); setConfirmed(false); setMessage('项目归属已保存。只影响此条记录的展示；正文和原工作目录保持不变。'); onSaved(value);
    } catch (reason) {
      if (controller.signal.aborted) return;
      setConfirmed(false); setError(`未能确认保存结果。${failure(reason)}请先重新核对当前归类，不要直接重复保存。`);
      setResult(undefined);
    } finally { if (!controller.signal.aborted) setBusy(false); }
  };
  const targetText = action === 'project' ? target?.path || '请选择已有项目' : action === 'unassigned' ? '未归类会话' : result?.organization.automatic.projectPath || `未归类会话 · ${unassignedReasonText(result?.organization.unassignedReason)}`;
  return <dialog ref={dialog} className="wb-organization-editor" aria-label="调整项目归属" onCancel={event => { event.preventDefault(); close(); }}>
    <header><h2>调整项目归属</h2><button onClick={close}>关闭</button></header>
    <p>记录：{entry.title}</p><p>仅修改此条来源记录，不自动调整父会话、子代理或其他来源副本；不删除或移动原始数据。</p>
    <p className="wb-library-note">继续此会话仍使用原工作目录：{entry.recordedCwd || '未记录'}</p>
    {busy && <p role="status">正在核对或保存归类…</p>}
    {error && <p role="alert">{error}<button disabled={busy} onClick={() => void load(true)}>重新核对当前归类</button></p>}
    {message && <p role="status">{message}</p>}
    {result && <><p>当前归属：{result.organization.effective.projectPath || '未归类会话'}</p><p>自动归属：{result.organization.automatic.projectPath || '未归类会话'} · 覆盖状态：{{ none: '使用自动规则', applied: '用户调整已生效', stale: '原调整身份已变化', unavailable: '用户归类暂不可用' }[result.organization.overrideState]}</p>{!canAdjust ? <p>当前记录不可调整：{result.adjustmentReason || (!entry.sourceIdentity ? '记录身份无法核验' : '用户归类暂不可用')}</p> : <>
      <label>调整方式<select aria-label="调整方式" disabled={busy} value={action} onChange={event => { setAction(event.currentTarget.value as OrganizationAction); setConfirmed(false); }}><option value="project">归入已有项目</option><option value="unassigned">设为未归类</option><option value="automatic">恢复自动归类</option></select></label>
      {action === 'project' && <section aria-label="选择已有项目"><form onSubmit={event => { event.preventDefault(); setSearch(projectQ.trim()); setProjectCursor(undefined); setProjectReload(value => value + 1); setTarget(undefined); setConfirmed(false); }}><input aria-label="搜索目标项目" type="search" maxLength={512} value={projectQ} onInput={event => setProjectQ(event.currentTarget.value)} /><button disabled={busy} type="submit">查找目标项目</button></form>{loadingProjects && <p role="status">正在读取项目…</p>}{projectError && <p role="alert">{projectError}<button disabled={busy || loadingProjects} onClick={() => { setProjectCursor(undefined); setProjectReload(value => value + 1); }}>重新读取目标项目</button></p>}<div className="wb-organization-projects">{projectPage?.records.filter(project => project.projectId).map(project => <button key={project.projectId!} disabled={busy || !project.path} aria-pressed={target?.projectId === project.projectId} onClick={() => { setTarget(project); setConfirmed(false); }}><strong>{projectName(project.path)}</strong><span>{project.path || '项目路径未记录'}</span></button>)}</div>{projectCursor && <button disabled={busy || loadingProjects} onClick={() => setProjectCursor(undefined)}>返回第一页</button>}{projectPage?.nextCursor && <button disabled={busy || loadingProjects} onClick={() => setProjectCursor(projectPage.nextCursor!)}>下一页项目</button>}</section>}
      <p>原归属：{result.organization.effective.projectPath || '未归类会话'}</p><p>目标归属：{targetText}</p>
      <label className="wb-organization-confirm"><input type="checkbox" checked={confirmed} disabled={busy || (action === 'project' && !target?.projectId)} onChange={event => setConfirmed(event.currentTarget.checked)} />确认只修改此条记录的展示归属，可恢复自动归类</label>
      <button disabled={busy || !confirmed || (action === 'project' && !target?.projectId)} onClick={() => void save()}>保存归属</button>
    </>}</>}
  </dialog>;
}
