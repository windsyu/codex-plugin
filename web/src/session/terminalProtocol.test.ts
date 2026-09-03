import { describe, expect, it } from 'vitest';
import { decodeBase64Bytes, decodeOutputFrame, terminalProtocols } from './terminalProtocol';

describe('terminal protocol', () => {
  it('decodes typed binary output without treating bytes as HTML', () => {
    const payload = new TextEncoder().encode('<img src=x onerror=alert(1)>');
    const frame = new Uint8Array(9 + payload.length);
    frame[0] = 1;
    new DataView(frame.buffer).setBigUint64(1, 42n, false);
    frame.set(payload, 9);
    const decoded = decodeOutputFrame(frame.buffer);
    expect(decoded.outputSeq).toBe(42);
    expect(new TextDecoder().decode(decoded.data)).toContain('<img');
  });

  it('rejects untyped frames and unsafe descriptors', () => {
    expect(() => decodeOutputFrame(new Uint8Array([2]).buffer)).toThrow();
    expect(() => terminalProtocols('../secret')).toThrow();
    expect(terminalProtocols('a'.repeat(64))).toEqual(['codex-terminal-v1', `codex-attach.${'a'.repeat(64)}`]);
  });

  it('restores binary snapshot bytes from base64', () => {
    expect([...decodeBase64Bytes('AP+A')]).toEqual([0, 255, 128]);
  });
});
