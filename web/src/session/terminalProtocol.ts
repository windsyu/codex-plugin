import type { TerminalSnapshotFrame } from '../types';

export type TerminalClientFrame =
  | { type: 'input'; leaseId: string; data: string }
  | { type: 'resize'; cols: number; rows: number }
  | { type: 'ack'; outputSeq: number }
  | { type: 'requestSnapshot'; afterSeq?: number };

export interface TerminalOutputFrame {
  outputSeq: number;
  data: Uint8Array;
}

export function terminalProtocols(descriptor: string) {
  if (!/^[A-Za-z0-9._-]{32,160}$/.test(descriptor)) throw new Error('Invalid terminal attachment descriptor');
  return ['codex-terminal-v1', `codex-attach.${descriptor}`];
}

export function terminalWebSocketUrl(workerId: string) {
  const scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${scheme}//${window.location.host}/v2/sessions/${encodeURIComponent(workerId)}/terminal`;
}

export function encodeClientFrame(frame: TerminalClientFrame) {
  return JSON.stringify(frame);
}

export function decodeOutputFrame(value: ArrayBuffer): TerminalOutputFrame {
  const bytes = new Uint8Array(value);
  if (bytes.length < 9 || bytes[0] !== 1) throw new Error('Unsupported terminal binary frame');
  const view = new DataView(value);
  const high = view.getUint32(1, false);
  const low = view.getUint32(5, false);
  const outputSeq = high * 2 ** 32 + low;
  if (!Number.isSafeInteger(outputSeq)) throw new Error('Terminal output sequence is unsafe');
  return { outputSeq, data: bytes.slice(9) };
}

export function decodeBase64Bytes(value: string) {
  const decoded = atob(value);
  const bytes = new Uint8Array(decoded.length);
  for (let index = 0; index < decoded.length; index += 1) bytes[index] = decoded.charCodeAt(index);
  return bytes;
}

export function decodeSnapshot(snapshot: TerminalSnapshotFrame) {
  if (snapshot.encoding !== 'base64') throw new Error('Unsupported terminal snapshot encoding');
  return {
    screen: decodeBase64Bytes(snapshot.screen),
    replay: decodeBase64Bytes(snapshot.replay)
  };
}
