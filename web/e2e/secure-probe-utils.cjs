// Test-only observation at the real WebCrypto boundary. No key export, plaintext
// logging or replacement transport: only fixed counters and public identity facts
// leave the page. Counting logical input attempts preserves the zero-input oracle
// after the outer WebSocket became opaque.
async function trackDocumentSockets(page) {
  const session = await page.context().newCDPSession(page);
  let loader, generation = 0, current = new Set();
  const document = frame => {
    // CDP frameNavigated carries a new loader only for a real document commit.
    // SPA pushState/hash navigation uses navigatedWithinDocument, not this event.
    if (frame.parentId || !frame.loaderId || frame.loaderId === loader) return;
    loader = frame.loaderId; generation++; current = new Set();
  };
  session.on('Page.frameNavigated', event => document(event.frame));
  await session.send('Page.enable');
  document((await session.send('Page.getFrameTree')).frameTree.frame);
  page.on('websocket', socket => {
    const owner = current;
    owner.add(socket); socket.on('close', () => owner.delete(socket));
  });
  return { get size() { return current.size; }, get generation() { return generation; } };
}
async function observeSecurePage(page, report) {
  await page.exposeBinding('__secureProbeMetric', (_, metric) => report(metric));
  await page.addInitScript(() => {
    const state = { application: null, runEpoch: null, encrypted: 0, decrypted: 0, errors: 0 };
    Object.defineProperty(window, '__secureProbe', { value: state });
    // Observe the native objects of this document, without replacing any send,
    // close, event, handshake, or transport behavior. Reload creates a new set.
    const NativeWebSocket = window.WebSocket, sockets = new Set();
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args, newTarget) {
        const socket = Reflect.construct(target, args, newTarget);
        sockets.add(socket);
        return socket;
      }
    });
    Object.defineProperty(state, 'socketCounts', { get() {
      const counts = { created: sockets.size, connecting: 0, open: 0, closing: 0, closed: 0, live: 0 };
      for (const socket of sockets) {
        if (socket.readyState === NativeWebSocket.CONNECTING) counts.connecting++;
        else if (socket.readyState === NativeWebSocket.OPEN) counts.open++;
        else if (socket.readyState === NativeWebSocket.CLOSING) counts.closing++;
        else if (socket.readyState === NativeWebSocket.CLOSED) counts.closed++;
        if (socket.readyState !== NativeWebSocket.CLOSED) counts.live++;
      }
      return counts;
    } });
    const channels = new Map();
    const decoder = new TextDecoder();
    const metric = value => { void window.__secureProbeMetric(value).catch(() => { state.errors++; }); };
    const failure = () => { state.errors++; metric({ kind: 'instrumentation_error' }); };
    const observe = (algorithm, plaintext, sending) => {
      try {
        if (algorithm.name !== 'AES-GCM') return;
        const aad = new Uint8Array(algorithm.additionalData.buffer || algorithm.additionalData, algorithm.additionalData.byteOffset || 0, algorithm.additionalData.byteLength);
        const identity = Array.from(aad.subarray(0, 32), byte => byte.toString(16).padStart(2, '0')).join('');
        let streams = channels.get(identity);
        if (!streams) { streams = new Map(); channels.set(identity, streams); }
        const frame = JSON.parse(decoder.decode(plaintext));
        if (sending) state.encrypted++; else state.decrypted++;
        if (sending && frame.type === 'open') {
          const isApplication = frame.path === '/workbench/v1/application';
          const isRun = /^\/workbench\/v1\/runs\/[^/]+\/run$/.test(frame.path);
          streams.set(frame.id, { kind: frame.kind, identityResponse: isApplication ? 'application' : isRun ? 'run' : null, response: '', input: null, events: frame.path.split('?')[0].endsWith('/live/events') });
          metric({ kind: 'open', terminal: frame.kind === 'terminal', start: frame.method === 'POST' && frame.path === '/workbench/v1/runs', picker: frame.path === '/workbench/v1/application/pick-directory', syntheticSearch: frame.path.includes('E2EE_SYNTHETIC_SEARCH_MARKER') });
        }
        const stream = streams.get(frame.id);
        if (!stream) return;
        if (!sending && frame.type === 'head' && stream.events && frame.status === 200) metric({ kind: 'sse' });
        if (sending && stream.kind === 'terminal' && frame.type === 'message_start') {
          if (frame.length > 96 * 1024 || stream.input !== null) throw new Error();
          stream.input = '';
        }
        if (frame.type === 'chunk') {
          if (sending && stream.kind === 'terminal') {
            if (stream.input === null) throw new Error();
            stream.input += atob(frame.data);
            if (stream.input.length > 96 * 1024) throw new Error();
          } else if (!sending && stream.identityResponse) {
            stream.response += atob(frame.data);
            if (stream.response.length > 2 * 1024 * 1024) throw new Error();
          }
        }
        if (sending && stream.kind === 'terminal' && frame.type === 'message_end') {
          const command = JSON.parse(stream.input); stream.input = null;
          if (command.command?.type === 'input') metric({ kind: 'input' });
        }
        if (!sending && frame.type === 'end') {
          if (stream.identityResponse && stream.response) {
            const value = JSON.parse(stream.response);
            if (stream.identityResponse === 'application') state.application = { instanceId: value.instanceId, remoteOwner: value.remoteOwner, directoryPickerAvailable: value.directoryPickerAvailable, directoryBrowserAvailable: value.directoryBrowserAvailable, launchAvailable: value.launchAvailable };
            else state.runEpoch = value.runEpoch;
          }
          streams.delete(frame.id);
        }
        if (frame.type === 'reset') streams.delete(frame.id);
      } catch { failure(); }
    };
    const subtle = crypto.subtle, encrypt = subtle.encrypt.bind(subtle), decrypt = subtle.decrypt.bind(subtle);
    subtle.encrypt = (algorithm, key, data) => { observe(algorithm, data, true); return encrypt(algorithm, key, data); };
    subtle.decrypt = async (algorithm, key, data) => { const plaintext = await decrypt(algorithm, key, data); observe(algorithm, plaintext, false); return plaintext; };
  });
}
module.exports = { observeSecurePage, trackDocumentSockets };
