export interface UsageMetric { tokens: number | null; responses: number }
export interface UsageSummary {
  responseCount: number; missingResponses: number; excludedResponses: number;
  inputTokens: UsageMetric; outputTokens: UsageMetric; totalTokens: UsageMetric;
  cachedInputTokens: UsageMetric; cacheWriteTokens: UsageMetric; reasoningTokens: UsageMetric;
  captureIncomplete: boolean; unidentifiedResponse: boolean; capacityExceeded: boolean;
}
export function parseUsageSummary(value: any): UsageSummary | undefined {
  if (value === undefined) return undefined; // Older saved snapshots remain readable.
  const count = (v: unknown) => Number.isSafeInteger(v) && (v as number) >= 0;
  if (!value || !['responseCount', 'missingResponses', 'excludedResponses'].every(key => count(value[key]))
    || value.missingResponses + value.excludedResponses > value.responseCount
    || !['captureIncomplete', 'unidentifiedResponse', 'capacityExceeded'].every(key => typeof value[key] === 'boolean')) throw new Error('invalid usage summary');
  for (const key of ['inputTokens', 'outputTokens', 'totalTokens', 'cachedInputTokens', 'cacheWriteTokens', 'reasoningTokens']) {
    const metric = value[key];
    if (!metric || !count(metric.responses) || metric.responses > value.responseCount - value.missingResponses - value.excludedResponses
      || (metric.tokens !== null && (!count(metric.tokens) || !metric.responses))) throw new Error('invalid usage metric');
  }
  return value;
}
export const usagePartial = (summary: UsageSummary) => summary.totalTokens.responses < summary.responseCount || summary.captureIncomplete || summary.unidentifiedResponse || summary.capacityExceeded;
export const fullTokens = (metric: UsageMetric | undefined) => metric?.tokens == null ? '—' : metric.tokens.toLocaleString('zh-CN');
export const compactTokens = (metric: UsageMetric | undefined) => metric?.tokens == null ? '—' : new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 }).format(metric.tokens);
