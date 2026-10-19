import { spawnSync } from 'node:child_process';
import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { hostname } from 'node:os';

for (const requestedHost of ['', 'not-this-validation-host']) {
  test(`NAT runner refuses missing or mismatched host opt-in (${requestedHost || 'missing'})`, () => {
    const result = spawnSync('bash', ['run-nat-validation.sh'], {
      cwd: new URL('..', import.meta.url),
      env: { ...process.env, BURST_NAT_VALIDATION_HOST: requestedHost },
      encoding: 'utf8',
      timeout: 5000,
    });
    assert.equal(result.status, 2);
    assert.match(result.stderr, /Desktop release builds are prohibited/);
    assert.equal(result.stdout, '');
  });
}

test('NAT runner requires reusable artifacts instead of silently rebuilding', () => {
  const result = spawnSync('bash', ['run-nat-validation.sh'], {
    cwd: new URL('..', import.meta.url),
    env: { ...process.env, BURST_NAT_VALIDATION_HOST: hostname(), BURST_NAT_ARTIFACTS: '' },
    encoding: 'utf8', timeout: 5000,
  });
  assert.equal(result.status, 2);
  assert.match(result.stderr, /Set BURST_NAT_ARTIFACTS|at least 6 GiB/);
  assert.equal(result.stdout, '');
});

test('Docker runtime is independently bounded and uses the inspected image identity', () => {
  const source = readFileSync(new URL('../run-nat-validation.sh', import.meta.url), 'utf8');
  assert.doesNotMatch(source, /docker build|cargo build|--cpus 2|--memory 4g/);
  assert.match(source, /--memory 3g --memory-swap 3g --cpus 1 --pids-limit 256/);
  assert.match(source, /"\$lab_image" timeout --signal=TERM --kill-after=10s 20m bash \/lab\/lab.sh/);
  assert.match(source, /--network none --privileged/);
  assert.doesNotMatch(source, /--network host|docker.sock/);
});
