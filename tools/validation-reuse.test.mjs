import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm, symlink, cp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { begin, finish, check, prepare, audit } from './validation-reuse.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'burst-evidence-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const input = join(root, 'input'), log = join(root, 'run.log');
  await mkdir(input);
  await writeFile(join(input, 'driver'), 'assert confirmed balance');
  await writeFile(log, 'PASS confirmed balance');
  const plan = { requirement: 'P4', scope: 'exact confirmed balance after restart',
    command: ['node', 'driver'], inputs: [input], context: {
      toolchain: 'node fixture', features: 'private-testnet', network: 'disposable',
      topology: 'loopback', limits: '1 CPU / 3 GiB / no swap',
    } };
  const result = { outcome: 'pass', exitCode: 0, assertions: 1,
    review: 'Confirmed balance matched the pinned fixture after restart.', logs: [log] };
  return { root, input, log, plan, result };
}

test('stage audit preserves a passed build when runtime failed or evidence is missing', async t => {
  const f = await fixture(t);
  const plan = join(f.root, 'plan.json');
  await writeFile(plan, JSON.stringify(f.plan));
  const stages = [];
  for (const outcome of ['pass', 'fail', 'incomplete']) {
    const receipt = join(f.root, `${outcome}.json`);
    await writeFile(receipt, JSON.stringify(await finish(await begin(f.plan), {
      ...f.result, outcome, exitCode: outcome === 'pass' ? 0 : 1,
    })));
    stages.push({ name: outcome, plan, receipt });
  }
  stages.push({ name: 'missing', plan, receipt: join(f.root, 'missing.json') });
  assert.deepEqual((await audit(stages)).map(stage => stage.status),
    ['reuse', 'diagnose', 'diagnose', 'unverified']);
  await writeFile(join(f.input, 'driver'), 'changed dependency');
  assert.ok((await audit(stages)).every(stage => stage.status === 'unverified'));
  await assert.rejects(audit([stages[0], stages[0]]), /unique/);
  await assert.rejects(audit([]), /stages/);
});

test('audit CLI reports missing evidence without authorizing execution', async t => {
  const f = await fixture(t), manifest = join(f.root, 'stages.json');
  await writeFile(manifest, JSON.stringify([{ name: 'runtime',
    plan: join(f.root, 'absent.json'), receipt: join(f.root, 'absent-receipt.json') }]));
  const script = new URL('./validation-reuse.mjs', import.meta.url).pathname;
  const result = spawnSync(process.execPath, [script, 'audit', manifest], { encoding: 'utf8' });
  assert.equal(result.status, 1);
  assert.equal(JSON.parse(result.stdout)[0].status, 'unverified');
});

test('unchanged input and reviewed evidence can be reused without execution', async t => {
  const { plan, result } = await fixture(t);
  const receipt = await finish(await begin(plan), result);
  assert.equal((await check(plan, receipt)).outcome, 'pass');
  assert.match((await check(plan, receipt)).action, /no release approval/);
});

test('content changes during a run invalidate the result', async t => {
  const f = await fixture(t), start = await begin(f.plan);
  await writeFile(join(f.input, 'driver'), 'different assertion');
  await assert.rejects(finish(start, f.result), /Inputs changed/);
});

test('changed, added and deleted input files invalidate reuse', async t => {
  const f = await fixture(t);
  const receipt = await finish(await begin(f.plan), f.result);
  await writeFile(join(f.input, 'new-untracked-fixture'), 'new');
  await assert.rejects(check(f.plan, receipt), /inventory changed/);
  await rm(join(f.input, 'new-untracked-fixture'));
  await writeFile(join(f.input, 'driver'), 'changed');
  await assert.rejects(check(f.plan, receipt), /inventory changed/);
  await rm(join(f.input, 'driver'));
  await assert.rejects(check(f.plan, receipt), /inventory changed/);
});

test('changed context, command and scope cannot inherit a pass', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  for (const plan of [
    { ...f.plan, scope: 'all production behavior' },
    { ...f.plan, command: ['different-driver'] },
    { ...f.plan, context: { ...f.plan.context, topology: 'cross-host' } },
    { ...f.plan, context: { ...f.plan.context, limits: 'unlimited' } },
  ]) await assert.rejects(check(plan, receipt), /changed/);
});

test('missing or changed logs invalidate reuse', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  await writeFile(f.log, 'altered');
  await assert.rejects(check(f.plan, receipt), /Evidence changed/);
  await rm(f.log);
  await assert.rejects(check(f.plan, receipt), /ENOENT/);
});

test('zero tests, failed exit and absent review cannot create a pass', async t => {
  const f = await fixture(t), start = await begin(f.plan);
  for (const change of [{ assertions: 0 }, { exitCode: 1 }, { review: '' }, { logs: [] }]) {
    await assert.rejects(finish(start, { ...f.result, ...change }));
  }
});

test('failed evidence remains failed and discourages unchanged retries', async t => {
  const f = await fixture(t);
  const receipt = await finish(await begin(f.plan), { ...f.result, outcome: 'fail', exitCode: 1 });
  const decision = await check(f.plan, receipt);
  assert.equal(decision.outcome, 'fail');
  assert.match(decision.action, /Do not repeat/);
});

test('symlinks and underspecified plans fail closed', async t => {
  const f = await fixture(t);
  await assert.rejects(begin({ ...f.plan, inputs: [] }));
  await assert.rejects(begin({ ...f.plan, context: {} }));
  await symlink(f.log, join(f.input, 'linked'));
  await assert.rejects(begin(f.plan), /symlink/);
});

test('CLI records are write-once', async t => {
  const f = await fixture(t), plan = join(f.root, 'plan.json'), start = join(f.root, 'start.json');
  await writeFile(plan, JSON.stringify(f.plan));
  const script = new URL('./validation-reuse.mjs', import.meta.url).pathname;
  const run = () => spawnSync(process.execPath, [script, 'begin', plan, start], { encoding: 'utf8' });
  assert.equal(run().status, 0);
  const second = run();
  assert.equal(second.status, 1);
  assert.match(second.stderr, /EEXIST/);
});

test('preflight refuses unchanged passes, failures and incomplete attempts', async t => {
  const f = await fixture(t);
  for (const outcome of ['pass', 'fail', 'incomplete']) {
    const receipt = await finish(await begin(f.plan), { ...f.result, outcome });
    await assert.rejects(prepare(f.plan, receipt), /Unchanged validation/);
  }
});

test('preflight allows changed inputs and records why a new attempt is needed', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  await writeFile(join(f.input, 'driver'), 'assert new behavior');
  const start = await prepare(f.plan, receipt);
  assert.deepEqual(start.previousAttempt.changed, ['input contents or inventory']);
  assert.equal((await finish(start, f.result)).result.outcome, 'pass');
});

test('preflight allows a different question without inheriting its prior pass', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  const start = await prepare({ ...f.plan, scope: 'different reviewed behavior',
    rerun: { reason: 'Check restart behavior; prior run checked only initial confirmation.', attemptBudget: 1 },
  }, receipt);
  assert.deepEqual(start.previousAttempt.changed, ['scope, command or context']);
  assert.equal(start.result, undefined);
});

test('renaming a scope or changing command alone cannot schedule unchanged inputs', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  for (const change of [{ scope: 'renamed' }, { command: ['retry-driver'] }]) {
    await assert.rejects(prepare({ ...f.plan, ...change }, receipt), /rerun.reason/);
  }
  await assert.rejects(prepare({ ...f.plan, scope: 'timing diagnosis',
    rerun: { reason: 'Investigate scheduler variance', attemptBudget: 0 },
  }, receipt), /attemptBudget/);
});

test('missing evidence does not silently authorize an unchanged retry', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  await rm(f.log);
  await assert.rejects(prepare(f.plan, receipt), /ENOENT/);
});

test('renamed checks cannot reset or exceed the unchanged-input retry budget', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  const plan = { ...f.plan, scope: 'diagnose timing',
    rerun: { reason: 'Measure scheduler variance', attemptBudget: 1 } };
  const retry = await finish(await prepare(plan, receipt), f.result);
  assert.deepEqual(retry.retrySequence, { used: 1, budget: 1 });
  await assert.rejects(prepare({ ...plan, scope: 'another label' }, retry), /budget exhausted/);
  await assert.rejects(prepare({ ...plan, rerun: { ...plan.rerun, attemptBudget: 2 } }, retry), /Cannot raise/);
  await writeFile(join(f.input, 'driver'), 'fixed measured cause');
  const changed = await prepare(plan, retry);
  assert.equal(changed.retrySequence, undefined);
});

test('changing scope cannot bypass missing or altered prior evidence', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  const plan = { ...f.plan, scope: 'renamed',
    rerun: { reason: 'New question', attemptBudget: 1 } };
  await writeFile(f.log, 'replaced evidence');
  await assert.rejects(prepare(plan, receipt), /Prior evidence changed/);
  await rm(f.log);
  await assert.rejects(prepare(plan, receipt), /ENOENT/);
});

test('relocating identical inputs cannot authorize a rerun or inherit a pass', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  const copied = join(f.root, 'copied-input');
  await cp(f.input, copied, { recursive: true });
  const plan = { ...f.plan, inputs: [copied] };
  await assert.rejects(prepare(plan, receipt), /rerun.reason/);
  await assert.rejects(check(plan, receipt), /changed/);
  const justified = { ...plan, rerun: {
    reason: 'Check a path-sensitive driver in its relocated workspace', attemptBudget: 1,
  } };
  const retry = await finish(await prepare(justified, receipt), f.result);
  assert.deepEqual(retry.retrySequence, { used: 1, budget: 1 });
  await assert.rejects(prepare({ ...justified, inputs: [f.input] }, retry), /budget exhausted/);
});

test('changed content in a relocated input authorizes new validation, never reuse', async t => {
  const f = await fixture(t), receipt = await finish(await begin(f.plan), f.result);
  const copied = join(f.root, 'changed-copy');
  await cp(f.input, copied, { recursive: true });
  await writeFile(join(copied, 'driver'), 'assert additional recovery state');
  const start = await prepare({ ...f.plan, inputs: [copied] }, receipt);
  assert.equal(start.retrySequence, undefined);
  assert.equal(start.result, undefined);
});
