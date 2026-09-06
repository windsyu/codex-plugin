import type {
  ApiEnvelope,
  Health,
  Item,
  ProjectSummary,
  RawEvent,
  Source,
  Thread,
  ThreadDetail,
  Turn
} from './types';

export class Api {
  private token: string;

  constructor(token: string) {
    this.token = token;
  }

  private headers(): HeadersInit {
    return this.token ? { Authorization: `Bearer ${this.token}` } : {};
  }

  async get<T>(path: string, signal?: AbortSignal): Promise<ApiEnvelope<T>> {
    const response = await fetch(path, {
      headers: this.headers(),
      credentials: 'same-origin',
      cache: 'no-store',
      signal
    });
    const body = await response.json().catch(() => undefined);
    if (!response.ok) {
      throw new ApiError(response.status, body?.error?.message || `HTTP ${response.status}`);
    }
    return body as ApiEnvelope<T>;
  }

  async getAll<T>(path: string, signal?: AbortSignal): Promise<ApiEnvelope<T[]>> {
    const url = new URL(path, window.location.origin);
    const data: T[] = [];
    let envelope: ApiEnvelope<T[]> | undefined;
    do {
      envelope = await this.get<T[]>(`${url.pathname}${url.search}`, signal);
      data.push(...envelope.data);
      if (envelope.nextCursor) url.searchParams.set('cursor', envelope.nextCursor);
    } while (envelope.nextCursor);
    return { ...envelope, data };
  }

  async post<T>(path: string, body: unknown, idempotencyKey: string, signal?: AbortSignal): Promise<ApiEnvelope<T>> {
    const response = await fetch(path, {
      method: 'POST',
      headers: { ...this.headers(), 'Content-Type': 'application/json', 'Idempotency-Key': idempotencyKey },
      credentials: 'same-origin',
      cache: 'no-store',
      body: JSON.stringify(body),
      signal
    });
    const payload = await response.json().catch(() => undefined);
    if (!response.ok) throw new ApiError(response.status, payload?.error?.message || `HTTP ${response.status}`, payload?.error?.code);
    return payload as ApiEnvelope<T>;
  }

  async delete<T>(path: string, body: unknown, idempotencyKey: string, signal?: AbortSignal): Promise<ApiEnvelope<T>> {
    const response = await fetch(path, {
      method: 'DELETE',
      headers: { ...this.headers(), 'Content-Type': 'application/json', 'Idempotency-Key': idempotencyKey },
      credentials: 'same-origin',
      cache: 'no-store',
      body: JSON.stringify(body),
      signal
    });
    const payload = await response.json().catch(() => undefined);
    if (!response.ok) throw new ApiError(response.status, payload?.error?.message || `HTTP ${response.status}`, payload?.error?.code);
    return payload as ApiEnvelope<T>;
  }

  async stream(path: string, onEvent: (event: StreamEvent) => void, signal: AbortSignal, onOpen?: () => void) {
    const response = await fetch(path, {
      headers: { ...this.headers(), Accept: 'text/event-stream' }, credentials: 'same-origin', cache: 'no-store', signal
    });
    if (!response.ok || !response.body) {
      const payload = await response.json().catch(() => undefined);
      throw new ApiError(response.status, payload?.error?.message || `实时连接失败：HTTP ${response.status}`, payload?.error?.code);
    }
    onOpen?.();
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    while (true) {
      const { value, done } = await reader.read();
      if (done) return;
      buffer += decoder.decode(value, { stream: true }).replaceAll('\r\n', '\n');
      let boundary = buffer.indexOf('\n\n');
      while (boundary >= 0) {
        const block = buffer.slice(0, boundary); buffer = buffer.slice(boundary + 2);
        let type = 'message'; let id: string | undefined; const data: string[] = [];
        for (const line of block.split('\n')) {
          if (line.startsWith('event:')) type = line.slice(6).trim();
          else if (line.startsWith('id:')) id = line.slice(3).trim();
          else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
        }
        if (data.length) {
          const text = data.join('\n');
          onEvent({ type, id, data: JSON.parse(text) });
        }
        boundary = buffer.indexOf('\n\n');
      }
    }
  }

}

export interface StreamEvent { type: string; id?: string; data: unknown; }

function streamPath(path: string, cursor?: string) {
  if (!cursor) return path;
  const url = new URL(path, window.location.origin);
  url.searchParams.set('cursor', cursor);
  return `${url.pathname}${url.search}`;
}

function reconnectDelay(milliseconds: number, signal: AbortSignal) {
  return new Promise<void>((resolve) => {
    if (signal.aborted || milliseconds <= 0) return resolve();
    const timer = window.setTimeout(done, milliseconds);
    signal.addEventListener('abort', done, { once: true });
    function done() { window.clearTimeout(timer); signal.removeEventListener('abort', done); resolve(); }
  });
}

export async function reconnectingStream(
  api: Api,
  path: string,
  onEvent: (event: StreamEvent) => void,
  onState: (state: 'connecting' | 'live' | 'disconnected') => void,
  signal: AbortSignal,
  retryMilliseconds = 1_000
) {
  let cursor: string | undefined;
  while (!signal.aborted) {
    onState('connecting');
    try {
      await api.stream(streamPath(path, cursor), (event) => {
        if (event.id) cursor = event.id;
        onEvent(event);
      }, signal, () => onState('live'));
    } catch (error) {
      if (signal.aborted) return;
      if (error instanceof ApiError && error.status === 401) throw error;
      if (error instanceof ApiError && error.status === 410) cursor = undefined;
    }
    if (signal.aborted) return;
    onState('disconnected');
    await reconnectDelay(retryMilliseconds, signal);
  }
}

export class ApiError extends Error {
  constructor(public readonly status: number, message: string, public readonly code?: string) {
    super(message);
    this.name = 'ApiError';
  }
}

export function connect(token: string) {
  return new Api(token);
}

export async function loadDashboard(api: Api, signal?: AbortSignal) {
  const [health, threads, projects, sources] = await Promise.all([
    api.get<Health>('/v1/health', signal),
    api.getAll<Thread>('/v1/threads?limit=200', signal),
    api.get<ProjectSummary[]>('/v1/projects', signal),
    api.get<Source[]>('/v1/sources', signal)
  ]);
  return { health, threads, projects, sources };
}

export async function loadThread(api: Api, threadKey: string, signal?: AbortSignal) {
  const encoded = encodeURIComponent(threadKey);
  const [detail, turns, items] = await Promise.all([
    api.get<ThreadDetail>(`/v1/threads/${encoded}`, signal),
    api.getAll<Turn>(`/v1/threads/${encoded}/turns?limit=200`, signal),
    api.getAll<Item>(`/v1/threads/${encoded}/items?limit=200`, signal)
  ]);
  return { detail, turns, items };
}

export async function loadEventPage(
  api: Api,
  threadKey: string,
  afterEventSeq = 0,
  signal?: AbortSignal
) {
  const encoded = encodeURIComponent(threadKey);
  return api.get<RawEvent[]>(
    `/v1/threads/${encoded}/events?limit=100&afterEventSeq=${afterEventSeq}`,
    signal
  );
}
