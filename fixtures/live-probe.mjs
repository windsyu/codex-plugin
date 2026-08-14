import crypto from 'node:crypto';
import net from 'node:net';

const [socketPath, cwd, resumeThreadId] = process.argv.slice(2);
if (!socketPath || !cwd) {
  throw new Error('usage: node fixtures/live-probe.mjs <socket-path> <cwd> [resume-thread-id]');
}

const socket = net.createConnection({ path: socketPath });
let buffer = Buffer.alloc(0);
let upgraded = false;
const key = crypto.randomBytes(16).toString('base64');

socket.on('connect', () => {
  socket.write([
    'GET / HTTP/1.1',
    'Host: localhost',
    'Upgrade: websocket',
    'Connection: Upgrade',
    `Sec-WebSocket-Key: ${key}`,
    'Sec-WebSocket-Version: 13',
    '',
    '',
  ].join('\r\n'));
});

socket.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  if (!upgraded) {
    const end = buffer.indexOf('\r\n\r\n');
    if (end < 0) return;
    const header = buffer.subarray(0, end).toString('utf8');
    if (!header.startsWith('HTTP/1.1 101')) throw new Error(`upgrade failed: ${header}`);
    buffer = buffer.subarray(end + 4);
    upgraded = true;
    sendJson({
      method: 'initialize', id: 1,
      params: { clientInfo: { name: 'codex_local_observer_probe', title: 'Observer Probe', version: '0.1.0' }, capabilities: { experimentalApi: false } },
    });
  }
  drainFrames();
});

socket.on('error', (error) => {
  process.stderr.write(`${error.stack}\n`);
  process.exitCode = 1;
});

function drainFrames() {
  while (buffer.length >= 2) {
    const opcode = buffer[0] & 0x0f;
    let length = buffer[1] & 0x7f;
    let offset = 2;
    if (length === 126) {
      if (buffer.length < 4) return;
      length = buffer.readUInt16BE(2);
      offset = 4;
    } else if (length === 127) {
      if (buffer.length < 10) return;
      length = Number(buffer.readBigUInt64BE(2));
      offset = 10;
    }
    if (buffer.length < offset + length) return;
    const payload = buffer.subarray(offset, offset + length);
    buffer = buffer.subarray(offset + length);
    if (opcode === 1) onJson(JSON.parse(payload.toString('utf8')));
    if (opcode === 8) socket.end();
  }
}

function onJson(message) {
  if (message.id === 1 && message.result) {
    sendJson({ method: 'initialized' });
    sendJson(resumeThreadId
      ? { method: 'thread/resume', id: 2, params: { threadId: resumeThreadId } }
      : { method: 'thread/start', id: 2, params: { cwd, ephemeral: false } });
  } else if (message.id === 2) {
    if (message.error) throw new Error(`thread/start failed: ${JSON.stringify(message.error)}`);
    process.stdout.write(`${JSON.stringify({ threadId: message.result.thread.id })}\n`);
    sendFrame(Buffer.alloc(0), 8);
    setTimeout(() => socket.end(), 250);
  }
}

function sendJson(value) {
  sendFrame(Buffer.from(JSON.stringify(value)), 1);
}

function sendFrame(payload, opcode) {
  const mask = crypto.randomBytes(4);
  let header;
  if (payload.length < 126) {
    header = Buffer.from([0x80 | opcode, 0x80 | payload.length]);
  } else if (payload.length <= 0xffff) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 126;
    header.writeUInt16BE(payload.length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 0x80 | 127;
    header.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  const masked = Buffer.alloc(payload.length);
  for (let index = 0; index < payload.length; index += 1) {
    masked[index] = payload[index] ^ mask[index % 4];
  }
  socket.write(Buffer.concat([header, mask, masked]));
}
