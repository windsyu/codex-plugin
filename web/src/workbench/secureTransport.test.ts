// @vitest-environment node
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SecureTransport } from './secureTransport';
import { RecordCipher, ephemeral, fromBase64, toBase64, type AccessContext, type ChannelBinding } from './secureCrypto';
const id = (digit: string) => `${digit.repeat(8)}-${digit.repeat(4)}-4${digit.repeat(3)}-8${digit.repeat(3)}-${digit.repeat(12)}`;
const access: AccessContext = { wireVersion: 3, apiVersion: 3, accountId: id('1'), parentSessionId: id('2'), grantId: id('3'), gatewayId: id('4'), instanceId: id('5'), connectionEpoch: id('6') };
const run = id('8');
type Frame = Record<string, any>;
class DesktopSocket {
  static OPEN = 1;
  static instances: DesktopSocket[] = [];
  static handle: (socket: DesktopSocket, frame: Frame) => Promise<void> = async () => {};
  readyState = 1; bufferedAmount = 0; binaryType = '';
  onmessage: ((event: { data: any }) => void) | null = null;
  onclose: (() => void) | null = null; onerror: (() => void) | null = null;
  private work = Promise.resolve();
  private outgoing = Promise.resolve();
  cipher?: RecordCipher;
  binding: ChannelBinding;
  frames: Frame[] = [];
  raw: (string | Uint8Array)[] = [];
  constructor(readonly url: string) {
    DesktopSocket.instances.push(this);
    const query = new URL(url).searchParams;
    this.binding = { ...access, channelId: crypto.randomUUID(), scope: query.get('scope') === 'run' ? { kind: 'run', runId: query.get('runId')! } : { kind: 'application' } };
    queueMicrotask(() => this.onmessage?.({ data: JSON.stringify({ type: 'channel_binding', binding: this.binding }) }));
  }
  close() { if (this.readyState === 3) return; this.readyState = 3; this.onclose?.(); }
  send(data: string | Uint8Array) {
    this.raw.push(data);
    this.work = this.work.then(async () => {
      if (typeof data === 'string') {
        const hello = JSON.parse(data); const keys = await ephemeral();
        this.cipher = await RecordCipher.create(this.binding, keys.privateKey, fromBase64(hello.publicKey, 65), keys.publicKey, true);
        this.onmessage?.({ data: JSON.stringify({ type: 'server_hello', publicKey: toBase64(keys.publicKey) }) });
        return;
      }
      const frame = await this.cipher!.open(data); this.frames.push(frame);
      if (frame.type === 'finished') await this.deliver({ type: 'finished' });
      else await DesktopSocket.handle(this, frame);
    });
  }
  deliver(frame: Frame) {
    this.outgoing = this.outgoing.then(async () => {
      const wire = await this.cipher!.seal(frame);
      if (this.readyState !== 3) this.onmessage?.({ data: wire.buffer });
    });
    return this.outgoing;
  }
}
let transport: SecureTransport;
let context: { gateway: string; grant: string; session: string; valid: boolean; assert(): void; invalidate(): void; headers(): Headers; stream(path: string): string };
let onInvalid: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.stubGlobal('location', { origin: 'https://relay.example', protocol: 'https:', host: 'relay.example' });
  vi.stubGlobal('WebSocket', DesktopSocket);
  vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify(access))));
  DesktopSocket.instances = [];
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: Math.min(frame.bodyLength, 256 * 1024) });
    if (frame.type === 'end') {
      await socket.deliver({ type: 'head', id: frame.id, status: 200, headers: [['content-type', 'application/json']] });
      await socket.deliver({ type: 'chunk', id: frame.id, data: toBase64(new TextEncoder().encode('{"accepted":true}')) });
      await socket.deliver({ type: 'end', id: frame.id });
    }
  };
  context = {
    gateway: access.gatewayId, grant: access.grantId, session: access.parentSessionId, valid: true,
    assert() { if (!this.valid) throw new Error('expired'); },
    invalidate() { this.valid = false; transport.invalidate(); },
    headers: () => new Headers({ 'X-Workbench-Grant': access.grantId, 'X-Workbench-Api-Version': '3' }),
    stream: path => `/g/${access.gatewayId}${path}&relayGrant=${access.grantId}&relayApi=3`
  };
  onInvalid = vi.fn(); transport = new SecureTransport(context, onInvalid);
});
afterEach(() => { transport.invalidate(); vi.unstubAllGlobals(); });
it('multiplexes one Run across page requests, separates Application, and hides business metadata', async () => {
  const path = `/workbench/v1/runs/${run}/workspace/file?path=private-file`;
  const response = await transport.fetch(path, { method: 'POST', headers: { 'If-Match': 'secret-revision' }, body: 'private-content' });
  expect(await response.json()).toEqual({ accepted: true });
  expect(await (await transport.fetch(`/workbench/v1/runs/${run}/live/snapshot`)).json()).toEqual({ accepted: true });
  expect(DesktopSocket.instances.length).toBe(1);
  const socket = DesktopSocket.instances[0];
  const opens = socket.frames.filter(frame => frame.type === 'open');
  expect(opens.map(frame => frame.id)).toEqual([1, 2]);
  expect(opens[0]).toMatchObject({ method: 'POST', path, bodyLength: 15, headers: expect.arrayContaining([['if-match', 'secret-revision']]) });
  expect(socket.frames.findIndex(frame => frame.type === 'chunk')).toBeLessThan(socket.frames.findIndex(frame => frame.type === 'end'));
  expect(socket.url).not.toContain('private');
  expect(socket.raw.filter(value => typeof value === 'string').join('')).not.toMatch(/private|secret-revision|workspace/);
  await (await transport.fetch('/workbench/v1/application')).text();
  expect(DesktopSocket.instances.length).toBe(2);
  const anotherTab = new SecureTransport(context, onInvalid);
  await (await anotherTab.fetch(path)).text();
  expect(DesktopSocket.instances.length).toBe(3);
  expect(socket.raw[0]).not.toEqual(DesktopSocket.instances[2].raw[0]);
  anotherTab.invalidate();
});
it('does not consider a head or truncated JSON body a completed response', async () => {
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 });
    if (frame.type === 'end') {
      await socket.deliver({ type: 'head', id: frame.id, status: 200, headers: [] });
      await socket.deliver({ type: 'chunk', id: frame.id, data: toBase64(new TextEncoder().encode('{"accepted":true}')) });
    }
  };
  const response = await transport.fetch(`/workbench/v1/runs/${run}/stop`, { method: 'POST' });
  let complete = false;
  const body = response.json().then(value => { complete = true; return value; });
  await new Promise(resolve => setTimeout(resolve, 20));
  expect(complete).toBe(false);
  DesktopSocket.instances[0].close();
  await expect(body).rejects.toThrow('中断');
  expect(DesktopSocket.instances[0].frames.filter(frame => frame.type === 'open')).toHaveLength(1);
});
it('waits for authenticated End before resolving a bodyless response', async () => {
  let stream = 0;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') { stream = frame.id; await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 }); }
    if (frame.type === 'end') await socket.deliver({ type: 'head', id: frame.id, status: 204, headers: [] });
  };
  let complete = false;
  const response = transport.fetch('/workbench/v1/application', { method: 'HEAD' }).then(value => { complete = true; return value; });
  await vi.waitFor(() => expect(DesktopSocket.instances[0]?.frames.some(frame => frame.type === 'end')).toBe(true));
  expect(complete).toBe(false);
  await DesktopSocket.instances[0].deliver({ type: 'end', id: stream });
  expect((await response).status).toBe(204);
});
it('business 403 preserves the grant; only the dedicated access probe can invalidate it', async () => {
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 });
    if (frame.type === 'end') { await socket.deliver({ type: 'head', id: frame.id, status: 403, headers: [] }); await socket.deliver({ type: 'end', id: frame.id }); }
  };
  expect((await transport.fetch('/workbench/v1/application')).status).toBe(403);
  expect(context.valid).toBe(true); expect(onInvalid).not.toHaveBeenCalled();
  vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 401 })));
  await expect(transport.probe()).rejects.toThrow('expired');
  expect(context.valid).toBe(false); expect(onInvalid).toHaveBeenCalledOnce();
  expect(DesktopSocket.instances[0].readyState).toBe(3);
});
it('abort authenticates and discards late stream data without breaking another stream', async () => {
  let first = 0;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') { first ||= frame.id; await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 }); }
    if (frame.type === 'end' && frame.id !== first) { await socket.deliver({ type: 'head', id: frame.id, status: 204, headers: [] }); await socket.deliver({ type: 'end', id: frame.id }); }
  };
  const controller = new AbortController();
  const firstResponse = transport.fetch('/workbench/v1/application', { signal: controller.signal });
  const rejected = expect(firstResponse).rejects.toThrow('Aborted');
  await vi.waitFor(() => expect(first).toBe(1)); controller.abort(); await rejected;
  const socket = DesktopSocket.instances[0];
  await socket.deliver({ type: 'head', id: first, status: 200, headers: [] });
  await socket.deliver({ type: 'chunk', id: first, data: toBase64(new Uint8Array([1])) });
  await socket.deliver({ type: 'end', id: first });
  expect((await transport.fetch('/workbench/v1/application')).status).toBe(204);
  expect(DesktopSocket.instances).toHaveLength(1);
});
it('revocation during a pending probe discards the result without starting a channel', async () => {
  let resolve!: (value: Response) => void;
  vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(yes => { resolve = yes; })));
  const pending = transport.fetch('/workbench/v1/application');
  await vi.waitFor(() => expect(resolve).toBeTypeOf('function'));
  context.invalidate(); resolve(new Response(JSON.stringify(access)));
  await expect(pending).rejects.toThrow();
  expect(DesktopSocket.instances).toHaveLength(0);
});
it('uploads a multi-window body and never emits End before all bytes are accepted', async () => {
  let length = 0;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 256 * 1024 });
    if (frame.type === 'chunk') { const bytes = fromBase64(frame.data, 16384); length += bytes.length; if (length <= 256 * 1024) await socket.deliver({ type: 'window', id: frame.id, credit: bytes.length }); }
    if (frame.type === 'end') {
      expect(length).toBe(512 * 1024);
      await socket.deliver({ type: 'head', id: frame.id, status: 204, headers: [] }); await socket.deliver({ type: 'end', id: frame.id });
    }
  };
  expect((await transport.fetch('/workbench/v1/application/settings', { method: 'PUT', body: new Uint8Array(512 * 1024) })).status).toBe(204);
  const records = DesktopSocket.instances[0].raw.filter(value => typeof value !== 'string') as Uint8Array[];
  expect(records.map(value => Number(new DataView(value.buffer).getBigUint64(0)))).toEqual(records.map((_, index) => index));
});
it('reserves and reassembles terminal messages larger than a credit window without deadlock', async () => {
  const outgoing = new TextEncoder().encode('x'.repeat(600 * 1024));
  let offset = 0, inputLength = 0, inputDone = false;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 });
    if (frame.type === 'end') {
      await socket.deliver({ type: 'terminal_ready', id: frame.id });
      await socket.deliver({ type: 'message_start', id: frame.id, length: outgoing.length, binary: false });
    }
    if (frame.type === 'window') {
      let credit = frame.credit;
      while (credit && offset < outgoing.length) {
        const count = Math.min(credit, 16384, outgoing.length - offset);
        await socket.deliver({ type: 'chunk', id: frame.id, data: toBase64(outgoing.subarray(offset, offset + count)) });
        offset += count; credit -= count;
        if (offset === outgoing.length) await socket.deliver({ type: 'message_end', id: frame.id });
      }
    }
    if (frame.type === 'message_start') await socket.deliver({ type: 'window', id: frame.id, credit: frame.length });
    if (frame.type === 'chunk') inputLength += fromBase64(frame.data, 16384).length;
    if (frame.type === 'message_end') inputDone = true;
  };
  const socket = transport.terminal(`/workbench/v1/runs/${run}/terminal?epoch=${run}`);
  const received: string[] = [];
  socket.onmessage = event => received.push(event.data);
  await vi.waitFor(() => expect(socket.readyState).toBe(1));
  socket.send('i'.repeat(96 * 1024));
  await vi.waitFor(() => expect(received[0]?.length).toBe(outgoing.length));
  await vi.waitFor(() => expect(inputDone).toBe(true));
  expect(inputLength).toBe(96 * 1024);
  expect(DesktopSocket.instances[0].frames.filter(frame => frame.type === 'window').every(frame => frame.credit <= 256 * 1024)).toBe(true);
  socket.close();
});
it('rejects oversized terminal declarations before granting any byte credit', async () => {
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 });
    if (frame.type === 'end') {
      await socket.deliver({ type: 'terminal_ready', id: frame.id });
      await socket.deliver({ type: 'message_start', id: frame.id, length: 16 * 1024 * 1024 + 1, binary: false });
    }
  };
  const socket = transport.terminal(`/workbench/v1/runs/${run}/terminal`);
  await vi.waitFor(() => expect(socket.readyState).toBe(3));
  expect(DesktopSocket.instances[0].frames.some(frame => frame.type === 'window')).toBe(false);
  expect(DesktopSocket.instances[0].readyState).toBe(1);
});
it('terminal End preserves a parallel SSE and later HTTP requests on the same Run channel', async () => {
  const paths = new Map<number, string>(); let terminalId = 0, sseId = 0;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') { paths.set(frame.id, frame.path); await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 }); }
    if (frame.type !== 'end') return;
    if (paths.get(frame.id)?.endsWith('/terminal')) {
      terminalId = frame.id; await socket.deliver({ type: 'terminal_ready', id: frame.id });
    } else if (paths.get(frame.id)?.endsWith('/live/events')) {
      sseId = frame.id; await socket.deliver({ type: 'head', id: frame.id, status: 200, headers: [['content-type', 'text/event-stream']] });
    } else { await socket.deliver({ type: 'head', id: frame.id, status: 204, headers: [] }); await socket.deliver({ type: 'end', id: frame.id }); }
  };
  const prefix = `/workbench/v1/runs/${run}`;
  const terminal = transport.terminal(`${prefix}/terminal`);
  const events = transport.events(`${prefix}/live/events`);
  const seen: string[] = [];
  events.addEventListener('view', event => seen.push((event as MessageEvent).data));
  await vi.waitFor(() => { expect(terminal.readyState).toBe(1); expect(sseId).toBeGreaterThan(0); });
  const socket = DesktopSocket.instances[0];
  await socket.deliver({ type: 'end', id: terminalId });
  await vi.waitFor(() => expect(terminal.readyState).toBe(3));
  await socket.deliver({ type: 'chunk', id: sseId, data: toBase64(new TextEncoder().encode('event: view\ndata: still-live\n\n')) });
  await vi.waitFor(() => expect(seen).toEqual(['still-live']));
  expect((await transport.fetch(`${prefix}/live/snapshot`)).status).toBe(204);
  expect(DesktopSocket.instances).toHaveLength(1); expect(socket.readyState).toBe(1);
  events.close();
});
it('terminal assembly budget exhaustion resets only that substream', async () => {
  const kinds = new Map<number, string>();
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') { kinds.set(frame.id, frame.kind); await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 }); }
    if (frame.type === 'end' && kinds.get(frame.id) === 'terminal') {
      await socket.deliver({ type: 'terminal_ready', id: frame.id });
      await socket.deliver({ type: 'message_start', id: frame.id, length: 16 * 1024 * 1024, binary: false });
    } else if (frame.type === 'end') { await socket.deliver({ type: 'head', id: frame.id, status: 204, headers: [] }); await socket.deliver({ type: 'end', id: frame.id }); }
  };
  const prefix = `/workbench/v1/runs/${run}`;
  const first = transport.terminal(`${prefix}/terminal`);
  await vi.waitFor(() => expect(DesktopSocket.instances[0]?.frames.some(frame => frame.type === 'window')).toBe(true));
  const second = transport.terminal(`${prefix}/terminal`);
  await vi.waitFor(() => expect(second.readyState).toBe(3));
  expect(first.readyState).toBe(1);
  expect((await transport.fetch(`${prefix}/live/snapshot`)).status).toBe(204);
  expect(DesktopSocket.instances).toHaveLength(1);
  first.close();
});
it('revocation closes a terminal even before its cached channel promise callback runs', async () => {
  await (await transport.fetch(`/workbench/v1/runs/${run}/live/snapshot`)).text();
  const socket = transport.terminal(`/workbench/v1/runs/${run}/terminal`);
  const closed = vi.fn(); socket.onclose = closed;
  transport.invalidate();
  await vi.waitFor(() => expect(socket.readyState).toBe(3));
  expect(closed).toHaveBeenCalledOnce();
});
it('cancellation of a queued Open never allocates a stream ID or creates a counter hole', async () => {
  await (await transport.fetch('/workbench/v1/application')).text();
  const encrypt = crypto.subtle.encrypt.bind(crypto.subtle);
  let resume: (() => void) | undefined, delayed = false;
  const spy = vi.spyOn(crypto.subtle, 'encrypt').mockImplementation((algorithm, key, bytes) => {
    const frame = JSON.parse(new TextDecoder().decode(bytes));
    if (frame.type === 'open' && !delayed) {
      delayed = true;
      return new Promise<ArrayBuffer>((resolve, reject) => { resume = () => { void encrypt(algorithm, key, bytes).then(resolve, reject); }; });
    }
    return encrypt(algorithm, key, bytes);
  });
  try {
    const first = transport.fetch('/workbench/v1/library/first');
    await vi.waitFor(() => expect(resume).toBeTypeOf('function'));
    const abort = new AbortController();
    const cancelled = transport.fetch('/workbench/v1/library/cancelled', { signal: abort.signal });
    const rejection = expect(cancelled).rejects.toThrow('Aborted');
    const third = transport.fetch('/workbench/v1/library/third');
    const channel = await [...(transport as any).channels.values()][0];
    await vi.waitFor(() => expect(channel.queue.length).toBeGreaterThanOrEqual(2));
    abort.abort(); resume!(); resume = undefined; await rejection;
    await (await first).text(); await (await third).text();
    const socket = DesktopSocket.instances[0];
    expect(socket.frames.filter(frame => frame.type === 'open').map(frame => frame.id)).toEqual([1, 2, 3]);
    expect(socket.frames.some(frame => frame.path?.endsWith('/cancelled'))).toBe(false);
    const wire = socket.raw.filter(value => typeof value !== 'string') as Uint8Array[];
    expect(wire.map(value => Number(new DataView(value.buffer).getBigUint64(0)))).toEqual(wire.map((_, index) => index));
  } finally { resume?.(); spy.mockRestore(); }
});
it('revocation during an SSE callback fences remaining plaintext events in the same chunk', async () => {
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'open') await socket.deliver({ type: 'accepted', id: frame.id, credit: 0 });
    if (frame.type === 'end') {
      await socket.deliver({ type: 'head', id: frame.id, status: 200, headers: [['content-type', 'text/event-stream']] });
      await socket.deliver({ type: 'chunk', id: frame.id, data: toBase64(new TextEncoder().encode('event: view\ndata: first\n\nevent: view\ndata: stale\n\n')) });
    }
  };
  const events = transport.events(`/workbench/v1/runs/${run}/live/events`);
  const seen: string[] = [];
  events.addEventListener('view', event => { seen.push((event as MessageEvent).data); context.invalidate(); });
  await vi.waitFor(() => expect(seen).toEqual(['first']));
  await new Promise(resolve => setTimeout(resolve, 10));
  expect(seen).toEqual(['first']); events.close();
});
it('discards an unassigned Window after response End without closing the shared channel', async () => {
  const defaultHandle = DesktopSocket.handle;
  DesktopSocket.handle = async (socket, frame) => {
    if (frame.type === 'end' && frame.id === 1) {
      await socket.deliver({ type: 'head', id: frame.id, status: 200, headers: [] });
    } else await defaultHandle(socket, frame);
  };
  const first = await transport.fetch('/workbench/v1/application');
  const firstBody = first.text();
  const channel = await [...(transport as any).channels.values()][0];
  const socket = DesktopSocket.instances[0];
  const encrypt = crypto.subtle.encrypt.bind(crypto.subtle);
  let resume: (() => void) | undefined, delayed = false;
  const spy = vi.spyOn(crypto.subtle, 'encrypt').mockImplementation((algorithm, key, bytes) => {
    if (JSON.parse(new TextDecoder().decode(bytes)).type === 'open' && !delayed) {
      delayed = true;
      return new Promise<ArrayBuffer>((resolve, reject) => { resume = () => { void encrypt(algorithm, key, bytes).then(resolve, reject); }; });
    }
    return encrypt(algorithm, key, bytes);
  });
  try {
    const second = transport.fetch('/workbench/v1/library/second').then(response => response.text());
    void second.catch(() => {});
    await vi.waitFor(() => expect(resume).toBeTypeOf('function'));
    await socket.deliver({ type: 'chunk', id: 1, data: toBase64(new TextEncoder().encode('first')) });
    await vi.waitFor(() => expect(channel.queue.some((item: any) => item.build().type === 'window')).toBe(true));
    await socket.deliver({ type: 'end', id: 1 });
    expect(await firstBody).toBe('first');
    resume!(); resume = undefined;
    expect(await second).toBe('{"accepted":true}');
    expect(await (await transport.fetch('/workbench/v1/library/third')).text()).toBe('{"accepted":true}');
    expect(DesktopSocket.instances).toHaveLength(1); expect(socket.readyState).toBe(1);
    expect(socket.frames.some(frame => frame.type === 'window' && frame.id === 1)).toBe(false);
    const wire = socket.raw.filter(value => typeof value !== 'string') as Uint8Array[];
    expect(wire.map(value => Number(new DataView(value.buffer).getBigUint64(0)))).toEqual(wire.map((_, index) => index));
  } finally { resume?.(); spy.mockRestore(); }
});
it('large business Opens cannot consume the reserved Window and Reset capacity', async () => {
  await (await transport.fetch('/workbench/v1/application')).text();
  const channel = await [...(transport as any).channels.values()][0];
  const encrypt = crypto.subtle.encrypt.bind(crypto.subtle);
  let resume: (() => void) | undefined, delayed = false;
  const spy = vi.spyOn(crypto.subtle, 'encrypt').mockImplementation((algorithm, key, bytes) => {
    if (JSON.parse(new TextDecoder().decode(bytes)).type === 'open' && !delayed) {
      delayed = true;
      return new Promise<ArrayBuffer>((resolve, reject) => { resume = () => { void encrypt(algorithm, key, bytes).then(resolve, reject); }; });
    }
    return encrypt(algorithm, key, bytes);
  });
  let releasePressure: (() => void) | undefined;
  try {
    const headers = { 'x-synthetic-metadata': 'x'.repeat(31 * 1024) };
    const first = transport.fetch('/workbench/v1/library/large-first', { headers });
    await vi.waitFor(() => expect(resume).toBeTypeOf('function'));
    const second = transport.fetch('/workbench/v1/library/large-second', { headers });
    const third = transport.fetch('/workbench/v1/library/large-third', { headers });
    const all = Promise.all([first, second, third].map(async response => (await response).text()));
    void all.catch(() => {});
    await vi.waitFor(() => expect(channel.queue.length).toBeGreaterThanOrEqual(2));
    expect(channel.controlBytes).toBe(0);
    expect(channel.queueBytes).toBeGreaterThan(3 * 31 * 1024);
    const budget = (transport as any).budget;
    releasePressure = budget.reserve(32 * 1024 * 1024 - budget.bytes);
    const window = channel.send({ type: 'window', id: 1, credit: 1 });
    const reset = channel.send({ type: 'reset', id: 1, code: 'cancelled' });
    expect(channel.controlCount).toBe(2);
    expect(channel.controlBytes).toBeLessThan(1024);
    releasePressure!(); releasePressure = undefined; resume!(); resume = undefined;
    await Promise.all([all, window, reset]);
    expect(DesktopSocket.instances[0].readyState).toBe(1);
    expect(DesktopSocket.instances[0].frames.filter(frame => frame.type === 'open')).toHaveLength(4);
  } finally { releasePressure?.(); resume?.(); spy.mockRestore(); }
});
