import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, copyFileSync, chmodSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const script = fileURLToPath(new URL('./limited-run.sh', import.meta.url));

function run(command, status = 0, available = '8388608') {
  const directory = mkdtempSync(join(tmpdir(), 'burst-limits-'));
  try {
    const mock = join(directory, 'systemd-run');
    const log = join(directory, 'args.json');
    copyFileSync(new URL('./fixtures/limited-systemd-run.mjs', import.meta.url), mock);
    chmodSync(mock, 0o700);
    copyFileSync(mock, join(directory, 'awk'));
    chmodSync(join(directory, 'awk'), 0o700);
    const result = spawnSync('bash', [script, ...command], {
      env: { ...process.env, PATH: `${directory}:${process.env.PATH}`, MOCK_LIMITED_LOG: log, MOCK_LIMITED_STATUS: String(status), MOCK_AVAILABLE_KIB: available },
      encoding: 'utf8', timeout: 5000,
    });
    assert.ifError(result.error);
    return { ...result, args: existsSync(log) ? JSON.parse(readFileSync(log, 'utf8')) : [] };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('local jobs receive fixed process-tree limits and literal arguments', { skip: process.platform !== 'linux' }, () => {
  const command = ['printf', '%s', '$HOME; not a shell command'];
  const result = run(command);
  assert.equal(result.status, 0, result.stderr);
  for (const flag of ['--user', '--wait', '--collect', '--expand-environment=no', '--unit=burst-local-validation',
    'MemoryHigh=2500M', 'MemoryMax=3G', 'MemorySwapMax=0', 'CPUQuota=100%', 'Nice=19', 'TasksMax=64',
    'OOMPolicy=kill', 'RuntimeMaxSec=20min', '--setenv=CARGO_BUILD_JOBS=1', '--setenv=BURST_TEST_CPUS=1']) {
    assert(result.args.includes(flag), flag);
  }
  assert.deepEqual(result.args.slice(result.args.indexOf('--') + 1), command);
});

test('failed or overlapping systemd jobs never fall back to uncapped execution', { skip: process.platform !== 'linux' }, () => {
  assert.equal(run(['true'], 1).status, 1);
  const missing = run([]);
  assert.equal(missing.status, 2);
  assert.deepEqual(missing.args, []);
});

test('low or unknown available memory defers work before starting a service', { skip: process.platform !== 'linux' }, () => {
  for (const available of ['0', '4194303', '', 'unknown']) {
    const result = run(['true'], 0, available);
    assert.equal(result.status, 75);
    assert.deepEqual(result.args, []);
  }
});
