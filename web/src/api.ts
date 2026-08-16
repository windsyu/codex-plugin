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

  async get<T>(path: string): Promise<ApiEnvelope<T>> {
    const response = await fetch(path, {
      headers: this.headers(),
      credentials: 'same-origin',
      cache: 'no-store'
    });
    const body = await response.json();
    if (!response.ok) {
      throw new Error(body?.error?.message || `HTTP ${response.status}`);
    }
    return body as ApiEnvelope<T>;
  }

  async getAll<T>(path: string): Promise<ApiEnvelope<T[]>> {
    const url = new URL(path, window.location.origin);
    const data: T[] = [];
    let envelope: ApiEnvelope<T[]> | undefined;
    do {
      envelope = await this.get<T[]>(`${url.pathname}${url.search}`);
      data.push(...envelope.data);
      if (envelope.nextCursor) url.searchParams.set('cursor', envelope.nextCursor);
    } while (envelope.nextCursor);
    return { ...envelope, data };
  }

  async getAllEvents(path: string): Promise<ApiEnvelope<RawEvent[]>> {
    const url = new URL(path, window.location.origin);
    const data: RawEvent[] = [];
    let envelope: ApiEnvelope<RawEvent[]> | undefined;
    do {
      envelope = await this.get<RawEvent[]>(`${url.pathname}${url.search}`);
      data.push(...envelope.data);
      const last = envelope.data.at(-1);
      if (last) url.searchParams.set('afterEventSeq', String(last.eventSeq));
    } while (envelope.data.length === 200);
    return { ...envelope, data };
  }
}

export function connect(token: string) {
  return new Api(token);
}

export async function loadDashboard(api: Api) {
  const [health, threads, projects, sources] = await Promise.all([
    api.get<Health>('/v1/health'),
    api.getAll<Thread>('/v1/threads?limit=200'),
    api.get<ProjectSummary[]>('/v1/projects'),
    api.get<Source[]>('/v1/sources')
  ]);
  return { health, threads, projects, sources };
}

export async function loadThread(api: Api, threadKey: string) {
  const encoded = encodeURIComponent(threadKey);
  const [detail, turns, items, events] = await Promise.all([
    api.get<ThreadDetail>(`/v1/threads/${encoded}`),
    api.getAll<Turn>(`/v1/threads/${encoded}/turns?limit=200`),
    api.getAll<Item>(`/v1/threads/${encoded}/items?limit=200`),
    api.getAllEvents(`/v1/threads/${encoded}/events?limit=200`)
  ]);
  return { detail, turns, items, events };
}
