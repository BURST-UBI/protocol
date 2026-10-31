import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const directory = process.argv[2];
if (!directory) throw new Error('Usage: node fuzz/protobuf-seeds.mjs <corpus-directory>');
await mkdir(directory, { recursive: true });
// Independent minimal envelopes from messages/wire.proto; both raw and TCP framed.
const messages = {
  legacy_telemetry: [8, 30, 170, 1, 0],
  keepalive: [8, 38, 114, 0],
  telemetry: [8, 38, 170, 1, 0],
  confirmation: [8, 38, 98, 0],
  bootstrap: [8, 38, 122, 4, 8, 1, 106, 0],
  sync: [8, 38, 186, 1, 2, 10, 0],
};
for (const [name, bytes] of Object.entries(messages)) {
  const body = Buffer.from(bytes);
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  await writeFile(join(resolve(directory), `${name}.bin`), body, { flag: 'wx' });
  await writeFile(join(resolve(directory), `${name}-framed.bin`), Buffer.concat([header, body]), { flag: 'wx' });
}
console.log(`Wrote ${Object.keys(messages).length * 2} Protobuf seed inputs.`);
