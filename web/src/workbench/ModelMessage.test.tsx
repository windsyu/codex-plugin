import { render } from 'preact';
import { afterEach, describe, expect, it } from 'vitest';
import { ModelMessage } from './ModelMessage';
import type { RequestView, ResponseView } from './reading';

const item = { key: { requestId: 'request', responseId: 'response', wireItemId: 'message', contentIndex: 0 }, text: '模型**正文**', revision: 1, truncated: false };
const root = document.createElement('div');
const request: RequestView = { requestId: 'request', clientRequestIndex: null, requestedModel: 'requested-model', codexThreadId: null, codexTurnId: null, purpose: 'conversation', purposeBasis: 'codex_turn_metadata' };
const response: ResponseView = { requestId: 'request', responseId: 'response', status: 'receiving', reportedModels: [] };
afterEach(() => render(null, root));
describe('model author identity', () => {
  it('presents a historical partial response as saved evidence rather than live generation', () => {
    render(<ModelMessage item={item} response={response} historical />, root);
    expect(root.textContent).toContain('保存时：尚未观察到响应结束');
    expect(root.textContent).not.toContain('正在接收');
  });
  it('labels an unknown model honestly and does not hard-code a model name', () => {
    render(<ModelMessage item={item} />, root);
    expect(root.querySelector('[data-role="assistant"]')).not.toBeNull();
    expect(root.textContent).toContain('模型名称未确认');
    expect(root.querySelector('strong')?.textContent).toBe('模型');
  });
  it('distinguishes the requested model from reported identity and retains mismatches', () => {
    render(<ModelMessage item={item} request={request} response={response} />, root);
    expect(root.textContent).toContain('requested-model · 请求模型');
    render(<ModelMessage item={item} request={request} response={{ ...response, reportedModels: ['reported-model'] }} />, root);
    expect(root.textContent).toContain('reported-model · 响应报告');
    expect(root.textContent).toContain('请求模型：requested-model；与响应报告不同');
  });
  it('keeps conflicting reports explicit and renders model labels as text', () => {
    render(<ModelMessage item={item} response={{ ...response, reportedModels: ['one', '<img src=x onerror=alert(1)>'] }} />, root);
    expect(root.textContent).toContain('模型名冲突');
    expect(root.querySelector('img')).toBeNull();
  });
  it('marks a missing native submission per turn until evidence arrives', () => {
    render(<ModelMessage item={item} userPending />, root);
    expect(root.textContent).toContain('尚未取得本轮原生用户提交记录');
    render(<ModelMessage item={item} userPending={false} />, root);
    expect(root.querySelector('[role="status"]')).toBeNull();
  });
});
