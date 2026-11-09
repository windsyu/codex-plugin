export const contextKinds = ['system', 'tools', 'skills', 'memory', 'files', 'results', 'chat'] as const;
export type ContextKind = typeof contextKinds[number];
export const contextLabels: Record<ContextKind, string> = { system: '系统', tools: '工具', skills: '技能', memory: '记忆', files: '文件', results: '工具结果', chat: '对话' };
interface ContextCounts { contentBytes: number; estimatedTokens: number }
interface ContextCoverage { complete: boolean; issues: string[] }
export interface ContextSummary extends ContextCounts {
  schemaVersion: 1; methodVersion: 1; coverage: ContextCoverage;
  parts: (ContextCounts & { kind: ContextKind; itemCount: number })[];
}
export interface ContextPresentation extends ContextCounts {
  schemaVersion: 1; tokens: number; calibrated: boolean; reportedInputTokens: number | null; coverage: ContextCoverage;
  parts: (ContextCounts & { kind: ContextKind; itemCount: number; tokens: number })[];
  items: (ContextCounts & { id: string; kind: ContextKind; name: string; tag: string; occurrences: number; references: string[]; referencesPartial: boolean; tokens: number })[];
}
export interface ContextSource { id: string; clientRequestIndex: number | null; captureSeq: number; available: boolean }
export interface ContextFilter { document?: string; category?: ContextKind; item?: string }

const count = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
const counts = (value: ContextCounts) => value && count(value.contentBytes) && count(value.estimatedTokens);
const kind = (value: unknown): value is ContextKind => contextKinds.includes(value as ContextKind);
export function validContextPresentation(value: ContextPresentation): boolean {
  return value?.schemaVersion === 1 && counts(value) && count(value.tokens) && typeof value.calibrated === 'boolean'
    && (value.reportedInputTokens === null || count(value.reportedInputTokens))
    && typeof value.coverage?.complete === 'boolean' && Array.isArray(value.coverage.issues) && value.coverage.issues.every(issue => typeof issue === 'string')
    && Array.isArray(value.parts) && value.parts.length <= 7 && value.parts.every(part => counts(part) && kind(part.kind) && count(part.tokens) && count(part.itemCount))
    && Array.isArray(value.items) && value.items.every(item => counts(item) && kind(item.kind) && count(item.tokens) && count(item.occurrences)
      && typeof item.id === 'string' && typeof item.name === 'string' && typeof item.tag === 'string'
      && typeof item.referencesPartial === 'boolean' && Array.isArray(item.references) && item.references.every(ref => typeof ref === 'string'));
}

const issueLabels: Record<string, string> = {
  media_opaque: '媒体内容无法按文本估算', opaque_media: '媒体内容无法按文本估算',
  opaque_content: '部分内容不可读取', encrypted_content: '加密内容未计入',
  unknown_shape: '存在未识别的内容结构', unknown_content: '存在未识别内容',
  observation_gap: '观察记录存在缺口', truncated: '内容已截断', analysis_truncated: '分类分析达到限制',
  unsupported_content: '存在不支持的内容', references_partial: '条目引用不完整',
  budget_exceeded: '分类分析达到容量限制', server_history: '服务端保留的历史未完整捕获', unsupported_settings: '部分请求设置无法纳入估算',
};
export const contextIssueLabel = (issue: string) => issueLabels[issue] || '部分内容未完整分类';
