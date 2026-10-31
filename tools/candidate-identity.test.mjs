import assert from 'node:assert/strict';
import {createHash, generateKeyPairSync} from 'node:crypto';
import {mkdtemp, mkdir, writeFile, readFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('./validation/candidate-identity.sh', import.meta.url));
const fixture = '197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61';
const publicKey = generateKeyPairSync('ed25519').publicKey.export({format:'der', type:'spki'}).subarray(-32).toString('hex');
const run = (...args) => spawnSync('bash', [script, ...args], {encoding:'utf8'});
const hasDebianTools = ['dpkg', 'dpkg-deb'].every(command => spawnSync(command, ['--version']).status === 0);

test('source inventory retains build inputs without private development files', async () => {
  const root = await mkdtemp(join(tmpdir(), 'burst-source-inventory-'));
  try {
    const builder = await readFile(new URL('./validation/build-linux-candidate.sh', import.meta.url), 'utf8');
    const command = builder.match(/find protocol wallet node-gui \\\n[\s\S]*?> "\$output\/SOURCES\.sha256"/);
    assert(command, 'Source inventory command not found');
    const kept = [];
    for (const app of ['protocol', 'wallet', 'node-gui']) {
      for (const file of ['Cargo.toml', 'src/main.rs', 'public/icon.svg', 'public/references/icon.svg', '.cf2/log', '.codex/session.json',
        '.claude/session.json', '.git/config', 'target/output', 'node_modules/pkg/index.js', 'dist/index.html',
        '.env', '.env.local', 'deploy/validation-peer.env', 'creator.seed', 'private.pem', 'wallet.key',
        'id_rsa', 'id_ed25519', 'credentials', 'build.log', 'cache.tsbuildinfo']) {
        const path = join(root, app, file);
        await mkdir(dirname(path), {recursive:true});
        await writeFile(path, 'disposable fixture');
        if (['Cargo.toml', 'src/main.rs', 'public/icon.svg', 'public/references/icon.svg'].includes(file)) kept.push(`${app}/${file}`);
      }
    }
    await mkdir(join(root, 'protocol/references/rsnano-node'), {recursive:true});
    await writeFile(join(root, 'protocol/references/rsnano-node/Cargo.toml'), 'reference-only');
    const result = spawnSync('bash', ['-euo', 'pipefail', '-c', command[0]], {
      cwd:root, encoding:'utf8', env:{...process.env, output:root},
    });
    assert.equal(result.status, 0, result.stderr);
    const actual = (await readFile(join(root, 'SOURCES.sha256'), 'utf8')).trim().split('\n').map(line => line.slice(66));
    assert.deepEqual(actual.sort(), kept.sort());
  } finally {
    await rm(root, {recursive:true, force:true});
  }
});

test('whole artifact preflight accepts matching packages and rejects an invalid second package before installation',
  {skip:process.getuid?.() === 0 ? 'requires a non-root validation user' : !hasDebianTools && 'requires Debian packaging tools'}, async () => {
    const root = await mkdtemp(join(tmpdir(), 'burst-candidate-preflight-'));
    try {
      const candidate = join(root, 'candidate');
      const source = join(root, 'source');
      const bin = join(root, 'bin');
      await mkdir(candidate);
      await mkdir(bin);
      await writeFile(join(bin, 'sudo'), '#!/bin/sh\necho UNEXPECTED_INSTALL >&2\nexit 99\n', {mode:0o755});
      await writeFile(join(bin, 'dpkg-query'), '#!/bin/sh\nexit 1\n', {mode:0o755});
      const sourceFiles = [];
      const packageFiles = [];
      const arch = spawnSync('dpkg', ['--print-architecture'], {encoding:'utf8'}).stdout.trim();
      async function packageApp(app, name, version = '0.1.0', depends = 'libc6 (>= 2.39)') {
        const payload = join(root, `payload-${app}`);
        await mkdir(join(payload, 'DEBIAN'), {recursive:true});
        await mkdir(join(payload, 'usr/bin'), {recursive:true});
        await writeFile(join(payload, 'DEBIAN/control'), `Package: ${name}\nVersion: ${version}\nArchitecture: ${arch}\nDepends: ${depends}\nMaintainer: Test <test@example.invalid>\nDescription: Preflight fixture\n`);
        await writeFile(join(payload, `usr/bin/burst-${app === 'wallet' ? 'wallet' : 'node'}-gui`), '#!/bin/sh\nexit 99\n', {mode:0o755});
        const file = `${app}/Burst ${app}.deb`;
        const built = spawnSync('dpkg-deb', ['--build', '--root-owner-group', payload, join(candidate, file)], {encoding:'utf8'});
        assert.equal(built.status, 0, built.stderr);
        return file;
      }
      for (const app of ['wallet', 'node-gui']) {
        await mkdir(join(candidate, app));
        await mkdir(join(source, app, 'tests'), {recursive:true});
        await mkdir(join(source, app, 'src-tauri'));
        for (const file of ['tests/native-session.mjs', ...(app === 'node-gui' ? ['tests/native-rpc.mjs', 'tests/native-dashboard.mjs'] : []), 'src-tauri/tauri.conf.json']) {
          const value = file.endsWith('.json') ? '{"version":"0.1.0","bundle":{"linux":{"deb":{"depends":["libc6 (>= 2.39)"]}}}}' : '';
          await writeFile(join(source, app, file), value);
          sourceFiles.push(`${createHash('sha256').update(value).digest('hex')}  ./${app}/${file}\n`);
        }
        packageFiles.push(await packageApp(app, app === 'wallet' ? 'burst-wallet' : 'burst-node'));
      }
      await writeFile(join(candidate, 'SOURCES.sha256'), sourceFiles.join(''));
      await writeFile(join(candidate, 'network.txt'), `private-testnet\n${publicKey}\n`);
      await writeFile(join(candidate, 'toolchain.txt'), 'test fixture');
      await writeFile(join(candidate, 'burst-daemon'), '#!/bin/sh\nexit 99\n', {mode:0o755});
      async function manifest(omit) {
        const lines = [];
        for (const file of ['SOURCES.sha256', 'network.txt', 'toolchain.txt', 'burst-daemon', ...packageFiles]) {
          if (file !== omit) lines.push(`${createHash('sha256').update(await readFile(join(candidate, file))).digest('hex')}  ./${file}\n`);
        }
        await writeFile(join(candidate, 'SHA256SUMS'), lines.join(''));
      }
      const validator = fileURLToPath(new URL('./validation/validate-linux-candidate.sh', import.meta.url));
      const validate = (phase = 'preflight') => spawnSync('bash', [validator, candidate, source], {encoding:'utf8', env:{
        PATH:`${bin}:${process.env.PATH}`, BURST_RELEASE_VALIDATION_HOST:'1', BURST_CANDIDATE_PHASE:phase,
      }});
      await manifest();
      const valid = validate();
      assert.equal(valid.status, 0, valid.stdout + valid.stderr);
      assert.match(valid.stdout, /PASS artifact preflight only/);
      await manifest(packageFiles[1]);
      const missing = validate('all');
      assert.equal(missing.status, 2, missing.stdout + missing.stderr);
      assert.match(missing.stdout + missing.stderr, /checksums must cover node-gui/);
      assert.doesNotMatch(missing.stdout + missing.stderr, /UNEXPECTED_INSTALL/);
      for (const [name, version, expected] of [['wrong-node', '0.1.0', /Unexpected node-gui package/], ['burst-node', '0.2.0', /version differs/]]) {
        await packageApp('node-gui', name, version);
        await manifest();
        const invalid = validate('all');
        assert.equal(invalid.status, 2, invalid.stdout + invalid.stderr);
        assert.match(invalid.stdout + invalid.stderr, expected);
        assert.doesNotMatch(invalid.stdout + invalid.stderr, /UNEXPECTED_INSTALL/);
      }
      for (const depends of ['libgtk-3-0', 'libc6 (>= 2.35)']) {
        await packageApp('node-gui', 'burst-node', '0.1.0', depends);
        await manifest();
        const invalid = validate('all');
        assert.equal(invalid.status, 2, invalid.stdout + invalid.stderr);
        assert.match(invalid.stdout + invalid.stderr, /runtime dependencies differ/);
        assert.doesNotMatch(invalid.stdout + invalid.stderr, /UNEXPECTED_INSTALL/);
      }
    } finally {
      await rm(root, {recursive:true, force:true});
    }
  });

test('creator candidate and disposable fixture have distinct labels', () => {
  const creator = run('--private-testnet', publicKey);
  assert.equal(creator.status, 0, creator.stderr);
  assert.equal(creator.stdout.trim(), 'private-testnet');
  const disposable = run('--private-testnet-fixture', fixture.toUpperCase());
  assert.equal(disposable.status, 0, disposable.stderr);
  assert.equal(disposable.stdout.trim(), 'private-testnet-fixture');
});

test('published fixture cannot be labelled as a creator candidate', () => {
  for (const key of [fixture, fixture.toUpperCase()]) {
    const result = run('--private-testnet', key);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /not distributable/);
    assert.equal(result.stdout, '');
  }
  assert.equal(run('--private-testnet-fixture', publicKey).status, 2);
});

test('unknown modes, extra arguments and malformed identities fail', () => {
  for (const args of [[], ['--live', publicKey], ['--private-testnet', publicKey, 'extra'],
    ['--private-testnet', ''], ['--private-testnet', 'g'.repeat(64)], ['--private-testnet', publicKey.slice(2)]]) {
    assert.equal(run(...args).status, 2);
  }
});

test('build rejects fixture identity before invoking build tools', () => {
  const build = fileURLToPath(new URL('./validation/build-linux-candidate.sh', import.meta.url));
  const result = spawnSync('bash', [build, '--private-testnet'], {encoding:'utf8', env:{
    PATH:process.env.PATH, BURST_RELEASE_VALIDATION_HOST:'1', BURST_TEST_GENESIS_PUBLIC_KEY:fixture,
  }});
  assert.equal(result.status, 2);
  assert.match(result.stderr, /not distributable/);
});

test('installer requires checksummed identity and explicit fixture opt-in before installation',
  {skip:process.getuid?.() === 0}, async () => {
    const root = await mkdtemp(join(tmpdir(), 'burst-candidate-identity-'));
    try {
      const candidate = join(root, 'candidate');
      const source = join(root, 'source');
      await mkdir(candidate);
      for (const app of ['wallet', 'node-gui']) {
        await mkdir(join(source, app, 'tests'), {recursive:true});
        await writeFile(join(source, app, 'tests/native-session.mjs'), '');
      }
      const validator = fileURLToPath(new URL('./validation/validate-linux-candidate.sh', import.meta.url));
      const validate = () => spawnSync('bash', [validator, candidate, source], {encoding:'utf8', env:{
        PATH:process.env.PATH, BURST_RELEASE_VALIDATION_HOST:'1',
      }});
      const files = {'network.txt':`private-testnet-fixture\n${fixture}\n`, 'SOURCES.sha256':'', 'toolchain.txt':'test'};
      const checksums = [];
      for (const [name, value] of Object.entries(files)) {
        await writeFile(join(candidate, name), value);
        checksums.push(`${createHash('sha256').update(value).digest('hex')}  ./${name}\n`);
      }
      await writeFile(join(candidate, 'SHA256SUMS'), checksums.slice(1).join(''));
      const missing = validate();
      assert.equal(missing.status, 2);
      assert.match(missing.stderr, /checksums must cover network.txt/);
      await writeFile(join(candidate, 'SHA256SUMS'), checksums.join(''));
      const fixtureResult = validate();
      assert.equal(fixtureResult.status, 2);
      assert.match(fixtureResult.stderr, /explicit validation opt-in/);
      await writeFile(join(candidate, 'network.txt'), `private-testnet\n${fixture}\n`);
      const tampered = validate();
      assert.notEqual(tampered.status, 0);
      assert.match(tampered.stdout, /network.txt: FAILED/);
    } finally {
      await rm(root, {recursive:true, force:true});
    }
  });
