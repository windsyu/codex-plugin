import { useEffect, useRef, useState } from 'preact/hooks';
import type { FileView, FileViewContent } from './fileView';

export function FileDocument({ content, diff, onOpenLine }: { content: FileViewContent; diff: boolean; onOpenLine: (line: number) => void }) {
  const host = useRef<HTMLDivElement>(null), reader = useRef<FileView | null>(null);
  const latest = useRef({ content, onOpenLine }); latest.current = { content, onOpenLine };
  const [lines, setLines] = useState(0), [jump, setJump] = useState('');
  const [error, setError] = useState(false), [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let disposed = false;
    setError(false);
    // Load the rendering engine only when a file is opened. A closed/changed file
    // must not create a late view or retain its old document.
    void import('./fileView').then(({ createFileView }) => {
      if (!disposed && host.current) reader.current = createFileView(host.current, latest.current.content, diff, line => latest.current.onOpenLine(line), setLines);
    }).catch(() => { if (!disposed) setError(true); });
    return () => { disposed = true; reader.current?.destroy(); reader.current = null; };
  }, [attempt]);
  useEffect(() => { reader.current?.update(content); }, [content.revision, content.targetLine]);
  return <>
    {error ? <p className="wb-notice" role="status">文件阅读组件加载失败。<button onClick={() => setAttempt(v => v + 1)}>重试</button></p> : !lines && <p className="wb-query-progress" role="status">正在打开文件…</p>}
    {!diff && lines > 0 && content.targetLine && content.targetLine > lines && <p className="wb-notice">文件已变化，目标行号超出当前内容。</p>}
    <div className={`wb-code-lines${diff ? ' wb-code-diff' : ''}`} ref={host} role="region" aria-label={diff ? 'Git 内容差异' : '代码内容'} />
    {lines > 0 && <div className="wb-code-navigation"><span>共 {lines} 行 · 只读</span><button onClick={() => reader.current?.find()} title="Ctrl / ⌘ F">查找</button>
      {!diff && <form onSubmit={e => { e.preventDefault(); reader.current?.jump(Number(jump), true); }}><input aria-label="跳转行号" placeholder="行号" type="number" min={1} max={lines} value={jump} onInput={e => setJump(e.currentTarget.value)} /><button>跳转</button></form>}
    </div>}
  </>;
}
