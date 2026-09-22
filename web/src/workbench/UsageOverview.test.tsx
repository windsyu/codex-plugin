import { render } from 'preact';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SaveOverview, UsageFooter, UsageOverview } from './UsageOverview';
import { RunDiagnostics } from './RunDiagnostics';
import { applyReadingEvent, readSnapshot } from './reading';
import { parseUsageSummary, type UsageSummary } from './usage';

const root = document.createElement('div');
beforeEach(() => document.body.append(root));
afterEach(() => { render(null, root); root.remove(); });
const metric = (tokens: number | null, responses = tokens === null ? 0 : 2) => ({ tokens, responses });
export const summaryFixture = (): UsageSummary => ({
  responseCount: 2, missingResponses: 0, excludedResponses: 0,
  inputTokens: metric(16000), outputTokens: metric(5000), totalTokens: metric(21000),
  cachedInputTokens: metric(9000), reasoningTokens: metric(1500), cacheWriteTokens: metric(null),
  captureIncomplete: false, unidentifiedResponse: false, capacityExceeded: false,
});
const snapshot = () => ({ runEpoch: 'current', schemaVersion: 2, viewSeq: 2, items: [], requests: [], responses: [], diagnostics: [], toolContexts: [], nativeCommands: [], nativeFileChanges: [], userCapture: { enabled: true, diagnostics: [] }, capture: 'ok', usageSummary: summaryFixture() });
it('puts tokens ahead of diagnostics, uses the reported total without adding its subsets, and opens on demand', () => {
  const reading = readSnapshot(snapshot(), 'current'), onToggle = vi.fn();
  render(<UsageFooter reading={reading} expanded={false} onToggle={onToggle} />, root);
  expect(root.textContent).toContain('Token 21K'); expect(root.textContent).toContain('输入 16K');
  expect(root.textContent).not.toMatch(/viewSeq|保存水位|CLI 进程|捕获：/);
  root.querySelector<HTMLButtonElement>('[aria-label="查看用量概览"]')!.click(); expect(onToggle).toHaveBeenCalledOnce();
  render(<UsageOverview summary={reading.usageSummary} connected />, root);
  expect(root.querySelector('.wb-usage-total strong')?.textContent).toBe('21,000');
  expect(root.querySelector('[aria-label="缓存命中"]')?.textContent).toContain('9,000');
  expect(root.querySelector('[aria-label="推理 Token"]')?.textContent).toContain('1,500');
  expect(root.textContent).toContain('不含启动前的历史会话');
});
it('distinguishes unreported tokens, zero, partial coverage, conflicts, overflow and disconnection', () => {
  render(<UsageOverview connected={false} />, root);
  expect(root.querySelector('.wb-usage-total strong')?.textContent).toBe('—');
  expect(root.textContent).toContain('等待模型返回用量');
  const summary = { ...summaryFixture(), responseCount: 4, missingResponses: 1, excludedResponses: 1, totalTokens: metric(0, 1), inputTokens: metric(null), capacityExceeded: true };
  render(<UsageOverview summary={summary} connected={false} />, root);
  expect(root.querySelector('.wb-usage-total strong')?.textContent).toBe('0');
  expect(root.textContent).toContain('已计入 1 / 4');
  expect(root.textContent).toContain('尚未提供用量');
  expect(root.textContent).toContain('异常或有冲突');
  expect(root.textContent).toContain('已达统计上限');
  expect(root.textContent).toContain('连接中，等待更新');
  render(<UsageOverview summary={{ ...summary, totalTokens: metric(null, 2) }} connected />, root);
  expect(root.textContent).toContain('总量超出可显示范围');
});
it('keeps saved-prefix diagnostics folded while loss and save failures remain visible', () => {
  const reading = readSnapshot({ ...snapshot(), capture: 'partial', recorderStatus: { runEpoch: 'current', state: 'degraded', persistedThroughViewSeq: 1, savedThroughViewSeq: 1, observedViewSeq: 2, gapCount: 1, historyCoverage: 'partial', error: 'sync_failed' } }, 'current');
  render(<><SaveOverview reading={reading} /><RunDiagnostics reading={reading} epoch={'<svg onload=alert(1)>'} processId={1234} /></>, root);
  const diagnostics = root.querySelector('details')!;
  expect(diagnostics.hasAttribute('open')).toBe(false);
  expect(diagnostics.textContent).toContain('连续保存至');
  expect(root.querySelector('.wb-save-overview')?.textContent).toContain('保存暂时失败');
  expect(root.querySelector('.wb-save-overview')?.textContent).toContain('历史未能保存');
  expect(root.querySelector('.wb-save-overview')?.textContent).not.toContain('保存水位');
  expect(root.querySelector('svg')).toBeNull();
});
it('replaces usage snapshots without recounting events, preserves totals when response previews shrink, and reads old history', () => {
  const before = readSnapshot(snapshot(), 'current');
  const response = { requestId: 'r', responseId: 'one', status: 'completed', reportedModels: [] };
  const event = { viewSeq: 3, kind: 'request.state' as const, response, usageSummary: { ...summaryFixture(), totalTokens: metric(22000) } };
  const after = applyReadingEvent(before, event);
  expect(applyReadingEvent(after, event)).toBe(after);
  expect(readSnapshot({ ...snapshot(), viewSeq: 3, usageSummary: after.usageSummary, responses: [] }, 'current').usageSummary?.totalTokens.tokens).toBe(22000);
  expect(readSnapshot({ ...snapshot(), usageSummary: undefined }, 'current').usageSummary).toBeUndefined();
  expect(() => readSnapshot(snapshot(), 'other')).toThrow();
  expect(() => parseUsageSummary({ ...summaryFixture(), inputTokens: metric(-1) })).toThrow();
  expect(() => parseUsageSummary({ ...summaryFixture(), totalTokens: metric(Number.MAX_SAFE_INTEGER + 1) })).toThrow();
  expect(() => parseUsageSummary({ ...summaryFixture(), missingResponses: 3 })).toThrow();
  expect(() => parseUsageSummary({ ...summaryFixture(), outputTokens: metric(0, 0) })).toThrow();
});
