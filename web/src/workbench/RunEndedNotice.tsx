import { useLayoutEffect, useRef } from 'preact/hooks';
import type { TerminalView } from './terminalClient';
import { Icon } from './Icons';

export type TerminalExit = NonNullable<TerminalView['exit']>;

export function RunEndedNotice({ projectName, exit, applicationHome, onClose }: {
  projectName: string | null; exit: TerminalExit; applicationHome: boolean; onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const continueButton = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const element = dialog.current;
    element?.showModal();
    continueButton.current?.focus({ preventScroll: true });
    return () => { element?.close(); if (opener?.isConnected) opener.focus({ preventScroll: true }); };
  }, []);

  return <dialog ref={dialog} className="wb-run-ended-dialog" aria-labelledby="wb-run-ended-title" aria-describedby="wb-run-ended-description" onCancel={event => { event.preventDefault(); onClose(); }}>
    <button className="wb-run-ended-close" aria-label="关闭运行结束提示" onClick={onClose}><Icon name="close" /></button>
    <div className="wb-run-ended-body">
      <div className="wb-run-ended-icon" aria-hidden="true"><svg width="28" height="28" viewBox="0 0 28 28" fill="none"><circle cx="14" cy="14" r="11" stroke="currentColor" stroke-width="1.8" /><rect x="10" y="10" width="8" height="8" rx="1.5" fill="currentColor" /></svg></div>
      <p className="wb-run-ended-project">{projectName || '当前项目'}</p>
      <h1 id="wb-run-ended-title">本次运行已结束</h1>
      <p id="wb-run-ended-description">Codex CLI 已退出，当前终端无法继续输入。<br />你仍可查看本次对话、终端输出和用量记录。</p>
      <details className="wb-run-ended-reason"><summary>退出详情</summary><p>{exit.signal || `退出码 ${exit.code}`}</p></details>
    </div>
    <div className="wb-run-ended-actions">
      {applicationHome && <a href="/">返回首页</a>}
      <button ref={continueButton} onClick={onClose}>继续查看记录</button>
    </div>
  </dialog>;
}
