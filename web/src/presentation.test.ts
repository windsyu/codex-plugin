import { describe, expect, it } from 'vitest';
import { coalesceActivities, presentItem, summarizeActivities } from './presentation';
import type { Item } from './types';

function item(itemType: string, raw: unknown = {}, itemId = itemType, status = 'completed'): Item {
  return { turnScope: 'turn-1', turnId: 'turn-1', itemId, itemType, status, raw, provenance: {}, lastEventSeq: 1 };
}

describe('presentation registry', () => {
  it.each([
    ['tool_search_output', 'activity', 'tool'],
    ['web_search_call', 'activity', 'search'],
    ['image_generation_call', 'activity', 'image'],
    ['compaction', 'status', 'compaction'],
    ['context_compaction', 'status', 'compaction'],
    ['additional_tools', 'activity', 'tool']
  ])('recognizes official %s', (type, group, activity) => {
    expect(presentItem(item(type))).toMatchObject({ group, activity });
  });

  it('keeps only genuine future variants unknown', () => {
    expect(presentItem(item('future_protocol_variant'))).toMatchObject({ group: 'unknown' });
    expect(presentItem(item('agent_message', { type: 'message', role: 'assistant' }))).toMatchObject({ group: 'dialogue', role: 'assistant' });
  });

  it('coalesces call and output by call_id while retaining severe state', () => {
    const entries = coalesceActivities([
      item('tool_call', { call_id: 'call-1', name: 'lookup' }, 'request'),
      item('tool_output', { call_id: 'call-1', output: 'failed' }, 'response', 'failed'),
      item('command_execution', { call_id: 'call-2', command: 'cargo test' }, 'command')
    ]);
    expect(entries).toHaveLength(2);
    expect(entries[0].items).toHaveLength(2);
    expect(entries[0].status).toBe('failed');
    expect(summarizeActivities(entries)).toBe('1 次命令 · 1 次工具调用');
  });
});
