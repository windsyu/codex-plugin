/** The transcript is byte-for-byte shared with relay-protocol::secure. Keys live only in this page. */
export interface AccessContext {
  wireVersion: number; apiVersion: number; accountId: string; parentSessionId: string;
  grantId: string; gatewayId: string; instanceId: string; connectionEpoch: string;
}
export type ChannelScope = { kind: 'application' } | { kind: 'run'; runId: string };
export interface ChannelBinding extends AccessContext { channelId: string; scope: ChannelScope }
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
export const CHUNK_BYTES = 16 * 1024;
export const RECORD_BYTES = 64 * 1024;
export const WIRE_BYTES = RECORD_BYTES + 24;
const MAX_RECORDS = 2 ** 20;
const MAX_BYTES = 1024 ** 3;
export const WINDOW_BYTES = 256 * 1024;
export function toBase64(bytes: Uint8Array): string {
  let result = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) result += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  return btoa(result);
}
export function fromBase64(value: string, limit: number): Uint8Array {
  if (typeof value !== 'string' || value.length > Math.ceil(limit / 3) * 4 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) throw new Error('secure_encoding');
  const raw = atob(value);
  if (raw.length > limit) throw new Error('secure_capacity');
  const bytes = Uint8Array.from(raw, c => c.charCodeAt(0));
  if (toBase64(bytes) !== value) throw new Error('secure_encoding');
  return bytes;
}
function uuidBytes(value: string): Uint8Array {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)) throw new Error('secure_identity');
  return Uint8Array.from(value.replaceAll('-', '').match(/../g)!, value => parseInt(value, 16));
}
export function transcript(binding: ChannelBinding, client: Uint8Array, server: Uint8Array): Uint8Array {
  if (binding.wireVersion !== 3 || binding.apiVersion !== 3 || client.length !== 65 || server.length !== 65 || client[0] !== 4 || server[0] !== 4) throw new Error('secure_version');
  const prefix = encoder.encode('codex-relay-e2ee-v3\0');
  const version = new Uint8Array(8); const view = new DataView(version.buffer);
  view.setUint32(0, binding.wireVersion); view.setUint32(4, binding.apiVersion);
  const fields = [prefix, version, ...[binding.accountId, binding.parentSessionId, binding.grantId, binding.gatewayId, binding.instanceId, binding.connectionEpoch, binding.channelId].map(uuidBytes),
    ...(binding.scope.kind === 'application' ? [new Uint8Array([0])] : [new Uint8Array([1]), uuidBytes(binding.scope.runId)]), client, server];
  const result = new Uint8Array(fields.reduce((sum, field) => sum + field.length, 0));
  let offset = 0; for (const field of fields) { result.set(field, offset); offset += field.length; }
  return result;
}
export function validBinding(binding: ChannelBinding, access: AccessContext, scope: ChannelScope) {
  if (!binding || binding.scope?.kind !== scope.kind || (scope.kind === 'run' && (binding.scope as { runId?: string }).runId !== scope.runId)) return false;
  return (['wireVersion', 'apiVersion', 'accountId', 'parentSessionId', 'grantId', 'gatewayId', 'instanceId', 'connectionEpoch'] as const).every(key => binding[key] === access[key]);
}
export async function ephemeral() {
  const keys = await crypto.subtle.generateKey({ name: 'ECDH', namedCurve: 'P-256' }, false, ['deriveBits']);
  const publicKey = new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey));
  return { privateKey: keys.privateKey, publicKey };
}
export class RecordCipher {
  private sendCounter = 0n;
  private receiveCounter = 0n;
  private sentBytes = 0;
  private receivedBytes = 0;
  constructor(private readonly sendKey: CryptoKey, private readonly receiveKey: CryptoKey, private readonly hash: Uint8Array) {}
  static async create(binding: ChannelBinding, privateKey: CryptoKey, client: Uint8Array, server: Uint8Array, desktop = false) {
    const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(transcript(binding, client, server))));
    const peer = await crypto.subtle.importKey('raw', new Uint8Array(desktop ? client : server), { name: 'ECDH', namedCurve: 'P-256' }, false, []);
    const shared = await crypto.subtle.deriveBits({ name: 'ECDH', public: peer }, privateKey, 256);
    const material = await crypto.subtle.importKey('raw', shared, 'HKDF', false, ['deriveKey']);
    new Uint8Array(shared).fill(0);
    const key = (direction: string) => crypto.subtle.deriveKey({ name: 'HKDF', hash: 'SHA-256', salt: hash, info: encoder.encode(`codex-relay-v3/${direction}`) }, material, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
    const [c2d, d2c] = await Promise.all([key('c2d'), key('d2c')]);
    return new RecordCipher(desktop ? d2c : c2d, desktop ? c2d : d2c, hash);
  }
  private parameters(counter: bigint) {
    const iv = new Uint8Array(12); new DataView(iv.buffer).setBigUint64(4, counter);
    const additionalData = new Uint8Array(40); additionalData.set(this.hash); additionalData.set(iv.subarray(4), 32);
    return { name: 'AES-GCM', iv, additionalData, tagLength: 128 };
  }
  async seal(frame: object): Promise<Uint8Array> {
    if (this.sendCounter >= BigInt(MAX_RECORDS)) throw new Error('secure_counter');
    const data = encoder.encode(JSON.stringify(frame));
    if (data.length > RECORD_BYTES || this.sentBytes + data.length > MAX_BYTES) throw new Error('secure_capacity');
    this.sentBytes += data.length;
    const counter = this.sendCounter++;
    const sealed = new Uint8Array(await crypto.subtle.encrypt(this.parameters(counter), this.sendKey, data));
    const record = new Uint8Array(8 + sealed.length); new DataView(record.buffer).setBigUint64(0, counter); record.set(sealed, 8);
    return record;
  }
  async open(record: Uint8Array): Promise<Record<string, any>> {
    if (record.length < 24 || record.length > WIRE_BYTES || this.receiveCounter >= BigInt(MAX_RECORDS) || this.receivedBytes + record.length - 24 > MAX_BYTES) throw new Error('secure_record');
    const counter = new DataView(record.buffer, record.byteOffset, record.byteLength).getBigUint64(0);
    if (counter !== this.receiveCounter) throw new Error('secure_counter');
    const plain = await crypto.subtle.decrypt(this.parameters(counter), this.receiveKey, new Uint8Array(record.subarray(8)));
    const frame = JSON.parse(decoder.decode(plain));
    if (!frame || typeof frame !== 'object' || Array.isArray(frame) || typeof frame.type !== 'string') throw new Error('secure_frame');
    this.receiveCounter++;
    this.receivedBytes += plain.byteLength;
    return frame;
  }
}
