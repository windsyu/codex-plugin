import { createContext } from 'preact';
import { useContext } from 'preact/hooks';
import { relativeFile, type FileSelection } from './workspace';

export const WorkspaceLinks = createContext<{ root: string; open: (file: FileSelection) => void } | null>(null);
export function FileLink({ path, line, cwd, remote = false }: { path: string; line?: number; cwd?: string | null; remote?: boolean }) {
  const workspace = useContext(WorkspaceLinks);
  const relative = workspace && !remote ? relativeFile(workspace.root, path, cwd) : null;
  return relative && workspace ? <button className="wb-file-link" title={`查看当前文件 ${relative}${line ? `:${line}` : ''}`} onClick={() => workspace.open({ path: relative, line })}><code>{path}{line ? `:${line}` : ''}</code></button> : <code>{path}{line ? `:${line}` : ''}</code>;
}
export function ArgumentFileLink({ arguments: raw, cwd }: { arguments: string; cwd?: string | null }) {
  let args: Record<string, unknown>;
  try { args = JSON.parse(raw); } catch { return null; }
  if (!args || typeof args !== 'object' || args.environment_id || args.environmentId) return null;
  const path = args.file_path ?? args.filePath ?? args.path;
  const line = args.line ?? args.start_line;
  if (typeof path !== 'string') return null;
  return <p className="wb-tool-path">当前文件：<FileLink path={path} cwd={typeof args.cwd === 'string' ? args.cwd : cwd} line={typeof line === 'number' && line > 0 ? line : undefined} /></p>;
}
