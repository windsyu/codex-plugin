import { afterEach, describe, expect, it, vi } from 'vitest';
import { Api, ApiError, loadThread, reconnectingStream } from './api';

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

  it('parses fragmented fetch SSE and keeps bearer credentials out of the URL', async () => {
    const encoder = new TextEncoder();
    const body = new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode('event: observer\nid: signed-'));
        controller.enqueue(encoder.encode('cursor\ndata: {"eventSeq":7}\n\n'));
        controller.close();
      }
    });
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(new Response(body, {
      status: 200, headers: { 'Content-Type': 'text/event-stream' }
    }));
    const events: unknown[] = []; const onOpen = vi.fn();
    await new Api('private-token').stream('/v2/stream', (event) => events.push(event), new AbortController().signal, onOpen);
    expect(onOpen).toHaveBeenCalledOnce();
    expect(events).toEqual([{ type: 'observer', id: 'signed-cursor', data: { eventSeq: 7 } }]);
    expect(fetchMock.mock.calls[0][0]).toBe('/v2/stream');
    expect(fetchMock.mock.calls[0][1]?.headers).toMatchObject({ Authorization: 'Bearer private-token' });
  });

  it('reconnects the stream from the last signed composite cursor', async () => {
    const api = new Api('token'); const controller = new AbortController(); const states: string[] = [];
    const streamMock = vi.spyOn(api, 'stream')
      .mockImplementationOnce(async (_path, onEvent, _signal, onOpen) => {
        onOpen?.(); onEvent({ type: 'observer', id: 'signed/composite+cursor', data: {} });
        throw new Error('network disconnected');
      })
      .mockImplementationOnce(async (path) => {
        expect(new URL(path, window.location.origin).searchParams.get('cursor')).toBe('signed/composite+cursor');
        controller.abort();
      });
    await reconnectingStream(api, '/v2/stream', vi.fn(), (state) => states.push(state), controller.signal, 0);
    expect(streamMock).toHaveBeenCalledTimes(2);
    expect(states).toEqual(['connecting', 'live', 'disconnected', 'connecting']);
  });

  it('drops an expired stream cursor before reconnecting', async () => {
    const api = new Api('token'); const controller = new AbortController();
    const streamMock = vi.spyOn(api, 'stream')
      .mockRejectedValueOnce(new ApiError(410, 'expired', 'CURSOR_EXPIRED'))
      .mockImplementationOnce(async (path) => { expect(path).toBe('/v2/stream'); controller.abort(); });
    await reconnectingStream(api, '/v2/stream', vi.fn(), vi.fn(), controller.signal, 0);
    expect(streamMock).toHaveBeenCalledTimes(2);
  });
});
