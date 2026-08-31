import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { Composer, ControlSettings, ItemCard, NewThreadDialog, OptimisticMessageCard, PendingRequestCard, Sidebar, TurnSection, commandNotice, itemRendererKind, reconcileOptimisticMessages, threadDisplay } from './App';
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
      slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false}] } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={vi.fn()} onInterrupt={onInterrupt} />);
    expect(container.textContent).toContain('活动 Turn');
    act(() => (Array.from(container.querySelectorAll('button')).find((button) => button.textContent === 'Interrupt') as HTMLButtonElement).click());
    expect(onInterrupt).toHaveBeenCalledOnce();
  });

  it('composer previews and sends text with bounded local images', () => {
    const onSend = vi.fn(); const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,slashCommands:[] } as ControlCatalog;
    const container = mount(<Composer catalog={catalog} busy={false} onSend={onSend} onInterrupt={vi.fn()} />);
    const textarea = container.querySelector('textarea')!;
    act(() => { textarea.value = 'hello image'; textarea.dispatchEvent(new Event('input', { bubbles: true })); });
    const image = new File(['png'], 'proof.png', { type: 'image/png' });
    const picker = container.querySelector<HTMLInputElement>('input[type="file"]')!;
    Object.defineProperty(picker, 'files', { configurable: true, value: [image] });
    act(() => picker.dispatchEvent(new Event('change', { bubbles: true })));
    expect(container.querySelector('.image-preview')?.textContent).toContain('proof.png');
    const send = Array.from(container.querySelectorAll('button')).find((button) => button.textContent === '发送')!;
    act(() => send.click());
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
    expect((container.querySelector('textarea') as HTMLTextAreaElement).value).toBe('/status ');

    const offline = mount(<Composer catalog={undefined} disabledReason="source 离线或 epoch 已变化" busy={false}
      onSend={vi.fn()} onInterrupt={vi.fn()} />);
    expect(offline.textContent).toContain('epoch 已变化');
    expect((offline.querySelector('textarea') as HTMLTextAreaElement).disabled).toBe(true);
    expect((offline.querySelector('input[type="file"]') as HTMLInputElement).disabled).toBe(true);
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
    expect(commandNotice({ commandId:'command',state:'completed' })).toBe('控制操作：completed');
  });

  it('renders capability-gated settings and Plan/Goal controls', () => {
    const onSetting = vi.fn(); const onSlash = vi.fn();
    const catalog = { sourceId:'source',sourceEpoch:'epoch',threadLoaded:true,collaborationMode:{mode:'default'},goal:{status:'active'},
      slashCommands:[],capabilities:{entries:{
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
