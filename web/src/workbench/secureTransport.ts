import { CHUNK_BYTES, WIRE_BYTES, WINDOW_BYTES, RecordCipher, ephemeral, fromBase64, toBase64, validBinding, type AccessContext, type ChannelBinding, type ChannelScope } from './secureCrypto';
import { SecureRecordScheduler } from './secureScheduler';

const encoder = new TextEncoder();
const HTTP_LIMIT = 2 * 1024 * 1024;
const CHANNEL_QUEUE = 2 * 1024 * 1024;
const CONTROL_QUEUE = 64 * 1024;
const PAGE_LIMIT = 32 * 1024 * 1024;
const fail = () => new Error('加密连接已中断，请重新读取状态；未确认的操作不会自动重发。');
interface Context {
  gateway?: string; grant: string; session: string; valid: boolean;
  assert(): void; invalidate(): void; headers(initial?: HeadersInit): Headers;
  stream(path: string): string;
}
class Budget {
  bytes = 0;
  records = 0;
  reserve(bytes: number) {
    if (!Number.isSafeInteger(bytes) || bytes < 0 || this.bytes + bytes > PAGE_LIMIT || this.records >= 8192) throw new Error('secure_page_capacity');
    this.bytes += bytes; this.records++;
    let held = true;
    return () => { if (held) { held = false; this.bytes -= bytes; this.records--; } };
  }
}
type Frame = Record<string, any>;
type Waiter = { resolve(): void; reject(error: Error): void };
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
interface Stream {
  id?: number; kind: 'http' | 'terminal'; closed: boolean; accepted: boolean; requestEnded: boolean;
  credit: number; creditWaiters: Waiter[]; receiveCredit: number;
  receive(frame: Frame): void; abort(error: Error): void;
}
interface Outgoing {
  build(): Frame; stream?: Stream; control: boolean; bytes: number; release(): void;
  flow: object; priority: boolean;
  resolve(): void; reject(error: Error): void;
}

/** One serial record writer owns every counter; cancelled, unassigned work cannot create holes. */
class Channel {
  readonly ready = deferred<void>();
  private socket: WebSocket;
  private cipher?: RecordCipher;
  private state: 'binding' | 'hello' | 'finished' | 'ready' | 'closed' = 'binding';
  private binding?: ChannelBinding;
  private keys?: Awaited<ReturnType<typeof ephemeral>>;
  private incoming = Promise.resolve();
  private incomingBytes = 0;
  private incomingCount = 0;
  private queue: Outgoing[] = [];
  private queueBytes = 0;
  private controlBytes = 0;
  private controlCount = 0;
  private dataCount = 0;
  private readonly controlFlow = {};
  private readonly scheduler = new SecureRecordScheduler();
  private writing = false;
  private nextId = 1;
  private streams = new Map<number, Stream>();
  private pending = new Set<Stream>();
  private timer: ReturnType<typeof setTimeout>;
  private readonly releaseScratch: () => void;
  constructor(readonly access: AccessContext, readonly scope: ChannelScope, private readonly context: Context, private readonly budget: Budget, private readonly disconnected: () => void) {
    // Covers bounded concurrent JSON/base64, UTF-8 and AEAD temporary copies for
    // the serial reader and writer; application-retained response bodies are separate.
    this.releaseScratch = budget.reserve(1024 * 1024 + CONTROL_QUEUE);
    const query = new URLSearchParams({ scope: scope.kind });
    if (scope.kind === 'run') query.set('runId', scope.runId);
    const path = context.stream(`/workbench/v1/secure-channel?${query}`);
    try { this.socket = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${path}`); }
    catch (error) { this.releaseScratch(); throw error; }
    this.socket.binaryType = 'arraybuffer';
    this.timer = setTimeout(() => this.close(), 10_000);
    // Suppress unhandled rejections when the caller is cancelled during handshake.
    void this.ready.promise.catch(() => {});
    this.socket.onmessage = event => {
      if (this.state === 'closed') return;
      const data = event.data;
      const size = typeof data === 'string' ? encoder.encode(data).length : data instanceof ArrayBuffer ? data.byteLength : WIRE_BYTES + 1;
      if (size > WIRE_BYTES || this.incomingBytes + size > CHANNEL_QUEUE || this.incomingCount >= 512) { this.close(); return; }
      let release: () => void;
      try { release = budget.reserve(size); } catch { this.close(); return; }
      this.incomingBytes += size; this.incomingCount++;
      this.incoming = this.incoming.then(async () => { if (this.state !== 'closed') await this.receive(data); }).catch(() => this.close()).finally(() => { this.incomingBytes -= size; this.incomingCount--; release(); this.releaseClosedScratch(); });
    };
    this.socket.onclose = () => this.close();
    this.socket.onerror = () => this.close();
  }
  get closed() { return this.state === 'closed'; }
  private releaseClosedScratch() { if (this.closed && !this.writing && this.incomingCount === 0) this.releaseScratch(); }
  private assert() { this.context.assert(); if (this.closed) throw fail(); }
  private async receive(data: string | ArrayBuffer) {
    this.assert();
    if (this.state === 'binding') {
      if (typeof data !== 'string' || data.length > 8192) throw fail();
      const message = JSON.parse(data);
      if (message.type !== 'channel_binding' || !validBinding(message.binding, this.access, this.scope)) throw fail();
      this.binding = message.binding;
      const keys = await ephemeral(); this.assert();
      this.keys = keys; this.state = 'hello';
      this.socket.send(JSON.stringify({ type: 'client_hello', publicKey: toBase64(keys.publicKey) }));
      return;
    }
    if (this.state === 'hello') {
      if (typeof data !== 'string' || data.length > 1024) throw fail();
      const message = JSON.parse(data);
      if (message.type !== 'server_hello') throw fail();
      const cipher = await RecordCipher.create(this.binding!, this.keys!.privateKey, this.keys!.publicKey, fromBase64(message.publicKey, 65));
      this.assert(); this.cipher = cipher; this.keys = undefined; this.state = 'finished';
      await this.send({ type: 'finished' });
      return;
    }
    if (!(data instanceof ArrayBuffer) || !this.cipher) throw fail();
    const frame = await this.cipher.open(new Uint8Array(data)); this.assert();
    if (this.state === 'finished') {
      if (frame.type !== 'finished') throw fail();
      this.state = 'ready'; clearTimeout(this.timer); this.ready.resolve(); return;
    }
    if (!Number.isSafeInteger(frame.id) || frame.id < 1 || frame.id >= this.nextId || frame.type === 'finished') throw fail();
    const stream = this.streams.get(frame.id);
    // Authenticated late records for locally reset or completed streams are discarded.
    if (!stream) return;
    if (frame.type === 'reset') { this.reset(stream, false); return; }
    if (frame.type === 'accepted') {
      if (stream.accepted) throw fail();
      stream.accepted = true; this.addCredit(stream, frame.credit);
    } else if (frame.type === 'window') this.addCredit(stream, frame.credit);
    else stream.receive(frame);
  }
  private addCredit(stream: Stream, credit: number) {
    if (!Number.isSafeInteger(credit) || credit < 0 || stream.credit + credit > WINDOW_BYTES) throw fail();
    stream.credit += credit;
    for (const waiter of stream.creditWaiters.splice(0)) waiter.resolve();
  }
  send(frame: Frame, stream?: Stream, control = true): Promise<void> {
    const bytes = encoder.encode(JSON.stringify(frame)).length + 24;
    return this.enqueue(() => frame, bytes, stream, control, !stream || stream.kind === 'terminal' || frame.type === 'window' || frame.type === 'reset');
  }
  private enqueue(build: () => Frame, bytes: number, stream?: Stream, control = true, priority = stream?.kind === 'terminal'): Promise<void> {
    try {
      this.assert();
      if (stream?.closed || bytes > WIRE_BYTES || (control ? this.controlCount >= 128 || this.controlBytes + bytes > CONTROL_QUEUE : this.dataCount >= 512 || this.queueBytes + bytes > CHANNEL_QUEUE)) throw fail();
      // Control bytes and their 128 slots were reserved when this channel opened.
      const release = control ? () => {} : this.budget.reserve(bytes);
      if (control) { this.controlBytes += bytes; this.controlCount++; } else { this.queueBytes += bytes; this.dataCount++; }
      const result = new Promise<void>((resolve, reject) => this.queue.push({ build, stream, control, bytes, release, resolve, reject, priority, flow: stream || this.controlFlow }));
      void this.write(); return result;
    } catch (error) { return Promise.reject(error); }
  }
  private async write() {
    if (this.writing) return;
    this.writing = true;
    try {
      while (this.queue.length && !this.closed) {
        const item = this.queue.splice(this.scheduler.choose(this.queue), 1)[0];
        let assigned = false;
        try {
          if (item.stream?.closed) throw fail();
          this.assert();
          const frame = item.build();
          assigned = true;
          const record = await this.cipher!.seal(frame);
          // Once seal allocated a counter the entire channel must close on failure.
          this.assert();
          if (this.socket.readyState !== WebSocket.OPEN || this.socket.bufferedAmount + record.length > CHANNEL_QUEUE) throw fail();
          this.socket.send(record); item.resolve();
          const started = performance.now();
          while (this.socket.bufferedAmount) {
            await new Promise(resolve => setTimeout(resolve, 10)); this.assert();
            if (performance.now() - started > 10_000) throw fail();
          }
        } catch (error) {
          item.reject(error as Error);
          // A cancelled item checked before build has not consumed an ID/counter.
          if (assigned || !item.stream?.closed) this.close();
        } finally {
          if (item.control) { this.controlBytes -= item.bytes; this.controlCount--; } else { this.queueBytes -= item.bytes; this.dataCount--; }
          item.release();
        }
      }
    } finally { this.writing = false; this.releaseClosedScratch(); }
  }
  async open(stream: Stream, frame: Frame) {
    if (this.streams.size + this.pending.size >= 64) throw new Error('secure_stream_capacity');
    this.pending.add(stream);
    try {
      await this.ready.promise; this.assert();
      await this.enqueue(() => {
        if (this.nextId > 0xffffffff) throw fail();
        stream.id = this.nextId++; this.streams.set(stream.id, stream);
        return { ...frame, type: 'open', id: stream.id };
      }, encoder.encode(JSON.stringify(frame)).length + 64, stream, false);
    } finally { this.pending.delete(stream); }
  }
  async chunks(stream: Stream, bytes: Uint8Array) {
    for (let offset = 0; offset < bytes.length;) {
      this.assert(); if (stream.closed) throw fail();
      if (!stream.accepted || stream.credit === 0) { await new Promise<void>((resolve, reject) => stream.creditWaiters.push({ resolve, reject })); continue; }
      const count = Math.min(CHUNK_BYTES, stream.credit, bytes.length - offset);
      stream.credit -= count;
      await this.send({ type: 'chunk', id: stream.id, data: toBase64(bytes.subarray(offset, offset + count)) }, stream, false);
      offset += count;
    }
  }
  async endRequest(stream: Stream) {
    // Empty bodies still wait for Accepted, so rejected opens cannot execute later data.
    while (!stream.accepted) {
      this.assert(); if (stream.closed) throw fail();
      await new Promise<void>((resolve, reject) => stream.creditWaiters.push({ resolve, reject }));
    }
    await this.send({ type: 'end', id: stream.id }, stream);
    stream.requestEnded = true;
  }
  grant(stream: Stream, credit: number) {
    if (credit <= 0) return;
    if (stream.receiveCredit + credit > WINDOW_BYTES) throw fail();
    stream.receiveCredit += credit;
    // Finished streams may discard an unassigned Window; writer failures after
    // counter allocation still close the channel inside write().
    void this.send({ type: 'window', id: stream.id, credit }, stream).catch(() => { if (!stream.closed) this.close(); });
  }
  consume(stream: Stream, frame: Frame) {
    const bytes = fromBase64(frame.data, CHUNK_BYTES);
    if (!bytes.length || bytes.length > stream.receiveCredit) throw fail();
    stream.receiveCredit -= bytes.length;
    return bytes;
  }
  finish(stream: Stream) {
    stream.closed = true;
    if (stream.id !== undefined) this.streams.delete(stream.id);
    this.pending.delete(stream);
    for (const waiter of stream.creditWaiters.splice(0)) waiter.reject(fail());
  }
  reset(stream: Stream, notify = true, error = fail()) {
    if (stream.closed) return;
    this.finish(stream); stream.abort(error);
    if (notify && stream.id !== undefined && !this.closed) void this.send({ type: 'reset', id: stream.id, code: 'cancelled' }).catch(() => this.close());
  }
  close() {
    if (this.closed) return;
    this.state = 'closed'; clearTimeout(this.timer); this.cipher = undefined; this.keys = undefined;
    this.ready.reject(fail()); this.socket.close();
    for (const item of this.queue.splice(0)) { item.reject(fail()); item.release(); if (item.control) { this.controlBytes -= item.bytes; this.controlCount--; } else { this.queueBytes -= item.bytes; this.dataCount--; } }
    for (const stream of [...this.streams.values(), ...this.pending]) this.reset(stream, false);
    this.releaseClosedScratch();
    this.disconnected();
  }
}

function newStream(kind: Stream['kind']): Stream {
  return { kind, closed: false, accepted: false, requestEnded: false, credit: 0, receiveCredit: 0, creditWaiters: [], receive() { throw fail(); }, abort() {} };
}
export interface SecureSocket {
  readyState: number; bufferedAmount: number;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: (() => void) | null; onerror: (() => void) | null;
  send(data: string): void; close(): void;
}

/** Lazily instantiated once per page, with no persisted key material or shared tab state. */
export class SecureTransport {
  private readonly budget = new Budget();
  private readonly channels = new Map<string, Promise<Channel>>();
  private access?: AccessContext;
  private probing?: Promise<AccessContext>;
  private readonly pendingTerminals = new Set<() => void>();
  private generation = 0;
  constructor(private readonly context: Context, private readonly onInvalid: () => void) {}
  invalidate() {
    this.generation++;
    for (const close of [...this.pendingTerminals]) close();
    for (const channel of this.channels.values()) void channel.then(value => value.close(), () => {});
    this.channels.clear(); this.probing = undefined;
  }
  async probe(): Promise<AccessContext> {
    this.context.assert();
    if (this.probing) return this.probing;
    const generation = this.generation;
    const task = (async () => {
      const response = await fetch(`/relay/v1/gateways/${this.context.gateway}/access`, { headers: this.context.headers(), credentials: 'same-origin', cache: 'no-store' });
      if (generation !== this.generation) throw fail();
      this.context.assert();
      if (response.status === 401 || response.status === 403) { this.context.invalidate(); this.invalidate(); this.onInvalid(); this.context.assert(); throw fail(); }
      if (!response.ok) throw fail();
      const access: AccessContext = await response.json();
      if (generation !== this.generation) throw fail();
      this.context.assert();
      if (access.wireVersion !== 3 || access.apiVersion !== 3 || access.gatewayId !== this.context.gateway || access.grantId !== this.context.grant || access.parentSessionId !== this.context.session) throw fail();
      if (this.access && ['accountId', 'parentSessionId', 'grantId', 'gatewayId', 'instanceId'].some(key => access[key as keyof AccessContext] !== this.access![key as keyof AccessContext])) {
        this.context.invalidate(); this.invalidate(); this.onInvalid(); throw fail();
      }
      if (this.access && access.connectionEpoch !== this.access.connectionEpoch) this.invalidate();
      this.access = access;
      return access;
    })();
    this.probing = task;
    void task.finally(() => { if (this.probing === task) this.probing = undefined; }).catch(() => {});
    return task;
  }
  private async channel(path: string) {
    this.context.assert();
    if (!path.startsWith('/workbench/v1/') || /[\\#]/.test(path)) throw fail();
    const match = /^\/workbench\/v1\/runs\/([0-9a-f-]{36})\//.exec(path);
    const scope: ChannelScope = match ? { kind: 'run', runId: match[1] } : { kind: 'application' };
    const key = JSON.stringify(scope);
    let task = this.channels.get(key);
    if (!task) {
      if (this.channels.size >= 64) throw new Error('secure_channel_capacity');
      const generation = this.generation;
      task = this.probe().then(access => {
        if (generation !== this.generation) throw fail();
        this.context.assert();
        const channel = new Channel(access, scope, this.context, this.budget, () => {
          if (this.channels.get(key) === task) this.channels.delete(key);
          // The probe is read-only; business 403 never expires a grant.
          if (this.context.valid) void this.probe().catch(() => {});
        });
        return channel;
      });
      this.channels.set(key, task);
      void task.catch(() => { if (this.channels.get(key) === task) this.channels.delete(key); });
    }
    return task;
  }
  async fetch(path: string, init: RequestInit = {}): Promise<Response> {
    this.context.assert();
    const generation = this.generation;
    const signal = init.signal;
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const request = new Request(new URL(path, location.origin), init);
    const releaseBody = this.budget.reserve(HTTP_LIMIT);
    let body = new Uint8Array();
    const reader = request.body?.getReader();
    const abort = () => { void reader?.cancel(); };
    signal?.addEventListener('abort', abort, { once: true });
    let timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; abort(); }, 10_000);
    const started = performance.now();
    try {
      if (reader) {
        const chunks: Uint8Array[] = []; let length = 0;
        for (;;) {
          const chunk = await reader.read();
          if (signal?.aborted || timedOut || performance.now() - started >= 10_000 || generation !== this.generation) throw fail();
          if (chunk.done) break;
          length += chunk.value.length;
          if (length > HTTP_LIMIT || chunks.length >= 512) throw new Error('secure_request_capacity');
          chunks.push(chunk.value);
        }
        const releaseCopy = this.budget.reserve(length);
        try { body = new Uint8Array(length); let offset = 0; for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.length; } }
        finally { releaseCopy(); }
      }
    } catch (error) { releaseBody(); void reader?.cancel(); throw error; }
    finally { clearTimeout(timeout); signal?.removeEventListener('abort', abort); }
    let channel: Channel;
    try { channel = await this.channel(path); } catch (error) { releaseBody(); throw error; }
    if (generation !== this.generation || signal?.aborted) { releaseBody(); throw fail(); }
    const stream = newStream('http');
    let releaseWindow: () => void;
    try { releaseWindow = this.budget.reserve(WINDOW_BYTES); } catch (error) { releaseBody(); throw error; }
    stream.receiveCredit = WINDOW_BYTES;
    const response = deferred<Response>();
    let head: Frame | undefined, ended = false, delivered = false;
    const chunks: { bytes: Uint8Array; release(): void }[] = [];
    let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
    let waiting = false;
    let requestTimer: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => channel.reset(stream, true, new DOMException('Aborted', 'AbortError'));
    const cleanup = () => { signal?.removeEventListener('abort', cancel); clearTimeout(requestTimer); releaseBody(); releaseWindow(); };
    const drain = () => {
      if (waiting && chunks.length) {
        const { bytes, release } = chunks.shift()!; release(); waiting = false; controller!.enqueue(bytes);
        if (!ended) channel.grant(stream, bytes.length);
      }
      if (ended && !chunks.length) { controller?.close(); channel.finish(stream); cleanup(); }
    };
    const bodyStream = new ReadableStream<Uint8Array>({ start(value) { controller = value; }, pull() { waiting = true; drain(); }, cancel() { channel.reset(stream); } }, { highWaterMark: 0 });
    stream.abort = error => { for (const chunk of chunks.splice(0)) chunk.release(); controller?.error(error); response.reject(error); cleanup(); };
    stream.receive = frame => {
      if (!stream.accepted || !stream.requestEnded || ended) throw fail();
      if (frame.type === 'head') {
        if (head || !Number.isInteger(frame.status) || frame.status < 200 || frame.status > 599 || !Array.isArray(frame.headers)) throw fail();
        head = frame;
        if (request.method !== 'HEAD' && ![204, 205, 304].includes(frame.status)) {
          response.resolve(new Response(bodyStream, { status: frame.status, headers: frame.headers })); delivered = true;
        }
      } else if (frame.type === 'chunk') {
        if (!head || !delivered) throw fail();
        const bytes = channel.consume(stream, frame);
        if (chunks.length >= 512) { channel.reset(stream); return; }
        let release: () => void;
        try { release = this.budget.reserve(0); } catch { channel.reset(stream); return; }
        chunks.push({ bytes, release }); drain();
      } else if (frame.type === 'end') {
        if (!head) throw fail();
        ended = true;
        if (!delivered) response.resolve(new Response(null, { status: head.status, headers: head.headers }));
        drain();
      } else throw fail();
    };
    signal?.addEventListener('abort', cancel, { once: true });
    if (signal?.aborted) cancel();
    requestTimer = setTimeout(() => channel.reset(stream), 10_000);
    void (async () => {
      await channel.open(stream, { kind: 'http', method: request.method, path, headers: [...request.headers], bodyLength: body.length, receiveWindow: WINDOW_BYTES });
      await channel.chunks(stream, body); releaseBody(); body = new Uint8Array();
      await channel.endRequest(stream); clearTimeout(requestTimer);
    })().catch(error => channel.reset(stream, true, error));
    return response.promise;
  }
  events(path: string) {
    const target = new EventTarget();
    const controller = new AbortController();
    let closed = false;
    const generation = this.generation;
    const assertCurrent = () => { this.context.assert(); if (generation !== this.generation || closed) throw fail(); };
    const source = {
      onopen: null as ((event: Event) => void) | null,
      onerror: null as ((event: Event) => void) | null,
      addEventListener: (type: string, listener: EventListener) => target.addEventListener(type, listener),
      close: () => { closed = true; controller.abort(); }
    };
    void (async () => {
      // LiveHub permits events up to its 2MiB client budget. Reserve UTF-16
      // assembly space before reading rather than deadlocking at the wire window.
      const eventLimit = 2 * 1024 * 1024;
      const release = this.budget.reserve(eventLimit * 2);
      try {
        const response = await this.fetch(path, { signal: controller.signal });
        if (!response.ok || !response.body || !response.headers.get('content-type')?.startsWith('text/event-stream')) throw fail();
        assertCurrent();
        source.onopen?.(new Event('open'));
        const reader = response.body.getReader();
        const decoder = new TextDecoder('utf-8', { fatal: true });
        let buffer = '', event = '', data = '';
        for (;;) {
          const chunk = await reader.read();
          assertCurrent();
          if (chunk.done) throw fail();
          buffer += decoder.decode(chunk.value, { stream: true });
          if (buffer.length + data.length > eventLimit) throw new Error('secure_event_capacity');
          let newline: number;
          while ((newline = buffer.indexOf('\n')) !== -1) {
            const line = buffer.slice(0, newline).replace(/\r$/, ''); buffer = buffer.slice(newline + 1);
            if (!line) {
              assertCurrent();
              if (data) target.dispatchEvent(new MessageEvent(event || 'message', { data: data.slice(0, -1) }));
              event = ''; data = '';
              if (closed) return;
            } else if (line.startsWith('event:')) { event = line.slice(6).replace(/^ /, ''); if (event.length > 128) throw fail(); }
            else if (line.startsWith('data:')) data += line.slice(5).replace(/^ /, '') + '\n';
          }
        }
      } finally { release(); }
    })().catch(() => { if (!closed) { source.onerror?.(new Event('error')); source.close(); } });
    return source;
  }
  terminal(path: string): SecureSocket {
    const stream = newStream('terminal');
    let channel: Channel | undefined;
    let assembly: { bytes: Uint8Array; offset: number; binary: boolean; release(): void } | undefined;
    let sending = Promise.resolve();
    let pendingMessages = 0;
    const generation = this.generation;
    const socket: SecureSocket = {
      readyState: 0, bufferedAmount: 0, onmessage: null, onclose: null, onerror: null,
      close: () => { this.pendingTerminals.delete(socket.close); if (stream.closed) return; if (channel) channel.reset(stream); else { stream.closed = true; socket.readyState = 3; socket.onclose?.(); } },
      send: (text: string) => {
        if (socket.readyState !== 1 || !channel || stream.closed) throw fail();
        const bytes = encoder.encode(text);
        if (bytes.length > 96 * 1024 || socket.bufferedAmount + bytes.length > CHANNEL_QUEUE || pendingMessages >= 128) { socket.close(); throw fail(); }
        let release: () => void;
        try { release = this.budget.reserve(bytes.length); } catch (error) { socket.close(); throw error; }
        socket.bufferedAmount += bytes.length; pendingMessages++;
        sending = sending.then(async () => {
          if (stream.closed || generation !== this.generation) throw fail();
          stream.credit = 0;
          await channel!.send({ type: 'message_start', id: stream.id, length: bytes.length, binary: false }, stream);
          await channel!.chunks(stream, bytes);
          await channel!.send({ type: 'message_end', id: stream.id }, stream);
        }).catch(() => socket.close()).finally(() => { release(); socket.bufferedAmount -= bytes.length; pendingMessages--; });
      }
    };
    stream.abort = () => { assembly?.release(); assembly = undefined; socket.readyState = 3; socket.onclose?.(); };
    stream.receive = frame => {
      if (frame.type === 'end') {
        if (!stream.accepted || !stream.requestEnded) throw fail();
        // Native WS completion belongs to this logical terminal, not the shared
        // Run connection. A partial message is discarded and recovered on reconnect.
        channel!.reset(stream, false);
      } else if (frame.type === 'terminal_ready') {
        if (!stream.accepted || !stream.requestEnded || socket.readyState !== 0) throw fail();
        socket.readyState = 1;
      } else if (frame.type === 'message_start') {
        if (socket.readyState !== 1 || assembly || !Number.isSafeInteger(frame.length) || frame.length < 0 || typeof frame.binary !== 'boolean') throw fail();
        if (frame.length > 16 * 1024 * 1024) { channel!.reset(stream); return; }
        let release: () => void;
        try { release = this.budget.reserve(frame.length); } catch { channel!.reset(stream); return; }
        try { assembly = { bytes: new Uint8Array(frame.length), offset: 0, binary: frame.binary, release }; }
        catch { release(); channel!.reset(stream); return; }
        stream.receiveCredit = 0; channel!.grant(stream, Math.min(frame.length, WINDOW_BYTES));
      } else if (frame.type === 'chunk') {
        if (!assembly) throw fail();
        const bytes = channel!.consume(stream, frame);
        if (assembly.offset + bytes.length > assembly.bytes.length) throw fail();
        assembly.bytes.set(bytes, assembly.offset); assembly.offset += bytes.length;
        channel!.grant(stream, Math.min(bytes.length, assembly.bytes.length - assembly.offset - stream.receiveCredit));
      } else if (frame.type === 'message_end') {
        if (!assembly || assembly.offset !== assembly.bytes.length) throw fail();
        const complete = assembly; assembly = undefined;
        try { socket.onmessage?.(new MessageEvent('message', { data: complete.binary ? complete.bytes.buffer : new TextDecoder('utf-8', { fatal: true }).decode(complete.bytes) })); }
        finally { complete.release(); }
      } else throw fail();
    };
    this.pendingTerminals.add(socket.close);
    void this.channel(path).then(async value => {
      if (stream.closed) return;
      if (generation !== this.generation) { socket.close(); return; }
      channel = value;
      this.pendingTerminals.delete(socket.close);
      await channel.open(stream, { kind: 'terminal', method: 'GET', path, headers: [], bodyLength: 0, receiveWindow: 0 });
      await channel.endRequest(stream);
    }).catch(() => socket.close());
    return socket;
  }
}
