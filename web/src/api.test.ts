import { afterEach, describe, expect, it, vi } from 'vitest';
import { Api, ApiError, loadThread } from './api';

afterEach(() => vi.restoreAllMocks());

function envelope(data: unknown, nextCursor?: string) {
  return new Response(JSON.stringify({ apiVersion: 'v1', asOfEventSeq: 1, data, nextCursor }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' }
  });
}

describe('Api', () => {
  it('passes AbortSignal through and reports status-aware errors', async () => {
    const controller = new AbortController();
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(envelope({ ok: true }));
    await new Api('token').get('/v1/health', controller.signal);
    expect(fetchMock.mock.calls[0][1]?.signal).toBe(controller.signal);

    fetchMock.mockResolvedValueOnce(new Response(JSON.stringify({ error: { message: 'expired' } }), { status: 401 }));
    await expect(new Api('token').get('/v1/health')).rejects.toEqual(expect.objectContaining({ status: 401, message: 'expired' }));
  });

  it('loads detail, turns and items without eagerly requesting raw events', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
      const path = String(input);
      if (path.includes('/turns')) return envelope([]);
      if (path.includes('/items')) return envelope([]);
      return envelope({ thread: {}, relations: { children: [] }, diagnostics: {}, pendingRequests: [], projectionConflicts: [] });
    });
    await loadThread(new Api('token'), 'thread/key');
    const paths = fetchMock.mock.calls.map(([input]) => String(input));
    expect(paths).toHaveLength(3);
    expect(paths.some((path) => path.includes('/events'))).toBe(false);
  });

  it('consumes cursor pages without truncating search results', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(envelope([{ id: 1 }], 'next'))
      .mockResolvedValueOnce(envelope([{ id: 2 }]));
    const result = await new Api('token').getAll<{ id: number }>('/v1/search?q=x&limit=200');
    expect(result.data).toEqual([{ id: 1 }, { id: 2 }]);
    expect(String(fetchMock.mock.calls[1][0])).toContain('cursor=next');
  });
});
