// In-memory interoperability peer: imports the actual browser crypto implementation.
// Private keys never leave this process, and no files or credentials are accessed.
import { createInterface } from 'node:readline';
import { ephemeral, RecordCipher, fromBase64, toBase64 } from '../../web/src/workbench/secureCrypto.ts';
const lines = createInterface({ input: process.stdin });
const pair = await ephemeral();
process.stdout.write(JSON.stringify({ publicKey: toBase64(pair.publicKey) }) + '\n');
for await (const line of lines) {
  const { binding, serverKey, finished } = JSON.parse(line);
  const cipher = await RecordCipher.create(binding, pair.privateKey, pair.publicKey, fromBase64(serverKey, 65));
  if ((await cipher.open(fromBase64(finished, 65560))).type !== 'finished') throw new Error('finished_required');
  const response = { finished: toBase64(await cipher.seal({ type: 'finished' })), end: toBase64(await cipher.seal({ type: 'end', id: 7 })) };
  process.stdout.write(JSON.stringify(response) + '\n');
  break;
}
lines.close();
