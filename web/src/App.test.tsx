import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Composer, ConversationFollowButton, ControlSettings, FeedbackNotice, GatewayStatusCardView, GoalStatusBar, ItemCard, NewThreadDialog, OptimisticMessageCard, PendingRequestCard, Sidebar, TurnSection, canNavigateComposerHistory, clearThreadInput, commandNotice, currentSessionPresence, dedupeDialogueItems, deferInitialDashboardForPairing, itemRendererKind, reconcileOptimisticMessages, resizeComposerTextarea, selectThreadControlCatalog, shouldFollowLatestMessage, threadDisplay } from './App';
import type { ControlCatalog, ControllerSource, Item, PendingRequest, ProjectSummary, Source, Thread, Turn } from './types';

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

  it('renders exact approval target and emits a closed typed action', () => {
    const onAction = vi.fn();
    const request = { requestKey:'signed',sourceId:'source',sourceEpoch:'epoch',requestId:'7',requestType:'approval',
      state:'pending',requestVersion:1,payload:{command:'cargo test',cwd:'/workspace'},requestEventSeq:1 } as PendingRequest;
    const container = mount(<PendingRequestCard request={request} busy={false} onAction={onAction} />);
    expect(container.textContent).toContain('cargo test'); expect(container.textContent).toContain('/workspace');
    act(() => (container.querySelector('.request-actions button') as HTMLButtonElement).click());
    expect(onAction).toHaveBeenCalledWith(request, { type:'approval', decision:'accept' });
  });

  it('composer exposes epoch-scoped slash commands and interrupt', () => {
    const onInterrupt = vi.fn(); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,activeTurnId:'turn-1',
      slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false}],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={vi.fn()} onInterrupt={onInterrupt} />);
    expect(container.textContent).toContain('在当前 Turn 中追加指令');
    act(() => (container.querySelector('[aria-label="停止当前 Turn"]') as HTMLButtonElement).click());
    expect(onInterrupt).toHaveBeenCalledOnce();
  });

  it('uses keyboard-first Slash selection and executes an exact no-argument command', async () => {
    const onSend = vi.fn(() => true); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,
      slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false},
        {name:'/model',capability:'thread.settings.model',interactionRequiredWithoutArgument:true}],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = '/st'; textarea.dispatchEvent(new Event('input', { bubbles:true })); });
    expect(container.querySelector('[role="listbox"]')).not.toBeNull();
    await act(async () => { textarea.dispatchEvent(new KeyboardEvent('keydown', { key:'Enter', bubbles:true, cancelable:true })); });
    expect((container.querySelector('textarea') as HTMLTextAreaElement).value).toBe('/status');
    await act(async () => { textarea.dispatchEvent(new KeyboardEvent('keydown', { key:'Enter', bubbles:true, cancelable:true })); });
    expect(onSend).toHaveBeenCalledWith('/status', []);
  });

  it('keeps a failed draft, focuses it for retry, and removes individual image previews', async () => {
    const onSend = vi.fn(async () => false); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = 'retry me'; textarea.dispatchEvent(new Event('input', { bubbles:true })); });
    const image = new File(['png'], 'proof.png', { type:'image/png' });
    const picker = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    Object.defineProperty(picker, 'files', { configurable:true, value:[image] });
    act(() => { picker.dispatchEvent(new Event('change', { bubbles:true })); });
    act(() => (container.querySelector('[aria-label="移除图片 proof.png"]') as HTMLButtonElement).click());
    expect(container.querySelector('.image-preview')).toBeNull();
    await act(async () => (container.querySelector('[aria-label="发送消息"]') as HTMLButtonElement).click());
    expect((container.querySelector('textarea') as HTMLTextAreaElement).value).toBe('retry me');
    expect(document.activeElement).toBe(textarea);
  });

  it('keeps IME composition and explicit newline shortcuts from submitting', () => {
    const onSend = vi.fn(); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value='中文输入'; textarea.dispatchEvent(new Event('input',{bubbles:true})); });
    const composing = new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true,isComposing:true});
    act(() => { textarea.dispatchEvent(composing); });
    act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',altKey:true,bubbles:true,cancelable:true})); });
    act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',shiftKey:true,bubbles:true,cancelable:true})); });
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toBe('中文输入');
  });

  it('dismisses command UI before Escape interrupts and keeps the active draft available', () => {
    const onInterrupt = vi.fn(); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,activeTurnId:'turn-1',
      slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false}],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={vi.fn()} onInterrupt={onInterrupt} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value='/st'; textarea.dispatchEvent(new Event('input',{bubbles:true})); });
    expect(container.querySelector('[role="listbox"]')).not.toBeNull();
    act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true})); });
    expect(container.querySelector('[role="listbox"]')).toBeNull(); expect(onInterrupt).not.toHaveBeenCalled();
    act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true})); });
    expect(onInterrupt).toHaveBeenCalledOnce(); expect(textarea.value).toBe('/st');
  });

  it('keeps stop available beside steer and recalls accepted prompts without crossing multiline cursor movement', () => {
    const onSend = vi.fn(() => true); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,activeTurnId:'turn-1',
      slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    for (const value of ['first prompt','second prompt']) {
      act(() => { textarea.value=value; textarea.dispatchEvent(new Event('input',{bubbles:true})); });
      act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true,cancelable:true})); });
    }
    expect(container.querySelector('[aria-label="停止当前 Turn"]')).not.toBeNull();
    act(() => { textarea.setSelectionRange(0,0); textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowUp',bubbles:true,cancelable:true})); });
    expect(textarea.value).toBe('second prompt');
    act(() => { textarea.setSelectionRange(0,0); textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowUp',bubbles:true,cancelable:true})); });
    expect(textarea.value).toBe('first prompt');
    act(() => { textarea.setSelectionRange(textarea.value.length,textarea.value.length);
      textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true})); });
    expect(textarea.value).toBe('second prompt');
    act(() => { textarea.value='fresh draft'; textarea.dispatchEvent(new Event('input',{bubbles:true})); });
    act(() => { textarea.setSelectionRange(11,11); textarea.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',bubbles:true,cancelable:true})); });
    expect(textarea.value).toBe('fresh draft');
    expect(canNavigateComposerHistory('line one\nline two', 10, 10, 'previous')).toBe(false);
    expect(canNavigateComposerHistory('line one\nline two', 3, 3, 'previous')).toBe(true);
  });

  it('accepts supported clipboard images and autosizes the composer within its visual bounds', () => {
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={vi.fn()} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    const files = [
      new File(['text'],'ignored.txt',{type:'text/plain'}),
      new File(['png'],'clipboard.png',{type:'image/png'}),
      new File(['jpg'],'clipboard.jpg',{type:'image/jpeg'}),
      new File(['webp'],'clipboard.webp',{type:'image/webp'}),
      new File(['gif'],'clipboard.gif',{type:'image/gif'}),
      new File(['extra'],'fifth.png',{type:'image/png'})
    ];
    const paste = new Event('paste',{bubbles:true,cancelable:true});
    Object.defineProperty(paste,'clipboardData',{value:{files}});
    act(() => { textarea.dispatchEvent(paste); });
    expect(container.querySelectorAll('.image-preview > span')).toHaveLength(4);
    expect(container.querySelector('.image-preview')?.textContent).toContain('clipboard.png');
    expect(container.querySelector('.image-preview')?.textContent).toContain('clipboard.jpg');
    expect(container.querySelector('.image-preview')?.textContent).toContain('clipboard.webp');
    expect(container.querySelector('.image-preview')?.textContent).toContain('clipboard.gif');
    expect(container.querySelector('.image-preview')?.textContent).not.toContain('ignored.txt');
    expect(container.querySelector('.image-preview')?.textContent).not.toContain('fifth.png');
    Object.defineProperty(textarea,'scrollHeight',{configurable:true,value:260}); resizeComposerTextarea(textarea);
    expect(textarea.style.height).toBe('220px'); expect(textarea.style.overflowY).toBe('auto');
    Object.defineProperty(textarea,'scrollHeight',{configurable:true,value:82}); resizeComposerTextarea(textarea);
    expect(textarea.style.height).toBe('82px'); expect(textarea.style.overflowY).toBe('hidden');
  });

  it('follows recent conversation updates without stealing an older reading position', () => {
    expect(shouldFollowLatestMessage(0)).toBe(true);
    expect(shouldFollowLatestMessage(119)).toBe(true);
    expect(shouldFollowLatestMessage(121)).toBe(false);
  });

  it('makes current-session control state explicit without treating stale history as live', () => {
    const active = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,activeTurnId:'turn-1',slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    expect(currentSessionPresence(thread, active)).toMatchObject({ label:'正在处理', tone:'live' });
    expect(currentSessionPresence(thread, { ...active, activeTurnId:undefined })).toMatchObject({ label:'可继续', tone:'ready' });
    expect(currentSessionPresence(thread, undefined, 'source 离线')).toEqual({ label:'仅浏览', tone:'warning', detail:'source 离线' });
    expect(currentSessionPresence({ ...thread, archived:true }, active)).toMatchObject({ label:'已归档', tone:'muted' });
  });

  it('shows unseen progress in a direct return-to-latest action', () => {
    const onJump = vi.fn();
    const container = mount(<ConversationFollowButton unseenUpdates={3} onJump={onJump} />);
    expect(container.textContent).toContain('回到最新 · 3 条新进展');
    expect(container.querySelector('.icon-down')).not.toBeNull();
    act(() => (container.querySelector('button') as HTMLButtonElement).click());
    expect(onJump).toHaveBeenCalledOnce();
  });

  it('composer previews and sends text with bounded local images', () => {
    const onSend = vi.fn(); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = 'hello image'; textarea.dispatchEvent(new Event('input', { bubbles: true })); });
    const image = new File(['png'], 'proof.png', { type: 'image/png' });
    const picker = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    Object.defineProperty(picker, 'files', { configurable: true, value: [image] });
    act(() => { picker.dispatchEvent(new Event('change', { bubbles: true })); });
    expect(container.querySelector('.image-preview')?.textContent).toContain('proof.png');
    const send = container.querySelector('[aria-label="发送消息"]')!;
    act(() => { send.dispatchEvent(new MouseEvent('click', { bubbles:true })); });
    expect(onSend).toHaveBeenCalledWith('hello image', [image]);
    expect((container.querySelector('textarea') as HTMLTextAreaElement).value).toBe('');
    expect(container.querySelector('.image-preview')).toBeNull();
  });

  it('shows optimistic user input and removes it only after matching projected reconciliation', () => {
    const message = { threadKey:'thread-1',clientUserMessageId:'client-message-1',text:'optimistic text',imageCount:1,
      createdAtMs:1,state:'accepted' as const };
    const container = mount(<OptimisticMessageCard message={message} />);
    expect(container.textContent).toContain('optimistic text');
    expect(container.textContent).toContain('已提交，等待投影');
    expect(container.textContent).toContain('1 张本地图片');

    const unrelated = { turnScope:'turn-1',itemId:'client-message-1',itemType:'agent_message',status:'completed',
      raw:{},provenance:{},lastEventSeq:1 } as Item;
    expect(reconcileOptimisticMessages([message], 'thread-1', [unrelated])).toHaveLength(1);
    const projected = { ...unrelated, itemId:'server-item', itemType:'user_message', raw:{payload:{clientUserMessageId:'client-message-1'}} } as Item;
    expect(reconcileOptimisticMessages([message], 'thread-1', [projected])).toHaveLength(0);
    const rolloutProjected = { ...unrelated, itemId:'rollout-user', itemType:'user_message', raw:{type:'user_message',client_id:'client-message-1'} } as Item;
    expect(reconcileOptimisticMessages([message], 'thread-1', [rolloutProjected])).toHaveLength(0);
    const liveProjected = { ...unrelated, itemId:'live-user', itemType:'user_message', raw:{type:'userMessage',clientId:'client-message-1'} } as Item;
    expect(reconcileOptimisticMessages([message], 'thread-1', [liveProjected])).toHaveLength(0);
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

  it('renders local slash results as a Gateway status card instead of a model message', () => {
    const container = mount(<GatewayStatusCardView card={{kind:'gatewayStatusCard',cardType:'status',sourceId:'source-1234',
      sourceEpoch:'epoch-1234',threadKey:'thread',content:{threadLoaded:true,activeTurnId:null}}} />);
    expect(container.textContent).toContain('Gateway /status');
    expect(container.textContent).toContain('查看状态');
    expect(container.querySelector('article')).toBeNull();
  });

  it('slash palette selects only advertised commands and offline state disables mutation', () => {
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,
      slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false},
        {name:'/steer',capability:'turn.steer',interactionRequiredWithoutArgument:false}] } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={vi.fn()} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = '/sta'; textarea.dispatchEvent(new Event('input', { bubbles: true })); });
    expect(container.querySelectorAll('.slash-palette button')).toHaveLength(1);
    act(() => (container.querySelector('.slash-palette button') as HTMLButtonElement).click());
    expect((container.querySelector('textarea') as HTMLTextAreaElement).value).toBe('/status');

    const offline = mount(<Composer catalog={undefined} disabledReason="source 离线或 epoch 已变化" busy={false}
      onSend={vi.fn()} onInterrupt={vi.fn()} />);
    expect(offline.textContent).toContain('epoch 已变化');
    expect((offline.querySelector('textarea') as HTMLTextAreaElement).disabled).toBe(true);
    expect((offline.querySelector('input[type="file"]') as HTMLInputElement).disabled).toBe(true);
  });

  it('opens structured Slash pickers and dispatches an exact selected model', () => {
    const onSend = vi.fn();
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,
      slashCommands:[
        {name:'/model',capability:'thread.settings.model',interactionRequiredWithoutArgument:true},
        {name:'/reasoning',capability:'thread.settings.reasoning',interactionRequiredWithoutArgument:true},
        {name:'/personality',capability:'thread.settings.personality',interactionRequiredWithoutArgument:true},
        {name:'/permissions',capability:'thread.settings.permissions',interactionRequiredWithoutArgument:true}
      ],capabilities:{entries:{
        'model/list':{available:true,experimental:false,data:{data:[
          {id:'model-a',displayName:'Model A',supportsPersonality:true,supportedReasoningEfforts:[
            {reasoningEffort:'low',label:'Low'},{reasoningEffort:'high',label:'High'}]},
          {id:'model-b',displayName:'Model B'}]}},
        'permissionProfile/list':{available:true,experimental:false,data:{data:[{id:'workspace',displayName:'Workspace'}]}}
      }}} as ControlCatalog;
    const controlledThread = { ...thread, context:{...thread.context,runtime:{model:'model-a',reasoningEffort:'low',activePermissionProfile:{id:'workspace'}}} };
    const container = mount(<Composer catalog={catalog} thread={controlledThread} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = '/model'; textarea.dispatchEvent(new Event('input', { bubbles:true })); });
    expect(container.querySelector('[role="listbox"]')?.getAttribute('aria-label')).toBe('选择 Model');
    expect(container.querySelectorAll('[role="option"]')).toHaveLength(2);
    expect(container.querySelector('[role="option"][aria-selected="true"]')?.textContent).toBe('Model A');
    act(() => { textarea.dispatchEvent(new KeyboardEvent('keydown', { key:'Enter', bubbles:true })); });
    expect(onSend).toHaveBeenCalledWith('/model model-a', []);
    act(() => { textarea.value = '/model'; textarea.dispatchEvent(new Event('input', { bubbles:true })); });
    act(() => (Array.from(container.querySelectorAll('[role="option"]')).find((option) => option.textContent === 'Model B') as HTMLButtonElement).click());
    expect(onSend).toHaveBeenCalledWith('/model model-b', []);
    expect(textarea.value).toBe('');
    for (const [command, label, value] of [
      ['/reasoning','High','/reasoning high'],
      ['/personality','Pragmatic','/personality pragmatic'],
      ['/permissions','Workspace','/permissions workspace']
    ]) {
      act(() => { textarea.value = command; textarea.dispatchEvent(new Event('input', { bubbles:true })); });
      act(() => (Array.from(container.querySelectorAll('[role="option"]')).find((option) => option.textContent === label) as HTMLButtonElement).click());
      expect(onSend).toHaveBeenLastCalledWith(value, []);
    }
  });

  it('keeps the matching unloaded catalog and exposes the exact resume action', () => {
    const unrelated = { sourceId:'wrong-source',sourceEpoch:'wrong-epoch',threadLoaded:false,
      slashCommands:[{name:'/new',capability:'thread.start',interactionRequiredWithoutArgument:true}] } as ControlCatalog;
    const resumable = { sourceId:'matching-source',sourceEpoch:'exact-epoch',threadLoaded:false,
      slashCommands:[{name:'/new',capability:'thread.start',interactionRequiredWithoutArgument:true},
        {name:'/resume',capability:'thread.resume',interactionRequiredWithoutArgument:false}] } as ControlCatalog;
    expect(selectThreadControlCatalog([unrelated, resumable])).toBe(resumable);

    const onSend = vi.fn();
    const container = mount(<Composer catalog={resumable} disabledReason="当前 Thread 尚未加载到 App Server"
      busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const resume = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '加载到当前 source') as HTMLButtonElement;
    expect(resume.disabled).toBe(false);
    expect((container.querySelector('textarea') as HTMLTextAreaElement).disabled).toBe(true);
    act(() => resume.click());
    expect(onSend).toHaveBeenCalledWith('/resume', []);
  });

  it('emits closed user-question and MCP elicitation payloads', () => {
    const onAction = vi.fn();
    const question = { requestKey:'q',sourceId:'source',sourceEpoch:'epoch',requestId:'q',requestType:'user_input',state:'pending',
      requestVersion:2,payload:{questions:[{id:'first',header:'First'},{id:'second',header:'Second'}]},requestEventSeq:1 } as PendingRequest;
    const questionCard = mount(<PendingRequestCard request={question} busy={false} onAction={onAction} />);
    const questionInput = questionCard.querySelector('textarea')!;
    act(() => { questionInput.value = 'alpha\nbeta'; questionInput.dispatchEvent(new Event('input', { bubbles: true })); });
    act(() => (Array.from(questionCard.querySelectorAll('button')).find((button) => button.textContent === '提交回答') as HTMLButtonElement).click());
    expect(onAction).toHaveBeenLastCalledWith(question, { type:'userInput', answers:{ first:['alpha'], second:['beta'] } });

    const mcp = { ...question, requestKey:'mcp',requestId:'mcp',requestType:'mcp_elicitation',
      payload:{requestedSchema:{type:'object',properties:{name:{type:'string'}}}} } as PendingRequest;
    const mcpCard = mount(<PendingRequestCard request={mcp} busy={false} onAction={onAction} />);
    const mcpInput = mcpCard.querySelector('textarea')!;
    act(() => { mcpInput.value = '{"name":"safe"}'; mcpInput.dispatchEvent(new Event('input', { bubbles: true })); });
    act(() => (Array.from(mcpCard.querySelectorAll('button')).find((button) => button.textContent === '提交表单') as HTMLButtonElement).click());
    expect(onAction).toHaveBeenLastCalledWith(mcp, { type:'mcpElicitation', action:'accept', content:{name:'safe'} });
  });

  it('never presents outcome_unknown as success or ordinary failure', () => {
    expect(commandNotice({ commandId:'command',state:'outcome_unknown' })).toContain('结果未知');
    expect(commandNotice({ commandId:'command',state:'outcome_unknown' })).toContain('不会自动重放');
    expect(commandNotice({ commandId:'command',state:'completed' })).toBe('控制操作已完成');
    expect(commandNotice({ commandId:'goal',capability:'thread.goal.get',state:'completed',result:{goalPresent:false,goalStatus:null} }))
      .toBe('当前会话没有 Goal');
  });

  it('renders operation feedback as contextual success, warning, or error notices', () => {
    const onClose = vi.fn();
    const success = mount(<FeedbackNotice message="控制操作已完成" onClose={onClose} />);
    expect(success.firstElementChild?.classList.contains('notice-success')).toBe(true);
    expect(success.firstElementChild?.getAttribute('role')).toBe('status');
    act(() => (success.querySelector('button') as HTMLButtonElement).click());
    expect(onClose).toHaveBeenCalledOnce();

    const warning = mount(<FeedbackNotice message="操作结果未知：请刷新状态，系统不会自动重放" onClose={vi.fn()} />);
    expect(warning.firstElementChild?.classList.contains('notice-warning')).toBe(true);
    const error = mount(<FeedbackNotice message="控制失败：source 已断开" onClose={vi.fn()} />);
    expect(error.firstElementChild?.classList.contains('notice-error')).toBe(true);
  });

  it('renders capability-gated settings and Plan/Goal controls', () => {
    const onSetting = vi.fn(); const onSlash = vi.fn();
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,collaborationMode:{mode:'default'},goal:{status:'active'},
      slashCommands:[{name:'/plan',capability:'thread.plan',interactionRequiredWithoutArgument:false}],capabilities:{entries:{
        'model/list':{available:true,experimental:false,data:{data:[{id:'model-a',displayName:'Model A',supportsPersonality:true,
          supportedReasoningEfforts:[{reasoningEffort:'low',label:'Low'},{reasoningEffort:'high',label:'High'}]},
          {id:'hidden',displayName:'Hidden',hidden:true}]}},
        'permissionProfile/list':{available:true,experimental:false,data:{data:[{id:'workspace',displayName:'Workspace'}]}}
      }}} as ControlCatalog;
    const controlledThread = { ...thread, context:{ ...thread.context,runtime:{model:'model-a',reasoningEffort:'low',activePermissionProfile:{id:'workspace'}} } };
    const container = mount(<ControlSettings thread={controlledThread} catalog={catalog} busy={false} onSetting={onSetting} onSlash={onSlash} />);
    expect(container.querySelectorAll('select[aria-label="Model"] option')).toHaveLength(1);
    const reasoning = container.querySelector<HTMLSelectElement>('select[aria-label="Reasoning"]')!;
    act(() => { reasoning.value = 'high'; reasoning.dispatchEvent(new Event('change', { bubbles:true })); });
    expect(onSetting).toHaveBeenCalledWith('thread.settings.reasoning', 'high');
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '进入 Plan') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/plan');
    const objective = container.querySelector<HTMLInputElement>('input[aria-label="Goal objective"]')!;
    act(() => { objective.value = 'ship safely'; objective.dispatchEvent(new Event('input', { bubbles:true })); });
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '设置') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/goal ship safely');
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '暂停') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/goal pause');

    const planCatalog = { ...catalog, collaborationMode:{mode:'plan'} } as ControlCatalog;
    const plan = mount(<ControlSettings thread={controlledThread} catalog={planCatalog} busy={false} onSetting={onSetting} onSlash={onSlash} />);
    act(() => (Array.from(plan.querySelectorAll('button')).find((button) => button.textContent === '退出 Plan') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/plan off');
  });

  it('shows the active Goal persistently with real pause and clear actions', () => {
    const onSlash = vi.fn();
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,
      goal:{objective:'完成可操作的会话闭环',status:'active',tokensUsed:1200,timeUsedSeconds:42},slashCommands:[],capabilities:{entries:{}} } as ControlCatalog;
    const container = mount(<GoalStatusBar catalog={catalog} busy={false} onSlash={onSlash} />);
    expect(container.textContent).toContain('完成可操作的会话闭环');
    expect(container.textContent).toContain('1,200 tokens');
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '暂停') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/goal pause');
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '清除') as HTMLButtonElement).click());
    expect(onSlash).toHaveBeenCalledWith('/goal clear');
  });

  it('maps /clear to a fresh Thread using the current source, cwd, model, personality, and permissions', () => {
    const controlledThread = { ...thread, cwdDisplay:'/fallback',model:'fallback-model',context:{...thread.context,runtime:{
      cwd:'/workspace/project',model:'model-a',activePermissionProfile:{id:'workspace'}
    }}} as Thread;
    const turns = [{turnId:'turn',status:'completed',captureCompleteness:'durable_complete',completenessReasons:[],coverage:{},raw:{},lastEventSeq:1,
      context:{model:'model-a',personality:'pragmatic',permissionProfile:{id:'workspace'}}}] as Turn[];
    const catalog = {sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[],capabilities:{entries:{}}} as ControlCatalog;
    expect(clearThreadInput(controlledThread, turns, catalog)).toEqual({
      sourceId:'source',sourceEpoch:'epoch',cwd:'/workspace/project',model:'model-a',personality:'pragmatic',permissions:'workspace'
    });
    expect(clearThreadInput({...controlledThread,context:{...controlledThread.context,runtime:{}}}, [], catalog)).toEqual({
      sourceId:'source',sourceEpoch:'epoch',cwd:'/fallback',model:'fallback-model',personality:null,permissions:null
    });
  });

  it('creates a new Thread only from a ready exact-epoch source and absolute cwd', () => {
    const onCreate = vi.fn(); const onSource = vi.fn();
    const catalog = { sourceId:'source',sourceEpoch:'epoch-exact',threadLoaded:false,slashCommands:[],capabilities:{entries:{
      'model/list':{available:true,experimental:false,data:{data:[{id:'model-a',displayName:'Model A'}]}},
      'permissionProfile/list':{available:true,experimental:false,data:{data:[{id:'workspace',displayName:'Workspace'}]}}
    }}} as ControlCatalog;
    const sources = [{sourceId:'source',sourceEpoch:'epoch-exact',state:'ready'}] as ControllerSource[];
    const container = mount(<NewThreadDialog sources={sources} catalog={catalog} busy={false} onSource={onSource}
      onClose={vi.fn()} onCreate={onCreate} />);
    const create = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '创建 Thread') as HTMLButtonElement;
    const cwd = container.querySelector<HTMLInputElement>('input[aria-label="cwd"]')!;
    act(() => { cwd.value = 'relative'; cwd.dispatchEvent(new Event('input', { bubbles:true })); });
    expect(create.disabled).toBe(true);
    act(() => { cwd.value = '/workspace/project'; cwd.dispatchEvent(new Event('input', { bubbles:true })); });
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="New thread model"]')!;
    const permissions = container.querySelector<HTMLSelectElement>('select[aria-label="New thread permissions"]')!;
    act(() => { model.value='model-a'; model.dispatchEvent(new Event('change',{bubbles:true}));
      permissions.value='workspace'; permissions.dispatchEvent(new Event('change',{bubbles:true})); });
    act(() => create.click());
    expect(onCreate).toHaveBeenCalledWith({sourceId:'source',sourceEpoch:'epoch-exact',cwd:'/workspace/project',model:'model-a',personality:null,permissions:'workspace'});
  });
});
