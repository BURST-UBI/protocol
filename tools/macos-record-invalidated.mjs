import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { inventory, finish, audit } from './validation-reuse.mjs';

const root = resolve(import.meta.dirname, '..');
const evidence = resolve(root, 'macos-candidate-v3-evidence');
process.chdir(resolve(root, 'candidate-source'));
const json = async name => JSON.parse(await readFile(resolve(evidence, name), 'utf8'));
const start = await json('daemon-final.start.json');
const original = await json('daemon-final.result.json');
if (original.outcome !== 'fail' || original.exitCode !== 124) throw Error('Unexpected terminal result');
const observed = await inventory(start.plan.inputs);
const before = new Map(start.inputs.map(row => [row[0], row]));
const after = new Map(observed.map(row => [row[0], row]));
const changes = [...new Set([...before.keys(), ...after.keys()])].filter(path =>
  JSON.stringify(before.get(path)) !== JSON.stringify(after.get(path)));
const node = resolve(root, '.macos-target/x86_64-apple-darwin/release/build/burst-node/2bcae47e8e580fc8/out/libburst_node-2bcae47e8e580fc8.rlib');
await writeFile(resolve(evidence, 'input-change-review.json'), JSON.stringify({ changes,
  before: before.get(node), after: after.get(node),
  scope: 'Read-only comparison against latest start; no compilation or acceptance' }, null, 2), { flag: 'wx' });
if (changes.length !== 1 || changes[0] !== node) throw Error('Additional input changes need review');
const result = { ...original,
  inputMutationReason: 'Retained Cargo log records node rebuild after ring deployment-setting change and LMDB directory timestamp invalidation; cache node output was incorrectly declared immutable. Restored daemon-v3 cache differs from original start only at this rlib. Original timeout/cleanup failure unchanged.',
  logs: [...original.logs, resolve(evidence, 'daemon-final.result.json'), resolve(evidence, 'input-change-review.json')],
};
await writeFile(resolve(evidence, 'daemon-final.invalidated-result.json'), JSON.stringify(result, null, 2), { flag: 'wx' });
const receiptPath = resolve(evidence, 'daemon-final.invalidated-receipt.json');
await writeFile(receiptPath, JSON.stringify(await finish(start, result, true), null, 2), { flag: 'wx' });
const stages = [{ name: 'daemon-final', plan: resolve(evidence, 'daemon-final.plan.json'), receipt: receiptPath }];
const decisions = await audit(stages);
if (decisions[0].status !== 'unverified') throw Error('Invalidated attempt unexpectedly qualified reuse');
await writeFile(resolve(evidence, 'invalidated-audit.json'), JSON.stringify(decisions, null, 2), { flag: 'wx' });
console.log('Recorded latest failed/invalidated attempt; reuse remains unverified. No compiler ran.');
