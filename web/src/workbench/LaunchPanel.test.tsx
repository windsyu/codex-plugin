import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { LaunchPanel, type LaunchPreview } from './LaunchPanel';

const root = document.createElement('div');
const preview: LaunchPreview = { targetId: 'target', canonicalPath: '/Users/test/project', configRevision: 'rev', modes: ['new', 'resume'], nativeHome: '/Users/test/.codex', expiresInSeconds: 30 };
const renderPanel = (overrides: Partial<Parameters<typeof LaunchPanel>[0]> = {}) => {
  const props = { path: '~/projects/my-project', pathEditable: true, mode: 'new' as const, busy: false, pending: false, uncertain: false, error: '', onPathChange: vi.fn(), onValidate: vi.fn(), onStart: vi.fn(), onCheck: vi.fn(), onClose: vi.fn(), onEnter: vi.fn(), ...overrides };
  render(<LaunchPanel {...props} />, root); return props;
};

beforeEach(() => {
  document.body.append(root);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { render(null, root); root.remove(); vi.restoreAllMocks(); });

it('opens a native dialog and restores the opener after cancel', async () => {
  const opener = document.createElement('button'); opener.textContent = '打开'; document.body.append(opener); opener.focus();
  const props = renderPanel();
  const dialog = root.querySelector('dialog') as HTMLDialogElement;
  expect(dialog.open).toBe(true);
  await act(async () => { dialog.dispatchEvent(new Event('cancel', { bubbles: true, cancelable: true })); });
  expect(props.onClose).toHaveBeenCalledTimes(1);
  render(null, root);
  expect(document.activeElement).toBe(opener);
  opener.remove();
});

it('keeps manual path entry, validates through Enter, and offers the folder picker when available', async () => {
  const props = renderPanel({ onPick: vi.fn() });
  const input = root.querySelector('input[aria-label="项目目录"]') as HTMLInputElement;
  await act(async () => { input.value = '/tmp/example'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  expect(props.onPathChange).toHaveBeenCalledWith('/tmp/example');
  await act(async () => { input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); (root.querySelector('form') as HTMLFormElement).dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  expect(props.onValidate).toHaveBeenCalledTimes(1);
  await act(async () => { (root.querySelector('.wb-launch-pick') as HTMLButtonElement).click(); });
  expect(props.onPick).toHaveBeenCalledTimes(1);
});

it('shows one confirmed target preview and explicit new or resume actions', async () => {
  const fresh = renderPanel({ preview });
  expect(root.textContent).toContain(preview.canonicalPath);
  expect(root.querySelectorAll('.wb-launch-location code')).toHaveLength(1);
  expect(root.querySelector('.wb-launch-start')?.textContent).toBe('开始新对话');
  await act(async () => { (root.querySelector('.wb-launch-start') as HTMLButtonElement).click(); });
  expect(fresh.onStart).toHaveBeenCalledTimes(1);
  const resume = renderPanel({ mode: 'resume', preview, pathEditable: false });
  expect(root.querySelector('.wb-launch-start')?.textContent).toBe('继续此会话');
  await act(async () => { (root.querySelector('.wb-launch-start') as HTMLButtonElement).click(); });
  expect(resume.onStart).toHaveBeenCalledTimes(1);
});

it('disables path and launch controls while a launch is pending', () => {
  renderPanel({ preview, pending: true });
  expect((root.querySelector('input') as HTMLInputElement).disabled).toBe(true);
  expect((root.querySelector('.wb-launch-validate') as HTMLButtonElement).disabled).toBe(true);
  expect((root.querySelector('.wb-launch-start') as HTMLButtonElement).disabled).toBe(true);
  expect(root.querySelector('.wb-launch-close')?.textContent).toBe('收起');
});

it('keeps CLI storage out of a new project prompt and explains the source when resuming', () => {
  renderPanel({ preview });
  expect(root.textContent).not.toContain('本次使用的原生历史位置');
  expect(root.textContent).not.toContain(preview.nativeHome);
  renderPanel({ preview, mode: 'resume', pathEditable: false });
  expect(root.textContent).toContain('所选会话的 Codex 数据目录');
  expect(root.textContent).toContain('由官方 Codex CLI 管理，用于继续这条会话。');
  expect(root.textContent).toContain(preview.nativeHome);
});

it('escapes user supplied paths and enters an existing running workbench', async () => {
  const onEnter = vi.fn();
  const existing = { ...preview, canonicalPath: '<script>alert(1)</script>', existingRun: { runId: 'run-1', projectName: '项目 A', state: 'running' } };
  renderPanel({ preview: existing, onEnter });
  expect(root.querySelector('script')).toBeNull();
  expect(root.textContent).toContain(existing.canonicalPath);
  await act(async () => { (root.querySelector('.wb-launch-existing button') as HTMLButtonElement).click(); });
  expect(onEnter).toHaveBeenCalledWith('run-1');
});
