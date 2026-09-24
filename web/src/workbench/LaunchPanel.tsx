import './launch.css';
import type { JSX } from 'preact';
import { useLayoutEffect, useRef } from 'preact/hooks';

export interface LaunchPreview {
  openInNewTab?: boolean;
  targetId: string;
  canonicalPath: string;
  configRevision: string;
  modes: ('new' | 'resume')[];
  nativeHome: string;
  expiresInSeconds: number;
  existingRun?: { runId: string; projectName: string; state: string } | null;
}

export interface LaunchPanelProps {
  path: string;
  pathEditable: boolean;
  mode: 'new' | 'resume';
  preview?: LaunchPreview;
  busy: boolean;
  pending: boolean;
  uncertain: boolean;
  error: string;
  readyHref?: string;
  onPathChange: (value: string) => void;
  onValidate: () => void;
  onStart: () => void;
  onCheck: () => void;
  onClose: () => void;
  onEnter: (runId: string) => void;
  onPick?: () => void;
  picking?: boolean;
}

export function LaunchPanel({
  path, pathEditable, mode, preview, busy, pending, uncertain, error, readyHref,
  onPathChange, onValidate, onStart, onCheck, onClose, onEnter, onPick, picking = false,
}: LaunchPanelProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const element = dialog.current;
    if (element && !element.open) element.showModal?.();
    return () => {
      element?.close?.();
      opener.current?.focus();
    };
  }, []);

  const allowed = !!preview && preview.modes.includes(mode);
  const locked = busy || pending || uncertain || picking;
  const existingRun = preview?.existingRun;
  const existingRunning = !!existingRun && existingRun.state === 'running';
  const canStart = allowed && !locked && !existingRunning;
  const title = mode === 'new' ? '打开项目' : '继续此会话';
  const closeLabel = pending || uncertain ? '收起' : '取消';
  const onInput: JSX.GenericEventHandler<HTMLInputElement> = event => onPathChange(event.currentTarget.value);
  const validate = () => { if (!locked && path.trim()) onValidate(); };
  const onSubmit: JSX.GenericEventHandler<HTMLFormElement> = event => { event.preventDefault(); validate(); };

  return <dialog ref={dialog} className="wb-launch-panel" aria-labelledby="wb-launch-title" onCancel={event => { event.preventDefault(); onClose(); }}>
    <div className="wb-launch-heading">
      <strong id="wb-launch-title">{title}</strong>
      <button type="button" className="wb-launch-close" onClick={onClose}>{closeLabel}</button>
    </div>

    <form className="wb-launch-body" onSubmit={onSubmit}>
      {pathEditable && <div className="wb-launch-path-entry">
        <label className="wb-launch-path-label">项目目录
          <input autoFocus aria-label="项目目录" placeholder="例如 ~/projects/my-project" value={path} onInput={onInput} disabled={locked} />
        </label>
        {onPick && <button type="button" className="wb-launch-pick" onClick={onPick} disabled={locked}>选择文件夹…</button>}
      </div>}

      {pathEditable && <button type="submit" className="wb-launch-validate" disabled={locked || !path.trim()}>检查目录</button>}
      {readyHref && <p className="wb-launch-state" role="status">工作台已就绪。<a href={readyHref} target="_blank" rel="noopener">打开工作台</a></p>}
      {error && <p className="wb-launch-error" role="alert">{error}</p>}
      {busy && <p className="wb-launch-state" role="status">正在检查目录…</p>}
      {picking && <p className="wb-launch-state" role="status">正在选择文件夹…</p>}
      {!readyHref && !pathEditable && !preview && !busy && !pending && !uncertain && <button type="button" onClick={onValidate}>重新检查</button>}
      {pending && <p className="wb-launch-state" role="status">正在启动 Codex…</p>}
      {uncertain && <div className="wb-launch-uncertain" role="status"><p>启动结果暂未确认，请查询已有操作。</p><button type="button" onClick={onCheck} disabled={busy || picking}>查询启动结果</button></div>}

      {preview && !uncertain && <div className="wb-launch-preview" aria-live="polite">
        <div className="wb-launch-location"><span>已确认目录</span><code>{preview.canonicalPath}</code></div>
        {mode === 'resume' && preview.nativeHome && <details className="wb-launch-native"><summary>所选会话的 Codex 数据目录</summary><p>由官方 Codex CLI 管理，用于继续这条会话。</p><code>{preview.nativeHome}</code></details>}
        {existingRunning && existingRun && <div className="wb-launch-existing"><p>此目录已有运行中的工作台：{existingRun.projectName}</p>{mode === 'resume' && <p>进入后查看当前对话；要恢复所选历史，请先结束该工作台的 CLI。</p>}<button type="button" onClick={() => onEnter(existingRun.runId)} disabled={busy || picking}>进入工作台</button></div>}
        {existingRun && !existingRunning && <p className="wb-launch-ended">之前的运行已结束，可以{mode === 'new' ? '开始新对话' : '继续所选会话'}。</p>}
        {!existingRunning && <button type="button" className="wb-launch-start" onClick={onStart} disabled={!canStart}>{mode === 'new' ? '开始新对话' : '继续此会话'}</button>}
        {!allowed && <p className="wb-launch-help">当前检查结果不支持此操作，请重新检查目录。</p>}
      </div>}
    </form>
  </dialog>;
}
