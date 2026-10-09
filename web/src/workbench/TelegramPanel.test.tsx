import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { TelegramPanel } from './TelegramPanel';
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const instanceId = id(1); const root = document.createElement('div');
const candidate = { generation: id(2), botId: 10, userId: 20, chatId: 20 };
function status(phase = 'idle', interactiveConsent = false) {
  return { instanceId, channel: { binding: { phase, generation: id(2), botId: 10, candidate: phase === 'awaitingConfirmation' ? { ...candidate } : null, active: phase === 'active' ? { ...candidate, target: { runEpoch: id(3), threadId: id(4), transportEpoch: id(5), subscriptionGeneration: id(6) }, interactiveConsent } : null }, configured: true, configuring: false, queued: 0, recovered: { received: 1, submitting: 2, closed: 3 }, lastTask: null as null | { id: string; phase: string; nativeConfirmed: boolean; nativeCompleted: boolean; delivery: string; reason: string | null }, failure: null as string | null } };
}
const runs = () => ({ instanceId, telegramImplemented: true, runs: [{ runId: id(3), supportsChannel: true, source: { revision: id(7), transportEpoch: id(5), phase: 'observing', targets: [{ threadId: id(4), busy: true }], failure: null } }] });
const ok = (value: unknown) => ({ ok: true, status: 200, json: async () => value });
const button = (label: string) => [...root.querySelectorAll('button')].find(b => b.textContent === label)!;
const checkboxes = () => [...root.querySelectorAll('input[type=checkbox]')] as HTMLInputElement[];
async function click(label: string) { await act(async () => button(label).click()); await act(async () => {}); }
async function show(open = true) { await act(async () => render(<TelegramPanel instanceId={instanceId} open={open} />, root)); for (let i = 0; i < 8; i++) await act(async () => { await Promise.resolve(); }); }
function mock(data = status()) {
  const fetch = vi.fn(async (url: string, init?: RequestInit) => ok(init?.method ? data : url.endsWith('channel-runs') ? runs() : data));
  vi.stubGlobal('fetch', fetch); return fetch;
}
beforeEach(() => document.body.append(root));
afterEach(() => { render(null, root); root.remove(); vi.useRealTimers(); vi.unstubAllGlobals(); });
it('only polls reads while open and stops on unmount', async () => {
  vi.useFakeTimers(); const fetch = mock(); await show(false); expect(fetch).not.toHaveBeenCalled();
  await show(); expect(fetch).toHaveBeenCalledTimes(2);
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); }); expect(fetch).toHaveBeenCalledTimes(4);
  expect(fetch.mock.calls.every(([, init]) => !init?.method)).toBe(true);
  await act(async () => render(null, root)); await vi.advanceTimersByTimeAsync(2000); expect(fetch).toHaveBeenCalledTimes(4);
});
it('clears password before response and never echoes token in errors or retries mutations', async () => {
  vi.useFakeTimers(); const secret = '123456:synthetic-secret-never-render'; let reject!: (reason: Error) => void;
  const fetch = vi.fn((url: string, init?: RequestInit) => init?.method ? new Promise((_resolve, no) => { reject = no; }) : Promise.resolve(ok(url.endsWith('channel-runs') ? runs() : status())));
  vi.stubGlobal('fetch', fetch); await show();
  const input = root.querySelector('input[type=password]') as HTMLInputElement;
  expect(input.autocomplete).toBe('off');
  await act(async () => { input.value = secret; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  expect(input.value).toBe('');
  const write = fetch.mock.calls.find(([, init]) => init?.method)!;
  expect(JSON.parse(String(write[1]!.body))).toEqual({ instanceId, token: secret });
  await act(async () => reject(new Error(secret)));
  expect(root.textContent).not.toContain(secret); expect(root.innerHTML).not.toContain(secret);
  expect(root.textContent).toContain('操作结果尚未确认');
  const calls = fetch.mock.calls.length; await act(async () => { await vi.advanceTimersByTimeAsync(3000); }); expect(fetch).toHaveBeenCalledTimes(calls);
  expect(button('保存并验证 Bot').disabled).toBe(true);
  await click('刷新渠道状态'); expect(fetch.mock.calls.filter(([, init]) => init?.method)).toHaveLength(1);
});
it('requires exact candidate, explicit thread choice and shared-context consent without implying interaction consent', async () => {
  const fetch = mock(status('awaitingConfirmation')); await show();
  expect(root.textContent).toContain('用户 ID：20'); expect(root.textContent).toContain('可能引用本机历史内容');
  expect(root.textContent).toContain('文件路径及操作'); expect(root.textContent).toContain('原始工具输出');
  expect(button('确认并激活私聊').disabled).toBe(true);
  expect(checkboxes().map(box => box.checked)).toEqual([false, false]);
  expect(checkboxes().every(box => box.disabled)).toBe(true);
  await act(async () => { const select = root.querySelector('select')!; select.value = `${id(3)}/${id(4)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  expect(button('确认并激活私聊').disabled).toBe(true);
  expect(checkboxes().every(box => !box.disabled)).toBe(true);
  await act(async () => checkboxes()[0].click());
  expect(checkboxes().map(box => box.checked)).toEqual([true, false]);
  await click('确认并激活私聊');
  const call = fetch.mock.calls.find(([, init]) => init?.method)!;
  expect(call[0]).toBe('/workbench/v1/application/telegram/confirm');
  expect(JSON.parse(String(call[1]!.body))).toEqual({ instanceId, candidate, runId: id(3), threadId: id(4), sourceRevision: id(7), interactiveConsent: false });
});
it('sends interaction consent only after the separate checkbox is explicitly selected', async () => {
  const fetch = mock(status('awaitingConfirmation')); await show();
  await act(async () => { const select = root.querySelector('select')!; select.value = `${id(3)}/${id(4)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  await act(async () => checkboxes()[1].click());
  expect(checkboxes().map(box => box.checked)).toEqual([false, true]);
  expect(button('确认并激活私聊').disabled).toBe(true);
  await act(async () => checkboxes()[0].click());
  await click('确认并激活私聊');
  const call = fetch.mock.calls.find(([, init]) => init?.method)!;
  expect(JSON.parse(String(call[1]!.body))).toEqual({ instanceId, candidate, runId: id(3), threadId: id(4), sourceRevision: id(7), interactiveConsent: true });
});
it('displays only locally returned pairing command and never sends it automatically', async () => {
  const command = `/bind ${id(9)}`;
  const fetch = vi.fn(async (url: string, init?: RequestInit) => ok(init?.method ? { instanceId, pairing: { generation: id(2), command, expiresInSeconds: 600 } } : url.endsWith('channel-runs') ? runs() : status()));
  vi.stubGlobal('fetch', fetch); await show(); await click('生成私聊配对命令');
  expect(root.querySelector('code')?.textContent).toBe(command);
  expect(fetch.mock.calls.filter(([, init]) => init?.method)).toHaveLength(1);
  expect(JSON.parse(String(fetch.mock.calls.find(([, init]) => init?.method)![1]!.body))).toEqual({ instanceId });
});
it('does not optimistically revoke after a rejected response and hides untrusted error content', async () => {
  const fetch = vi.fn(async (url: string, init?: RequestInit) => init?.method ? { ok: false, status: 500, json: async () => ({ error: { code: '<img src=x onerror=alert(1)>', message: 'secret' } }) } : ok(url.endsWith('channel-runs') ? runs() : status('active')));
  vi.stubGlobal('fetch', fetch); await show(); await click('撤销渠道授权');
  expect(root.textContent).toContain('渠道状态：已激活'); expect(root.textContent).toContain('操作结果尚未确认');
  expect(root.textContent).not.toContain('已撤销渠道授权。'); expect(root.querySelector('img')).toBeNull(); expect(root.textContent).not.toContain('secret');
});
it('rejects stale instance and malformed identity without enabling mutations', async () => {
  const fetch = vi.fn(async (url: string) => ok(url.endsWith('channel-runs') ? runs() : { ...status(), instanceId: id(99) }));
  vi.stubGlobal('fetch', fetch); await show(); expect(root.textContent).toContain('应用已重新启动'); expect(button('刷新渠道状态').disabled).toBe(true);
  await act(async () => render(null, root));
  const malformed = status('awaitingConfirmation'); malformed.channel.binding.candidate!.userId = Number.MAX_SAFE_INTEGER + 1;
  mock(malformed); await show(); expect(root.textContent).toContain('格式无法识别'); expect(root.querySelector('select')).toBeNull();
});
it('handles unsupported APIs and separates native completion from unknown delivery', async () => {
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false, status: 404, json: async () => ({}) })));
  await show(); expect(root.textContent).toContain('当前版本尚未开放'); expect(root.querySelector('input[type=password]')).toBeNull();
  await act(async () => render(null, root));
  const data = status(); data.channel.lastTask = { id: id(8), phase: 'closed', nativeConfirmed: true, nativeCompleted: false, delivery: 'unknown', reason: '<script>secret</script>' }; data.channel.failure = '<img src=x>';
  mock(data); await show(); expect(root.textContent).toContain('未确认完成'); expect(root.textContent).toContain('投递结果未知'); expect(root.textContent).toContain('不会自动重放'); expect(root.querySelector('script,img')).toBeNull(); expect(root.textContent).not.toContain('secret');
});
it.each([
  ['channel_interaction_send_unknown', '提示投递结果未知，按钮未激活，请回本机回答'],
  ['channel_interaction_send_rejected', '提示未送达，请回本机回答'],
  ['channel_interaction_written_unconfirmed', '答复已写入原生通道，采纳结果尚未确认'],
  ['channel_interaction_withheld', '原生答复未发送，请回本机核对'],
  ['channel_interaction_write_unknown', '原生答复结果未知，不自动重试，请回本机核对'],
])('shows a fixed message for interaction reason %s', async (reason, expected) => {
  const data = status('active');
  data.channel.lastTask = { id: id(8), phase: 'closed', nativeConfirmed: true, nativeCompleted: false, delivery: 'withheld', reason };
  mock(data); await show();
  const task = root.querySelector('[aria-label="最近渠道任务"]')!;
  expect(task.textContent).toContain(expected);
  expect(task.textContent).not.toContain(reason);
});
it('uses the generic fallback for an unknown interaction reason without rendering its HTML', async () => {
  const reason = '<img src=x onerror=alert(1)>secret';
  const data = status('active');
  data.channel.lastTask = { id: id(8), phase: 'closed', nativeConfirmed: true, nativeCompleted: false, delivery: 'withheld', reason };
  mock(data); await show();
  const task = root.querySelector('[aria-label="最近渠道任务"]')!;
  expect(task.textContent).toContain('任务需要本机核对；未自动重试。');
  expect(task.textContent).not.toContain('secret');
  expect(task.querySelector('img')).toBeNull();
});
it('invalidates selection when source revision changes instead of silently updating consent', async () => {
  vi.useFakeTimers(); let source = runs(); const fetch = vi.fn(async (url: string) => ok(url.endsWith('channel-runs') ? source : status('awaitingConfirmation')));
  vi.stubGlobal('fetch', fetch); await show();
  await act(async () => { const select = root.querySelector('select')!; select.value = `${id(3)}/${id(4)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  await act(async () => { checkboxes()[0].click(); checkboxes()[1].click(); }); expect(button('确认并激活私聊').disabled).toBe(false);
  source = runs(); source.runs[0].source.revision = id(90); await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
  expect(button('确认并激活私聊').disabled).toBe(true); expect(checkboxes().map(box => box.checked)).toEqual([false, false]);
});

it('loads saved credentials only on explicit click and omits token', async () => {
  const fetch=mock();await show();expect(fetch.mock.calls.every(([,init])=>!init?.method)).toBe(true);
  await click('加载已保存凭证并验证');const write=fetch.mock.calls.find(([,init])=>init?.method)!;
  expect(write[0]).toBe('/workbench/v1/application/telegram/configure');expect(JSON.parse(String(write[1]!.body))).toEqual({instanceId});
});
it('bounds a missing mutation ACK and requires manual state refresh', async () => {
  vi.useFakeTimers();
  const fetch=vi.fn((url:string,init?:RequestInit)=>init?.method ? new Promise((_resolve,reject)=>init.signal!.addEventListener('abort',()=>reject(new Error('private transport detail')))) : Promise.resolve(ok(url.endsWith('channel-runs')?runs():status('active'))));
  vi.stubGlobal('fetch',fetch);await show();await act(async()=>button('撤销渠道授权').click());
  await act(async()=>{await vi.advanceTimersByTimeAsync(15000);});expect(root.textContent).toContain('操作结果尚未确认');expect(root.textContent).not.toContain('private transport detail');
  const writes=fetch.mock.calls.filter(([,init])=>init?.method);expect(writes).toHaveLength(1);expect(button('撤销渠道授权').disabled).toBe(true);
});

it('binds local consent to the exact candidate identity across polling', async () => {
  vi.useFakeTimers();let current=status('awaitingConfirmation');
  vi.stubGlobal('fetch',vi.fn(async(url:string)=>ok(url.endsWith('channel-runs')?runs():current)));await show();
  await act(async()=>{const select=root.querySelector('select')!;select.value=`${id(3)}/${id(4)}`;select.dispatchEvent(new Event('change',{bubbles:true}));});
  await act(async()=>{checkboxes()[0].click();checkboxes()[1].click();});expect(button('确认并激活私聊').disabled).toBe(false);
  current=status('awaitingConfirmation');current.channel.binding.candidate!.userId=99;current.channel.binding.candidate!.chatId=99;
  await act(async()=>{await vi.advanceTimersByTimeAsync(1000);});expect(button('确认并激活私聊').disabled).toBe(true);expect(checkboxes().map(box => box.checked)).toEqual([false, false]);
});
it('clears both permissions when the target changes and does not restore them when switching back', async () => {
  const source = runs(); source.runs[0].source.targets.push({ threadId: id(8), busy: false });
  vi.stubGlobal('fetch', vi.fn(async (url: string) => ok(url.endsWith('channel-runs') ? source : status('awaitingConfirmation'))));
  await show();
  const select = root.querySelector('select')!;
  await act(async () => { select.value = `${id(3)}/${id(4)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  await act(async () => checkboxes()[0].click()); await act(async () => checkboxes()[1].click());
  expect(checkboxes().map(box => box.checked)).toEqual([true, true]);
  await act(async () => { select.value = `${id(3)}/${id(8)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  expect(checkboxes().map(box => box.checked)).toEqual([false, false]);
  await act(async () => { select.value = `${id(3)}/${id(4)}`; select.dispatchEvent(new Event('change', { bubbles: true })); });
  expect(checkboxes().map(box => box.checked)).toEqual([false, false]);
  expect(button('确认并激活私聊').disabled).toBe(true);
});
it('shows the active interaction scope from the server snapshot and rejects an absent flag', async () => {
  mock(status('active')); await show();
  expect(root.textContent).toContain('必要交互：未授权'); expect(root.textContent).toContain('审批和补充问题请回本机 Codex');
  await act(async () => render(null, root));
  mock(status('active', true)); await show();
  expect(root.textContent).toContain('必要交互：已授权'); expect(root.textContent).toContain('普通单次命令审批'); expect(root.textContent).toContain('非秘密选项问题');
  expect(root.textContent).not.toContain('整轮取消');
  await act(async () => render(null, root));
  const malformed = status('active', true); delete (malformed.channel.binding.active as Record<string, unknown>).interactiveConsent;
  mock(malformed); await show();
  expect(root.textContent).toContain('格式无法识别'); expect(root.textContent).not.toContain('必要交互：已授权');
});

it('explicitly loads a saved credential after restart when no client is configured', async () => {
  const current=status('unconfigured');current.channel.configured=false;current.channel.binding.botId=null as unknown as number;
  const fetch=mock(current);await show();
  expect(button('加载已保存凭证并验证').disabled).toBe(false);await click('加载已保存凭证并验证');
  const write=fetch.mock.calls.find(([,init])=>init?.method)!;
  expect(write[0]).toBe('/workbench/v1/application/telegram/configure');expect(JSON.parse(String(write[1]!.body))).toEqual({instanceId});
});


it('allows forgetting saved credentials after restart without loading or sending a token', async () => {
  const current = status('unconfigured'); current.channel.configured = false;
  const fetch = mock(current); await show();
  expect(button('移除保存的 Bot 凭证').disabled).toBe(false);
  await click('移除保存的 Bot 凭证');
  const writes = fetch.mock.calls.filter(([, init]) => init?.method);
  expect(writes).toHaveLength(1);
  expect(writes[0][0]).toBe('/workbench/v1/application/telegram/forget');
  expect(JSON.parse(String(writes[0][1]!.body))).toEqual({ instanceId });
});

it.each([
  ['waiting', 'pending', '等待原生会话可提交', '等待投递'],
  ['submitting', 'pending', '正在提交', '等待投递'],
  ['running', 'pending', '原生执行中', '等待投递'],
  ['abandoned', 'withheld', '任务已停止等待', '未满足外发条件'],
  ['uncertain', 'withheld', '原生提交结果未知，不自动重试，请在本机核对', '未满足外发条件'],
  ['rejected', 'withheld', '原生提交被拒绝或未发送', '未满足外发条件'],
  ['withheld', 'withheld', '回复已保留在本机', '未满足外发条件'],
  ['completed', 'confirmed', '处理结束', '已投递'],
  ['completed', 'partialUnknown', '处理结束', '部分投递，后续结果未知，不自动补发'],
  ['completed', 'noReply', '处理结束', '无可发送的最终回复'],
  ['completed', 'unknown', '处理结束', '投递结果未知，不自动重发'],
  ['completed', 'partial', '处理结束', '部分投递，不自动补发'],
  ['completed', 'rejected', '处理结束', '投递被拒绝'],
  ['futurePhase', 'futureDelivery', '任务状态暂无法识别', '投递状态暂无法识别'],
])('renders actual Service snapshot %s/%s without conflating delivery', async (phase, delivery, phaseLabel, deliveryLabel) => {
  const current = status('active');
  current.channel.lastTask = { id: id(8), phase, delivery, nativeConfirmed: phase === 'completed', nativeCompleted: phase === 'completed', reason: null };
  mock(current); await show();
  const task = root.querySelector('[aria-label="最近渠道任务"]')!;
  expect(task.textContent).toContain(phaseLabel);
  expect(task.textContent).toContain(deliveryLabel);
  expect(task.textContent).not.toContain('futurePhase');
  expect(task.textContent).not.toContain('futureDelivery');
});
