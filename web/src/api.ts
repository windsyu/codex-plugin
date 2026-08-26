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

}

export class ApiError extends Error {
  constructor(public readonly status: number, message: string) {
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
