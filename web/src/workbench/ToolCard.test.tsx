import { render } from 'preact';
import { afterEach, describe, expect, it } from 'vitest';
import { NativeCommandDetails, NativeFileChangeDetails, ToolCard, ToolContextDetails } from './ToolCard';
import type { ToolCall } from './toolTypes';

export const toolFixture = (): ToolCall => ({
  key: { requestId: 'request', responseId: 'response', wireItemId: 'tool-1', contentIndex: 0 }, kind: 'tool_call', toolKind: 'custom', callId: 'call-1', name: 'exec', namespace: null,
  category: 'code', arguments: 'text(await tools.exec_command({cmd:"echo hi"}));', argumentsState: 'receiving', command: null, execution: 'unobserved', result: null,
  proposedPatch: null, revision: 1, orderIndex: 2, captureSeq: 5, truncated: false, identityConflict: false, resultConflict: false,
});
const root = document.createElement('div');
afterEach(() => render(null, root));
describe('typed tool cards', () => {
  it('labels saved partial tool arguments and unfinished executions at the time of saving', () => {
    render(<ToolCard tool={{ ...toolFixture(), execution: 'running' }} historical />, root);
    expect(root.textContent).toContain('保存时：');
    expect(root.textContent).toContain('参数尚未完整生成');
    expect(root.textContent).toContain('尚未观察到执行结束');
    expect(root.textContent).not.toContain('参数生成中');
    expect(root.textContent).not.toContain('执行中');
  });
  it('keeps generated code separate from execution and never manufactures a nested command', () => {
    render(<ToolCard tool={toolFixture()} />, root);
    expect(root.textContent).toContain('代码工具'); expect(root.textContent).toContain('参数生成中');
    expect(root.textContent).toContain('尚未观察到执行'); expect(root.querySelector('.wb-command-preview')).toBeNull();
    render(<ToolCard tool={{ ...toolFixture(), argumentsState: 'generated', revision: 2 }} />, root);
    expect(root.textContent).toContain('参数已生成'); expect(root.textContent).not.toContain('执行成功');
    expect(root.querySelector('[data-role]')).toBeNull(); expect(root.querySelector('.wb-tool-result')).toBeNull();
  });
  it('renders command, directory, failed output and source separately with literal escaping', () => {
    render(<ToolCard tool={{ ...toolFixture(), category: 'command', command: { text: 'echo "<script>"', cwd: '/synthetic' }, execution: 'failed', result: { source: { kind: 'model_request', requestId: 'next-request', clientRequestIndex: null, inputIndex: 3 }, output: '<img src=x onerror=alert(1)>\nfailed', exitCode: 3, durationMs: 125, truncated: true, omitted: true } }} />, root);
    expect(root.querySelector('.wb-command-preview')?.textContent).toBe('echo "<script>"');
    expect(root.textContent).toContain('工作目录：/synthetic'); expect(root.textContent).toContain('退出码：3');
    expect(root.textContent).toContain('125 ms'); expect(root.textContent).toContain('next-req');
    expect(root.querySelector('.wb-tool-result details')?.hasAttribute('open')).toBe(true);
    expect(root.querySelector('img,script')).toBeNull(); expect(root.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(root.textContent).toContain('输出预览超限或来源含截断标记'); expect(root.textContent).toContain('部分非文本结果');
  });
  it('retains expansion when a later result replaces the card contents', () => {
    render(<ToolCard tool={toolFixture()} />, root);
    const details = root.querySelector('details')!; details.open = true;
    render(<ToolCard tool={{ ...toolFixture(), arguments: 'more code', revision: 2 }} />, root);
    expect(root.querySelector('details')).toBe(details); expect(details.open).toBe(true);
  });
  it('identifies a native final result without inventing a following model request', () => {
    render(<ToolCard tool={{ ...toolFixture(), category: 'command', argumentsState: 'generated', execution: 'declined', result: { source: { kind: 'native_rollout', sourceRef: 'source', byteOffset: 200, nativeItemId: 'call-1', processId: '41' }, output: 'request declined', exitCode: -1, durationMs: 0, truncated: false, omitted: false } }} />, root);
    expect(root.textContent).toContain('原生运行记录'); expect(root.textContent).toContain('已拒绝执行');
    expect(root.textContent).not.toContain('后续模型请求');
    expect(root.querySelector('.wb-tool-result details')?.hasAttribute('open')).toBe(true);
  });
  it('preserves unmatched native evidence as literal request details, without claiming a parent code call', () => {
    render(<NativeCommandDetails commands={[{ key: { codexThreadId: 'thread', codexTurnId: 'turn', nativeItemId: 'child' }, source: { sourceRef: 'source', byteOffset: 20, ordinal: null }, status: 'failed', processId: '41', commandSource: 'unified_exec_startup', command: ['printf', '<img onerror=alert(1)>'], cwd: '/synthetic', output: '<script>literal</script>', exitCode: 7, durationMs: 2, truncated: true, omitted: false }]} />, root);
    expect(root.textContent).toContain('同一轮的原生执行记录');
    expect(root.textContent).toContain('调用 child'); expect(root.textContent).toContain('退出码：7');
    expect(root.querySelector('script,img,[data-kind="tool_call"]')).toBeNull();
    expect(root.textContent).toContain('截断或省略');
  });
  it('makes missing identity, conflicting results and proposed patches explicit', () => {
    render(<ToolCard tool={{ ...toolFixture(), category: 'patch', callId: null, resultConflict: true, execution: 'succeeded', argumentsState: 'incomplete', truncated: true }} />, root);
    expect(root.textContent).toContain('不代表工作区已修改'); expect(root.textContent).toContain('调用 ID 未确认');
    expect(root.textContent).toContain('关联存在冲突'); expect(root.textContent).toContain('参数不完整');
    expect(root.querySelector('article')?.getAttribute('data-execution')).toBe('unobserved');
    expect(root.querySelector('.wb-execution.succeeded')).toBeNull();
  });
  it('keeps developer tool definitions and historic results inside request context', () => {
    render(<ToolContextDetails context={{ requestId: 'request', clientRequestIndex: null, partial: true, definitions: [{ name: 'exec', namespace: 'functions', toolKind: 'custom', category: 'code', source: 'additional_tools', inputIndex: 0 }], outputs: [{ inputIndex: 8, toolKind: 'custom', callId: null, output: '<svg onload=alert(1)>', truncated: false, omitted: false }] }} />, root);
    expect(root.textContent).toContain('functions.exec'); expect(root.textContent).toContain('additional_tools');
    expect(root.textContent).toContain('这里包含历史上下文'); expect(root.textContent).toContain('ID 未确认');
    expect(root.querySelector('[data-role],svg')).toBeNull();
  });
  it('shows proposed operations with literal paths, exact edit rows and unknown deletion counts', () => {
    render(<ToolCard tool={{ ...toolFixture(), name: 'apply_patch', category: 'patch', argumentsState: 'generated', proposedPatch: { state: 'ready', environmentId: null, files: [
      { operation: 'update', path: '../<img>.txt', moveTo: '新文件.txt', addedLines: 1, removedLines: 1, sections: [{ anchor: '<svg onload=alert(1)>', atEof: true, lines: [{ kind: 'context', text: 'keep' }, { kind: 'removed', text: 'same' }, { kind: 'added', text: 'same' }] }] },
      { operation: 'delete', path: 'old.txt', moveTo: null, addedLines: 0, removedLines: null, sections: [] },
      { operation: 'add', path: 'empty.txt', moveTo: null, addedLines: 0, removedLines: 0, sections: [] },
    ] } }} />, root);
    expect(root.querySelectorAll('.wb-diff-file')).toHaveLength(3);
    expect(root.textContent).toContain('移动并修改'); expect(root.textContent).toContain('../<img>.txt → 新文件.txt');
    expect(root.textContent).toContain('删除行数未知'); expect(root.textContent).toContain('−?');
    expect(root.textContent).toContain('拟议新增空文件'); expect(root.textContent).toContain('文件末尾');
    expect(root.querySelectorAll('.wb-diff-line.added, .wb-diff-line.removed')).toHaveLength(2);
    expect(root.querySelector('.wb-diff-lines')?.getAttribute('tabindex')).toBe('0');
    expect(root.querySelector('img,svg,script,a')).toBeNull();
    expect(root.textContent).toContain('尚未观察到执行'); expect(root.textContent).toContain('不代表工作区已修改');
  });
  it('retains parameter and diff expansion and focus when final parameters and late results arrive', () => {
    document.body.append(root);
    const base: ToolCall = { ...toolFixture(), category: 'patch', name: 'apply_patch' };
    render(<ToolCard tool={base} />, root);
    const parameters = root.querySelector<HTMLDetailsElement>('.wb-tool-arguments')!;
    parameters.open = true; const summary = parameters.querySelector('summary')!; summary.focus();
    const complete: ToolCall = { ...base, argumentsState: 'generated', revision: 2, proposedPatch: { state: 'ready', environmentId: null, files: [{ operation: 'add', path: 'file', moveTo: null, addedLines: 1, removedLines: 0, sections: [{ anchor: null, atEof: false, lines: [{ kind: 'added', text: 'new' }] }] }] } };
    render(<ToolCard tool={complete} />, root);
    expect(root.querySelector('.wb-tool-arguments')).toBe(parameters); expect(parameters.open).toBe(true);
    expect(document.activeElement).toBe(summary);
    const file = root.querySelector<HTMLDetailsElement>('.wb-diff-file')!; file.open = false;
    const fileSummary = file.querySelector('summary')!; fileSummary.focus();
    render(<ToolCard tool={{ ...complete, revision: 3, execution: 'result_observed', result: { source: { kind: 'model_request', requestId: 'next', clientRequestIndex: null, inputIndex: 4 }, output: 'observed output', exitCode: null, durationMs: null, truncated: false, omitted: false } }} />, root);
    expect(root.querySelector('.wb-diff-file')).toBe(file); expect(file.open).toBe(false);
    expect(document.activeElement).toBe(fileSummary); expect(root.querySelector('.wb-tool-result')).not.toBeNull();
    root.remove();
  });
  it('retains parameters when a complete diff cannot be safely shown', () => {
    for (const reason of ['incomplete', 'truncated', 'identity_conflict', 'unsupported_format', 'budget'] as const) {
      render(<ToolCard tool={{ ...toolFixture(), category: 'patch', proposedPatch: { state: 'unavailable', reason } }} />, root);
      expect(root.querySelector('.wb-diff-file')).toBeNull();
      expect(root.querySelector('[role="status"]')).not.toBeNull();
      expect(root.querySelector('.wb-tool-arguments pre')?.textContent).toBe(toolFixture().arguments);
    }
  });
  it('shows explicit native patch failure with separate stdout/stderr without inventing an exit code', () => {
    render(<ToolCard tool={{ ...toolFixture(), category: 'patch', execution: 'failed', result: { source: { kind: 'native_rollout', sourceRef: 'source', byteOffset: 42, nativeItemId: 'call-1', processId: null }, output: '', streams: { stdout: '', stderr: '<script>native failure</script>' }, exitCode: null, durationMs: null, truncated: false, omitted: false } }} />, root);
    const result = root.querySelector('.wb-tool-result')!;
    expect(root.textContent).toContain('执行失败'); expect(result.textContent).toContain('原生运行记录');
    expect(result.querySelector('[aria-label="stdout"]')?.textContent).toContain('空输出');
    expect(result.querySelector('[aria-label="stderr"]')?.textContent).toContain('<script>native failure</script>');
    expect(result.textContent).not.toContain('未区分'); expect(result.textContent).not.toContain('退出码');
    expect(result.querySelector('details')?.hasAttribute('open')).toBe(true);expect(root.querySelector('script')).toBeNull();
  });
  it('keeps unmatched native file evidence readable with literal paths and missing stream labels', () => {
    render(<NativeFileChangeDetails changes={[{ key: { codexThreadId: 'thread', codexTurnId: 'turn', nativeItemId: 'child' }, source: { sourceRef: 'source', byteOffset: 800, ordinal: null }, status: 'declined', files: [{ path: '../<img>.txt', operation: 'unknown', moveTo: 'new.txt' }], stdout: null, stderr: '', truncated: true, omitted: true }]} />, root);
    expect(root.textContent).toContain('同一轮的原生文件修改记录');expect(root.textContent).toContain('已拒绝执行');
    expect(root.textContent).toContain('../<img>.txt → new.txt');expect(root.textContent).toContain('未知修改');
    expect(root.querySelector('[aria-label="stdout"]')?.textContent).toContain('未捕获');
    expect(root.querySelector('[aria-label="stderr"]')?.textContent).toContain('空输出');
    expect(root.querySelector('img,a,[data-kind="tool_call"]')).toBeNull();
  });
});
