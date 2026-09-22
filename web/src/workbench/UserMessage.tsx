import type { UserRecord } from './reading';

export function UserMessage({ user, orderUnconfirmed = false }: { user: UserRecord; orderUnconfirmed?: boolean }) {
  return <article className="wb-user-message" data-role="user">
    <div className="wb-message-meta"><span>原生提交</span><strong>用户</strong><span className="wb-user-avatar" aria-hidden="true">●</span></div>
    <div className="wb-user-body">{user.text || '（本条提交没有可展示的文本）'}</div>
    {orderUnconfirmed && <div className="wb-message-status">同轮多条提交按来源归组，与模型片段的先后尚未确认。</div>}
    {(user.truncated || user.omitted) && <div className="wb-message-status">{user.truncated ? '文本预览已截断。' : ''}{user.omitted ? '本条提交含尚未接入的非文本内容。' : ''}</div>}
  </article>;
}
