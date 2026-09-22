import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ConversationFollowButton, FeedbackNotice, ItemCard, Sidebar, TurnSection, dedupeDialogueItems, deferInitialDashboardForPairing, itemRendererKind, shouldFollowLatestMessage, threadDisplay } from './App';
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
  it('defers unauthenticated Dashboard requests until a pairing fragment is redeemed', () => {
    expect(deferInitialDashboardForPairing('#pair=signed-code')).toBe(true);
    expect(deferInitialDashboardForPairing('#other=value')).toBe(false);
    expect(deferInitialDashboardForPairing('')).toBe(false);
  });

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

  it('keeps projectless conversations in Recent instead of inventing cwd projects', () => {
    const projectless = {
      ...thread,
      threadKey: 'thread-projectless',
      codexThreadId: 'thread-projectless',
      cwdDisplay: '/Users/demo/Documents/Codex/2026-08-28/generated-name',
      name: '独立对话',
      project: undefined
    } as unknown as Thread;
    const container = mount(<Sidebar projects={[]} threads={[projectless]} sources={[]}
      filters={{ source: '', status: '', completeness: '', archived: '', q: '' }} setFilters={vi.fn()} searchResults={null}
      searchState={{ phase: 'idle' }} onSearch={vi.fn()} onSearchResult={vi.fn()} onSelect={vi.fn()} />);
    expect(container.querySelector('.recent-section')?.textContent).toContain('独立对话');
    expect(container.textContent).not.toContain('generated-name');
    expect(container.querySelectorAll('.project-group')).toHaveLength(0);
  });

  it('follows recent conversation updates without stealing an older reading position', () => {
    expect(shouldFollowLatestMessage(0)).toBe(true);
    expect(shouldFollowLatestMessage(119)).toBe(true);
    expect(shouldFollowLatestMessage(121)).toBe(false);
  });

  it('shows unseen progress in a direct return-to-latest action', () => {
    const onJump = vi.fn();
    const container = mount(<ConversationFollowButton unseenUpdates={3} onJump={onJump} />);
    expect(container.textContent).toContain('回到最新 · 3 条新进展');
    expect(container.querySelector('.icon-down')).not.toBeNull();
    act(() => (container.querySelector('button') as HTMLButtonElement).click());
    expect(onJump).toHaveBeenCalledOnce();
  });

  it('renders one dialogue message for mirrored rollout event and response records', () => {
    const base = { turnScope:'turn-1',turnId:'turn-1',status:'completed',startedAtMs:1_000,
      completedAtMs:1_000,summaryText:'same text',provenance:{source:'rollout'},lastEventSeq:1 };
    const eventUser = { ...base,itemId:'event-user',itemType:'user_message',raw:{type:'user_message',message:'same text'} } as Item;
    const responseUser = { ...base,itemId:'response-user',itemType:'user_message',raw:{type:'message',role:'user',content:[{type:'input_text',text:'same text'}]} } as Item;
    const eventAssistant = { ...base,itemId:'event-agent',itemType:'agent_message',raw:{type:'agent_message',message:'same answer'} } as Item;
    const responseAssistant = { ...base,itemId:'response-agent',itemType:'agent_message',startedAtMs:5_000,
      raw:{type:'message',role:'assistant',content:[{type:'output_text',text:'same answer'}]} } as Item;
    expect(dedupeDialogueItems([responseUser,eventUser,eventAssistant,responseAssistant]))
      .toEqual([eventUser,eventAssistant]);
  });

  it('renders one user message when App Server live and rollout durable projections overlap', () => {
    const base = { turnScope:'turn-1',turnId:'turn-1',itemType:'user_message',status:'completed',
      completedAtMs:1_000,summaryText:'same input',lastEventSeq:1 };
    const live = { ...base,itemId:'live-user',startedAtMs:1_150,raw:{type:'userMessage',clientId:'client-1'},
      provenance:{source:'app_server'} } as Item;
    const response = { ...base,itemId:'response-user',startedAtMs:1_000,
      raw:{type:'message',role:'user',content:[{type:'input_text',text:'same input'}]},provenance:{source:'rollout'} } as Item;
    const event = { ...base,itemId:'event-user',startedAtMs:1_000,
      raw:{type:'user_message',client_id:'client-1',message:'same input'},provenance:{source:'rollout'} } as Item;
    expect(dedupeDialogueItems([live,response,event])).toEqual([event]);
  });

  it('deduplicates image messages without rendering a local staging path', () => {
    const base = { turnScope:'turn-image',turnId:'turn-image',itemType:'user_message',status:'completed',
      completedAtMs:1_000,lastEventSeq:1 };
    const live = { ...base,itemId:'live-image',startedAtMs:4_100,summaryText:'inspect image',
      raw:{type:'userMessage',clientId:'client-image',content:[{type:'text',text:'inspect image'},
        {type:'localImage',path:'/private/staging/image.bin'}]},provenance:{source:'app_server'} } as Item;
    const response = { ...base,itemId:'response-image',startedAtMs:1_000,
      summaryText:'inspect image\n<image name=[Image #1] path="/private/staging/image.bin">\n</image>',
      raw:{type:'message',role:'user',content:[{type:'input_text',text:'inspect image'},
        {type:'input_image',image_url:{$redacted:true}}]},provenance:{source:'rollout'} } as Item;
    const event = { ...base,itemId:'event-image',startedAtMs:1_000,summaryText:'inspect image',
      raw:{type:'user_message',client_id:'client-image',message:'inspect image',local_images:[{$redacted:true}]},
      provenance:{source:'rollout'} } as Item;
    const items = dedupeDialogueItems([live,response,event]);
    expect(items).toEqual([event]);
    const container = mount(<TurnSection turn={null} items={items} token="" onBlob={vi.fn()} ordinal={1} />);
    expect(container.querySelectorAll('.dialogue-message')).toHaveLength(1);
    expect(container.textContent).toContain('图片附件 · 1 张');
    expect(container.textContent).not.toContain('/private/staging');
  });

  it('renders operation feedback as contextual success, warning, or error notices', () => {
    const onClose = vi.fn();
    const success = mount(<FeedbackNotice message="已复制脱敏 JSON" onClose={onClose} />);
    expect(success.firstElementChild?.classList.contains('notice-success')).toBe(true);
    expect(success.firstElementChild?.getAttribute('role')).toBe('status');
    act(() => (success.querySelector('button') as HTMLButtonElement).click());
    expect(onClose).toHaveBeenCalledOnce();

    const warning = mount(<FeedbackNotice message="操作结果未知：请刷新状态，系统不会自动重放" onClose={vi.fn()} />);
    expect(warning.firstElementChild?.classList.contains('notice-warning')).toBe(true);
    const error = mount(<FeedbackNotice message="复制失败" onClose={vi.fn()} />);
    expect(error.firstElementChild?.classList.contains('notice-error')).toBe(true);
  });

});
