// @vitest-environment node
import { expect, it } from 'vitest';
import { RecordCipher, ephemeral, transcript, fromBase64, toBase64, type ChannelBinding } from './secureCrypto';

export const binding: ChannelBinding = {
  wireVersion: 3, apiVersion: 3,
  accountId: '11111111-1111-4111-8111-111111111111', parentSessionId: '22222222-2222-4222-8222-222222222222',
  grantId: '33333333-3333-4333-8333-333333333333', gatewayId: '44444444-4444-4444-8444-444444444444',
  instanceId: '55555555-5555-4555-8555-555555555555', connectionEpoch: '66666666-6666-4666-8666-666666666666',
  channelId: '77777777-7777-4777-8777-777777777777', scope: { kind: 'run', runId: '88888888-8888-4888-8888-888888888888' }
};
async function pair(value = binding) {
  const client = await ephemeral(), server = await ephemeral();
  return Promise.all([
    RecordCipher.create(value, client.privateKey, client.publicKey, server.publicKey),
    RecordCipher.create(value, server.privateKey, client.publicKey, server.publicKey, true)
  ]);
}
it('uses real WebCrypto with separate directional keys, Finished zero and strict counters', async () => {
  const [client, server] = await pair();
  const first = await client.seal({ type: 'finished' });
  expect(new DataView(first.buffer).getBigUint64(0)).toBe(0n);
  expect(await server.open(first)).toEqual({ type: 'finished' });
  await expect(server.open(first)).rejects.toThrow('secure_counter');
  const response = await server.seal({ type: 'finished' });
  expect(await client.open(response)).toEqual({ type: 'finished' });
  await expect(client.open(await client.seal({ type: 'open', id: 1 }))).rejects.toThrow();
  const next = await client.seal({ type: 'end', id: 1 });
  await expect(server.open(next)).rejects.toThrow('secure_counter');
});
it('authenticates transcript identities, scope and ciphertext', async () => {
  const browser = await ephemeral(), desktop = await ephemeral();
  const client = await RecordCipher.create(binding, browser.privateKey, browser.publicKey, desktop.publicKey);
  const changed = await RecordCipher.create({ ...binding, scope: { kind: 'application' } }, desktop.privateKey, browser.publicKey, desktop.publicKey, true);
  const record = await client.seal({ type: 'finished' });
  await expect(changed.open(record)).rejects.toThrow();
  const server = await RecordCipher.create(binding, desktop.privateKey, browser.publicKey, desktop.publicKey, true);
  record[record.length - 1] ^= 1;
  await expect(server.open(record)).rejects.toThrow();
  record[record.length - 1] ^= 1;
  expect(await server.open(record)).toEqual({ type: 'finished' });
  expect(transcript(binding, browser.publicKey, desktop.publicKey).length).toBe(287);
});
it('applies record and byte caps in both directions, including Finished', async () => {
  const [client, server] = await pair();
  (client as any).sendCounter = BigInt(2 ** 20);
  await expect(client.seal({ type: 'finished' })).rejects.toThrow('secure_counter');
  (client as any).sendCounter = 0n;
  (client as any).sentBytes = 1024 ** 3;
  await expect(client.seal({ type: 'finished' })).rejects.toThrow('secure_capacity');
  (client as any).sentBytes = 0;
  const record = await client.seal({ type: 'finished' });
  (server as any).receiveCounter = BigInt(2 ** 20);
  await expect(server.open(record)).rejects.toThrow('secure_record');
  (server as any).receiveCounter = 0n;
  (server as any).receivedBytes = 1024 ** 3;
  await expect(server.open(record)).rejects.toThrow('secure_record');
});
it('accepts the complete 64KiB plaintext boundary and canonical standard base64 only', async () => {
  const [client, server] = await pair();
  const overhead = JSON.stringify({ type: 'x', data: '' }).length;
  const frame = { type: 'x', data: 'a'.repeat(65536 - overhead) };
  const record = await client.seal(frame);
  expect(record.length).toBe(65560);
  expect(await server.open(record)).toEqual(frame);
  await expect(client.seal({ ...frame, data: frame.data + 'a' })).rejects.toThrow('secure_capacity');
  expect(fromBase64(toBase64(new Uint8Array([255, 254])), 2)).toEqual(new Uint8Array([255, 254]));
  for (const invalid of ['__4=', '//5=', 'YQ', 'YQ==\n']) expect(() => fromBase64(invalid, 10)).toThrow();
});
