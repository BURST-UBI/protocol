import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const updater = new URL('./update-node.sh', import.meta.url).pathname;
const installer = readFileSync(new URL('../install.sh', import.meta.url), 'utf8');
const source = readFileSync(updater, 'utf8');
const body = 'new verified binary';
const hash = createHash('sha256').update(body).digest('hex');

test('installer and updater use the same verified download implementation', () => {
  assert.equal(
    installer.slice(installer.indexOf('download_verified_binary()'), installer.indexOf('xml_text()')).trim(),
    source.slice(source.indexOf('download_verified_binary()'), source.indexOf('# Arguments')).trim(),
  );
});

function run(platform, scenario) {
  const directory = mkdtempSync(join(tmpdir(), 'burst-update-'));
  try {
    const mocks = join(directory, 'tools');
    mkdirSync(mocks);
    const log = join(directory, 'service.log');
    const binary = join(directory, 'daemon with % & " spaces');
    writeFileSync(binary, scenario === 'unchanged' ? body : 'previous bytes', { mode: 0o755 });
    writeFileSync(log, '');
    writeFileSync(join(mocks, 'curl'), `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
if (!args.includes('--proto') || !args.includes('--proto-redir')) process.exit(88);
const sums = args.at(-1).endsWith('/SHA256SUMS');
if (process.env.SCENARIO === 'download' && !sums) process.exit(22);
fs.writeFileSync(args[args.indexOf('-o') + 1], sums ? process.env.MANIFEST : process.env.BODY);
`, { mode: 0o700 });
    const serviceMock = `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.LOG, JSON.stringify(args) + '\\n');
if (args[0] === 'list') {
  if (process.env.SCENARIO === 'stopped') process.exit(1);
  console.log('PID Status Label');
  console.log('999 0 burst-mock-other');
  console.log((process.env.SCENARIO === 'loaded-stopped' ? '-' : '123') + ' 0 burst-mock');
  process.exit(process.env.SCENARIO === 'query-failed' ? 1 : 0);
}
if (['is-active', 'print'].includes(args[0])) process.exit(process.env.SCENARIO === 'stopped' ? 1 : 0);
if (['restart', 'kickstart'].includes(args[0])) process.exit(process.env.SCENARIO === 'restart' ? 1 : 0);
process.exit(99);
`;
    for (const tool of ['systemctl', 'launchctl']) writeFileSync(join(mocks, tool), serviceMock, { mode: 0o700 });
    const entry = `${hash}  burst-daemon-${platform}\n`;
    if (scenario === 'locked') mkdirSync(`${binary}.update-lock`);
    const result = spawnSync('sh', [updater, binary, platform, 'burst-mock'], {
      env: { ...process.env, PATH: `${mocks}:${process.env.PATH}`, LOG: log, SCENARIO: scenario,
        BODY: scenario === 'tampered' ? 'bad bytes' : body,
        MANIFEST: scenario === 'duplicate' ? entry + entry : entry },
      encoding: 'utf8', timeout: 10_000,
    });
    assert.ifError(result.error);
    const files = readdirSync(directory);
    const backups = files.filter(name => name.includes('.previous.'));
    if (scenario !== 'locked') assert.ok(!files.some(name => name.includes('.update-lock')), 'staging lock leaked');
    return { ...result, contents: readFileSync(binary, 'utf8'),
      backups: backups.map(name => readFileSync(join(directory, name), 'utf8')),
      calls: readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) };
  } finally { rmSync(directory, { recursive: true, force: true }); }
}

test('verified updates retain the old binary and restart only running services', () => {
  for (const platform of ['linux-amd64', 'darwin-arm64']) {
    for (const scenario of ['success', 'stopped', 'unchanged', 'restart']) {
      const result = run(platform, scenario);
      assert.equal(result.status, scenario === 'restart' ? 1 : 0, result.stderr);
      assert.equal(result.contents, body);
      assert.deepEqual(result.backups, scenario === 'unchanged' ? [] : ['previous bytes']);
      assert.equal(result.calls.length, scenario === 'unchanged' ? 0 : scenario === 'stopped' ? 1 : 2);
      if (result.calls.length === 2) assert.ok(result.calls[1].at(-1).endsWith('burst-mock'));
    }
  }
});

test('bad downloads and concurrent attempts leave the service and binary untouched', () => {
  for (const scenario of ['tampered', 'duplicate', 'download', 'locked']) {
    const result = run('linux-amd64', scenario);
    assert.notEqual(result.status, 0, result.stderr);
    assert.equal(result.contents, 'previous bytes');
    assert.deepEqual(result.backups, []);
    assert.deepEqual(result.calls, []);
  }
});

test('macOS update does not start a stopped job or trust a failed status query', () => {
  for (const scenario of ['loaded-stopped', 'query-failed']) {
    const result = run('darwin-arm64', scenario);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.contents, body);
    assert.deepEqual(result.backups, ['previous bytes']);
    assert.equal(result.calls.length, 1, 'loaded is not the same as running');
    assert.ok(!result.calls.some(call => call[0] === 'kickstart'));
  }
});

test('installer escapes TOML, launchd XML and systemd path arguments', () => {
  const helpers = installer.slice(installer.indexOf('xml_text()'), installer.indexOf('# ── Detect platform'));
  const value = 'space & <xml> "quote" \\slash %specifier ${BURST_PATH}';
  for (const [helper, expected] of [
    ['xml_text', 'space &amp; &lt;xml&gt; &quot;quote&quot; \\slash %specifier'],
    ['toml_string', '"space & <xml> \\"quote\\" \\\\slash %specifier"'],
    ['systemd_arg', '"space & <xml> \\"quote\\" \\\\slash %%specifier"'],
  ]) {
    const result = spawnSync('sh', ['-c', `${helpers}\n${helper} "$1"`, 'test', value], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, expected.replace(/"?$/, match => ` \${BURST_PATH}${match}`), helper);
  }
  for (const line of installer.split('\n').filter(line => line.startsWith('ExecStart='))) {
    assert.ok(line.startsWith('ExecStart=:'), 'systemd must not expand environment variables in literal paths');
  }
});
