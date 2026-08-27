import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ItemCard, Sidebar, TurnSection, itemRendererKind, threadDisplay } from './App';
import type { Item, ProjectSummary, Source, Thread, Turn } from './types';

const containers: HTMLElement[] = [];

function mount(node: preact.ComponentChildren) {
  const container = document.createElement('div');
  document.body.append(container);
  containers.push(container);
  act(() => render(node, container));
  return container;
}

afterEach(() => {
  for (const container of containers.splice(0)) {
    act(() => render(null, container));
    container.remove();
  }
});

const thread = {
  threadKey: 'thread-1',
  codexThreadId: 'thread-1',
  storeSourceId: 'source-1',
  archived: false,
  stale: true,
  captureCompleteness: 'durable_partial',
  completenessReasons: ['durable_turn_not_terminal'],
  lastEventSeq: 2,
  ruleVersion: 'v1',
  project: { key: 'project-1', name: 'Project', path: '/tmp/project' },
  context: { session: {}, runtime: {} }
} as Thread;

const project = {
  project: thread.project,
  threadCount: 1,
  currentThreadCount: 1,
  lastRecencyAtMs: 1
} as ProjectSummary;

describe('Viewer components', () => {
  it('turns raw previews into concise human-facing thread titles', () => {
    expect(threadDisplay({ ...thread, lastMessagePreview: '{"risk_level":"low","outcome":"allow","rationale":"只读检查本机服务"}' })).toEqual({
      title: '审批审查 · 允许', excerpt: '只读检查本机服务'
    });
    expect(threadDisplay({ ...thread, lastMessagePreview: '<turn_aborted>\nThe previous turn was interrupted' })).toEqual({
      title: '已中断的会话', excerpt: '执行被中断，历史记录可能不完整'
    });
    expect(threadDisplay({ ...thread, lastMessagePreview: '已完成界面改进：\n- 收纳子代理记录\n- 优化标题' })).toEqual({
      title: '已完成界面改进：', excerpt: '收纳子代理记录'
    });
  });

  it('submits search with Enter and exposes named filters and selected state', () => {
    const onSearch = vi.fn();
    const container = mount(<Sidebar
      projects={[project]}
      threads={[thread]}
      sources={[]}
      filters={{ source: '', status: '', completeness: '', archived: '', q: '' }}
      setFilters={vi.fn()}
      searchResults={null}
      searchState={{ phase: 'idle' }}
      selected="thread-1"
      onSearch={onSearch}
      onSearchResult={vi.fn()}
      onSelect={vi.fn()}
    />);
    const input = container.querySelector<HTMLInputElement>('#viewer-search')!;
    act(() => {
      input.value = 'needle';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    act(() => {
      input.form!.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
    });
    expect(onSearch).toHaveBeenCalledWith('needle');
    expect(container.querySelectorAll('label select')).toHaveLength(4);
    expect(container.querySelector('[aria-current="true"]')).not.toBeNull();
    expect(container.querySelector('.project-heading .icon-folder')).not.toBeNull();
  });

  it('shows snippets and opens the exact search result', () => {
    const onSearchResult = vi.fn();
    const result = { entityKey: 'e', threadKey: 'thread-1', turnId: 'turn-2', itemId: 'item-3', snippet: 'safe snippet', score: 1, lastEventSeq: 2 };
    const container = mount(<Sidebar
      projects={[project]}
      threads={[thread]}
      sources={[]}
      filters={{ source: '', status: '', completeness: '', archived: '', q: 'safe' }}
      setFilters={vi.fn()}
      searchResults={[result]}
      searchState={{ phase: 'success' }}
      onSearch={vi.fn()}
      onSearchResult={onSearchResult}
      onSelect={vi.fn()}
    />);
    expect(container.textContent).toContain('safe snippet');
    act(() => (container.querySelector('.search-result') as HTMLButtonElement).click());
    expect(onSearchResult).toHaveBeenCalledWith(result);
  });

  it('uses explicit renderers and treats unknown payload HTML as text', () => {
    expect(itemRendererKind('command_execution')).toBe('tool');
    expect(itemRendererKind('approval')).toBe('request');
    expect(itemRendererKind('new_future_variant')).toBe('unknown');
    const item = {
      turnScope: 'turn-1', itemId: 'item-1', itemType: 'new_future_variant', status: 'completed',
      raw: { type: 'future', payload: '<img src=x onerror=alert(1)>' }, provenance: {}, lastEventSeq: 1
    } as Item;
    const container = mount(<ItemCard item={item} token="" onBlob={vi.fn()} />);
    expect(container.querySelector('img')).toBeNull();
    expect(container.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(container.textContent).toContain('未知 Item 类型');
  });

  it('renders dialogue first and collapses coalesced process records by default', () => {
    const items = [
      { turnScope: 'turn-1', turnId: 'turn-1', itemId: 'user', itemType: 'user_message', status: 'completed', summaryText: '请检查测试', raw: {}, provenance: {}, lastEventSeq: 1 },
      { turnScope: 'turn-1', turnId: 'turn-1', itemId: 'call', itemType: 'tool_call', status: 'completed', raw: { call_id: 'call-1', name: 'test' }, provenance: {}, lastEventSeq: 2 },
      { turnScope: 'turn-1', turnId: 'turn-1', itemId: 'output', itemType: 'tool_output', status: 'completed', raw: { call_id: 'call-1', output: 'ok' }, provenance: {}, lastEventSeq: 3 },
      { turnScope: 'turn-1', turnId: 'turn-1', itemId: 'answer', itemType: 'agent_message', status: 'completed', summaryText: '测试通过', raw: { phase: 'commentary' }, provenance: {}, lastEventSeq: 4 }
    ] as Item[];
    const turn = { turnId: 'turn-1', status: 'completed', captureCompleteness: 'durable_complete', completenessReasons: [], coverage: {}, raw: {}, lastEventSeq: 4 } as Turn;
    const container = mount(<TurnSection turn={turn} items={items} token="" onBlob={vi.fn()} ordinal={1} />);
    expect(container.querySelectorAll('.dialogue-message')).toHaveLength(2);
    expect(container.querySelectorAll('.dialogue-avatar')).toHaveLength(1);
    expect(container.querySelectorAll('.activity-entry')).toHaveLength(1);
    expect(container.querySelector('.activity-counts')?.textContent).toBe('1 次工具调用');
    expect((container.querySelector('.activity-panel') as HTMLDetailsElement).open).toBe(false);
    expect(container.textContent).toContain('进度更新');
  });

  it('surfaces failed and pending process records without opening every healthy detail', () => {
    const failed = { turnScope: 'turn-1', itemId: 'error', itemType: 'error', status: 'failed', summaryText: 'failed safely', raw: {}, provenance: {}, lastEventSeq: 1 } as Item;
    const container = mount(<TurnSection turn={null} items={[failed]} token="" onBlob={vi.fn()} ordinal={1} />);
    expect((container.querySelector('.activity-panel') as HTMLDetailsElement).open).toBe(true);
    expect(container.textContent).toContain('failed safely');
  });

  it('shows source compatibility risk once instead of stamping every Thread row', () => {
    const container = mount(<Sidebar projects={[project]} threads={[thread]} sources={[{
      sourceId: 'source-1', kind: 'rollout', stableIdentity: 'source', status: 'ready', currentEpoch: { decodeErrorCount: 2, unknownEventCount: 3 }
    }] as Source[]} filters={{ source: '', status: '', completeness: '', archived: '', q: '' }} setFilters={vi.fn()}
      searchResults={null} searchState={{ phase: 'idle' }} onSearch={vi.fn()} onSearchResult={vi.fn()} onSelect={vi.fn()} />);
    expect(container.querySelectorAll('.compatibility-notice')).toHaveLength(1);
    expect(container.querySelector('.compatibility-notice')?.textContent).toContain('3 条未知事件');
    expect(container.querySelector('.thread-row')?.textContent).not.toContain('unknown event');
  });

  it('keeps primary conversations visible and tucks sub-agent records into a disclosure', () => {
    const child = { ...thread, threadKey: 'thread-child', codexThreadId: 'thread-child', parentThreadKey: thread.threadKey,
      lastMessagePreview: '{"risk_level":"low","outcome":"allow","rationale":"只读检查"}' } as Thread;
    const container = mount(<Sidebar projects={[{ ...project, threadCount: 2, currentThreadCount: 2 }]} threads={[thread, child]} sources={[]}
      filters={{ source: '', status: '', completeness: '', archived: '', q: '' }} setFilters={vi.fn()} searchResults={null}
      searchState={{ phase: 'idle' }} onSearch={vi.fn()} onSearchResult={vi.fn()} onSelect={vi.fn()} />);
    expect(container.querySelectorAll('.primary-thread-list > .thread-row')).toHaveLength(1);
    expect(container.querySelector('.subagent-group summary')?.textContent).toContain('子代理记录1');
    expect((container.querySelector('.subagent-group') as HTMLDetailsElement).open).toBe(false);
    expect(container.querySelector('.subagent-list')?.textContent).toContain('审批审查 · 允许');
  });
});
