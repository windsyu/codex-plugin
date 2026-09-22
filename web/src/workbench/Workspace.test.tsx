import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { WorkspacePanel } from './WorkspacePanel';
import { FileReading } from './FileReading';
import { WorkspaceLinks, FileLink, ArgumentFileLink } from './WorkspaceLinks';
import { relativeFile } from './workspace';
const root = document.createElement('div');
const meta = { readAt: '2026-09-20T12:00:00Z', elapsedMs: 12, truncated: false };
const response = (value: unknown, ok = true) => ({ ok, json: async () => value });
const button = (label: string) => [...root.querySelectorAll('button')].find(b => b.textContent?.trim() === label || b.getAttribute('aria-label') === label)!;
const click = async (target: HTMLElement) => { await act(async () => target.click()); await act(async () => {}); };
// jsdom has no layout; scrolling/selection geometry is verified in installed Chrome.
beforeEach(() => {
  Object.defineProperty(Range.prototype, 'getClientRects', { configurable: true, value: () => [] });
  Object.defineProperty(Range.prototype, 'getBoundingClientRect', { configurable: true, value: () => new DOMRect() });
  document.body.append(root);
});
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
it('opens files and paginates without treating unsafe names as markup', async () => {
  const open = vi.fn(); const fetch = vi.fn().mockResolvedValue(response({ ...meta, entries: [{ name: '<img>.ts', path: '<img>.ts', kind: 'file' }], nextCursor: 'cursor', omitted: 0 })); vi.stubGlobal('fetch', fetch);
  await act(async () => render(<WorkspacePanel tab="files" active selection={{ path: '<img>.ts' }} onClose={() => {}} onOpen={open} />, root));
  await act(async () => {});
  expect(root.querySelector('img')).toBeNull();
  expect(root.querySelector('.wb-file-row[aria-selected="true"]')?.getAttribute('title')).toBe('<img>.ts');
  expect(root.querySelector('[role="tree"] .wb-read-stamp')).toBeNull();
  expect(root.querySelector('.wb-workspace-footnote .wb-read-stamp')?.textContent).toContain('读取于');
  await click(root.querySelector('.wb-file-row')!); expect(open).toHaveBeenCalledWith({ path: '<img>.ts' });
  await click(button('下一页文件')); expect(fetch.mock.calls.at(-1)![0]).toContain('cursor=cursor');
});
it('submits literal search, reports invalid regex and aborts an obsolete query', async () => {
  let resolve: (value: unknown) => void = () => {};
  const fetch = vi.fn().mockImplementationOnce(() => new Promise(done => { resolve = done; })).mockResolvedValue(response({ error: { code: 'invalid_regex' } }, false));vi.stubGlobal('fetch', fetch);
  const show = (tab: 'search' | 'files') => render(<WorkspacePanel tab={tab} active onClose={() => {}} onOpen={() => {}} />, root);
  await act(async () => show('search'));
  const input = root.querySelector('input[aria-label="搜索内容"]') as HTMLInputElement;
  await act(async () => { input.value = '-test'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  expect(fetch.mock.calls[0][0]).toContain('q=-test'); expect(fetch.mock.calls[0][0]).toContain('regex=false');
  const signal = fetch.mock.calls[0][1].signal;
  await act(async () => show('files')); expect(signal.aborted).toBe(true);
  await act(async () => resolve(response({ ...meta, hits: [{ path: 'obsolete', line: 1, text: 'obsolete' }] })));
  expect(root.textContent).not.toContain('obsolete');
  await act(async () => show('search')); expect(root.textContent).toContain('正则表达式无效');
});
it('reads a continuous document, escapes code, jumps to source lines and labels content diff', async () => {
  const text = Array.from({ length: 900 }, (_, i) => `${i} <script>literal</script>`).join('\n');
  const fetch = vi.fn().mockResolvedValue(response({ ...meta, path: 'large.ts', text, revision: 'file' })); vi.stubGlobal('fetch', fetch);
  await act(async () => render(<FileReading selection={{ path: 'large.ts', line: 501 }} onOpen={() => {}} onClose={() => {}} blocked={false} />, root));
  await act(async () => {});
  await vi.waitFor(() => expect(root.querySelector('.target')?.getAttribute('data-line')).toBe('501'));
  expect(root.textContent).not.toContain('上一段');
  expect(root.textContent).not.toContain('下一段');
  expect(root.querySelector('.target')?.getAttribute('data-line')).toBe('501'); expect(root.querySelector('script')).toBeNull();
  fetch.mockResolvedValue(response({ ...meta, path: 'large.ts', patch: '@@ -1 +1 @@\n-before\n+<img>after\n', revision: 'diff' }));
  await act(async () => render(<FileReading selection={{ path: 'large.ts', scope: 'staged' }} onOpen={() => {}} onClose={() => {}} blocked={false} />, root));
  await act(async () => {});
  await vi.waitFor(() => expect(root.querySelector('.added')?.textContent).toContain('<img>after'));
  expect(root.textContent).toContain('HEAD → 暂存区');expect(root.querySelector('img')).toBeNull();
});
it('keeps Git staged and working entries separate and reports missing capability', async () => {
  const open = vi.fn(); const fetch = vi.fn().mockResolvedValue(response({ ...meta, branch: 'feature', entries: [{ path: 'file.ts', index: 'M', working: 'M' }], omitted: 0 }));vi.stubGlobal('fetch', fetch);
  await act(async () => render(<WorkspacePanel tab="git" active onClose={() => {}} onOpen={open} />, root));
  await act(async () => {});
  expect(root.querySelectorAll('.wb-git-row')).toHaveLength(2);
  await click(root.querySelector('.wb-git-row button')!); expect(open).toHaveBeenCalledWith({ path: 'file.ts', scope: 'staged' });
  await click(root.querySelectorAll('.wb-git-row')[1].querySelector('button')!); expect(open).toHaveBeenLastCalledWith({ path: 'file.ts', scope: 'working' });
  fetch.mockResolvedValue(response({ error: { code: 'git_unavailable' } }, false));await click(button('刷新 Git'));
  expect(root.textContent).toContain('未找到 Git');
  expect(root.textContent).toContain('保留上次成功读取的结果');
});
it('distinguishes same-named Git paths and selected diff scope while retaining rename and unknown status text', async () => {
  const open = vi.fn();
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response({ ...meta, branch: '<svg>feature', entries: [
    { path: 'src/name.ts', originalPath: 'src/<img>.ts', index: 'R', working: 'M' },
    { path: 'tests/name.ts', index: ' ', working: 'X' },
  ], omitted: 0 })));
  await act(async () => render(<WorkspacePanel tab="git" active selection={{ path: 'src/name.ts', scope: 'working' }} onClose={() => {}} onOpen={open} />, root));
  await act(async () => {});
  const rows = [...root.querySelectorAll('.wb-git-row')];
  expect(rows).toHaveLength(3);
  expect(rows.map(row => row.querySelector('.wb-git-name')?.textContent)).toEqual(['name.ts', 'name.ts', 'name.ts']);
  expect(rows.map(row => row.querySelector('.wb-git-path small')?.textContent)).toEqual(['src', 'src', 'tests']);
  expect(rows[0].querySelector('button')?.title).toContain('原路径：src/<img>.ts');
  expect(rows[0].textContent).toContain('重命名'); expect(rows[2].textContent).toContain('变化');
  expect(root.querySelectorAll('.wb-git-row button[aria-current="true"]')).toHaveLength(1);
  expect(rows[1].querySelector('button')?.getAttribute('aria-current')).toBe('true');
  expect(root.querySelector('img, svg:not(.wb-icon)')).toBeNull();
  await click(rows[2].querySelector('button')!); expect(open).toHaveBeenCalledWith({ path: 'tests/name.ts', scope: 'working' });
  await click(rows[0].querySelector('.wb-git-open')!); expect(open).toHaveBeenLastCalledWith({ path: 'src/name.ts' });
});
it('only links current local paths with explicit provenance, never remote or root escapes', async () => {
  expect(relativeFile('/project', '/project2/a')).toBeNull(); expect(relativeFile('/project', '../a')).toBeNull();
  expect(relativeFile('/project', 'a', '/project/src')).toBe('src/a');
  const open = vi.fn();
  await act(async () => render(<WorkspaceLinks.Provider value={{ root: '/project', open }}><FileLink path="/project/src/a.ts" line={4} /><FileLink path="/outside/a" /><FileLink path="src/remote" remote /><ArgumentFileLink arguments={'{"file_path":"src/read.ts","start_line":7}'} /><ArgumentFileLink arguments={'{"path":"no.ts","environment_id":"remote"}'} /></WorkspaceLinks.Provider>, root));
  await act(async () => {});
  expect(root.querySelectorAll('button')).toHaveLength(2);
  await click(root.querySelector('button')!);expect(open).toHaveBeenCalledWith({ path: 'src/a.ts', line: 4 });
});
