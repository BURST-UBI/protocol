import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const executable = process.argv[2];
// Private-mode validation uses the public disposable [42; 32] fixture, never
// the eventual pilot's founding key. Build the test artifact with its public key.
const privateFixture = process.argv[3] === '--private-testnet-fixture';
const publicIdentityMode = process.argv[3] === '--public-testnet-identity';
const epochHelper = process.env.BURST_EPOCH_VALIDATION_HELPER;
if (epochHelper) assert.ok(publicIdentityMode, 'Epoch qualification requires the public identity check');
const experimental = privateFixture || publicIdentityMode || process.argv[3] === '--experimental-testnet';
if (
  !executable ||
  (publicIdentityMode ? process.argv.length !== 5 : process.argv.length > 4) ||
  (process.argv[3] && !experimental)
) {
  throw new Error(
    'Usage: node tools/daemon-smoke.mjs <daemon> [--experimental-testnet|--private-testnet-fixture|--public-testnet-identity <public-observation.json>]',
  );
}
if (process.platform === 'win32')
  throw new Error('Use windows-service-smoke.ps1 on Windows');
const publicIdentity = publicIdentityMode
  ? JSON.parse(await readFile(process.argv[4], 'utf8'))
  : null;
if (publicIdentity) {
  assert.equal(publicIdentity.network, 'Test');
  assert.match(publicIdentity.reportedAddress, /^brst_[a-z0-9]+$/);
  assert.match(publicIdentity.reportedGenesisHash, /^[a-f0-9]{64}$/);
}
const directory = await mkdtemp(join(tmpdir(), 'burst-daemon-smoke-'));
let child;
let completion;
let logs = '';

async function unusedPort() {
  const listener = createServer();
  await new Promise((ok, fail) => {
    listener.once('error', fail);
    listener.listen(0, '127.0.0.1', ok);
  });
  const { port } = listener.address();
  await new Promise((ok, fail) =>
    listener.close((error) => (error ? fail(error) : ok())),
  );
  return port;
}

async function stop() {
  if (child.exitCode === null && child.signalCode === null)
    child.kill('SIGTERM');
  const deadline = Date.now() + 30_000;
  while (
    child.exitCode === null &&
    child.signalCode === null &&
    Date.now() < deadline
  ) {
    await delay(50);
  }
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await completion;
    throw new Error('Daemon did not drain within 30 seconds');
  }
  const result = await completion;
  assert.deepEqual(
    result,
    { code: 0, signal: null },
    `Unclean shutdown: ${logs}`,
  );
}

try {
  const creatorSeedFile = join(directory, 'disposable-creator.seed');
  if (privateFixture)
    await writeFile(creatorSeedFile, '2a'.repeat(32), { mode: 0o600 });
  const observerEnv = { ...process.env };
  delete observerEnv.BURST_GENESIS_SEED;
  delete observerEnv.BURST_GENESIS_SEED_FILE;
  const rpcPort = await unusedPort();
  const config = join(directory, 'node.toml');
  async function rpc(action, params = {}) {
    const response = await fetch(`http://127.0.0.1:${rpcPort}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ action, ...params }),
      signal: AbortSignal.timeout(1000),
    });
    assert.ok(response.ok, `RPC ${action}: HTTP ${response.status}`);
    const body = await response.json();
    assert.equal(
      body.error,
      undefined,
      `RPC ${action}: ${JSON.stringify(body)}`,
    );
    return body.result;
  }
  // All runs have no bootstrap peers, signer, faucet or authenticated clock.
  // Live here is an isolated rejection check, never a connection to mainnet.
  for (const network of experimental ? ['Dev', 'Test', 'Live'] : ['Dev']) {
    const epochReports = [];
    const epochDev = Boolean(epochHelper) && network === 'Dev';
    await writeFile(
      config,
      [
        `network = ${JSON.stringify(network)}`,
        `data_dir = ${JSON.stringify(join(directory, `ledger-${network}`))}`,
        'port = 0',
        'enable_rpc = true',
        'rpc_bind = "127.0.0.1"',
        `rpc_port = ${rpcPort}`,
        'enable_websocket = false',
        'enable_faucet = false',
        'enable_representative = false',
        'enable_upnp = false',
        'enable_peer_reachout = false',
        'bootstrap_peers = []',
        'nts_sources = []',
      ].join('\n'),
    );
    const privateTest = privateFixture && network === 'Test';
    const publicTest = publicIdentityMode && network === 'Test';
    for (let attempt = 0; attempt < (privateTest || epochDev ? 3 : 2); attempt++) {
      const env = { ...observerEnv };
      if ((privateTest || publicTest) && attempt === 1) env.BURST_GENESIS_SEED = '07'.repeat(32);
      if (privateTest && attempt === 2) env.BURST_GENESIS_SEED_FILE = creatorSeedFile;
      child = spawn(resolve(executable), ['--config', config, 'node', 'run'], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env,
      });
      logs = '';
      const collect = (chunk) => {
        logs = (logs + chunk).slice(-32_768);
      };
      child.stdout.on('data', collect);
      child.stderr.on('data', collect);
      let spawnError;
      child.once('error', (error) => {
        spawnError = error;
      });
      completion = new Promise((ok) =>
        child.once('close', (code, signal) => ok({ code, signal })),
      );
      const deadline = Date.now() + 30_000;
      let ready = false;
      while (Date.now() < deadline) {
        if (spawnError) throw spawnError;
        if (child.exitCode !== null || child.signalCode !== null) {
          throw new Error(`Daemon exited before readiness: ${logs}`);
        }
        try {
          const response = await fetch(`http://127.0.0.1:${rpcPort}`, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ action: 'telemetry' }),
            signal: AbortSignal.timeout(1000),
          });
          const { result } = await response.json();
          ready =
            response.ok &&
            result?.network === network.toLowerCase() &&
            Number(result.block_count) >= 1 &&
            result.faucet_enabled === false;
        } catch {
          /* The listener may not be bound yet. */
        }
        if (ready) break;
        await delay(100);
      }
      assert.ok(ready, `Local RPC did not become ready: ${logs}`);
      const representatives = await rpc('representatives');
      const account = representatives.representatives[0]?.account;
      assert.ok(account, 'Genesis representative missing from RPC');
      if (publicTest) {
        assert.equal(account, publicIdentity.reportedAddress, 'Test creator identity mismatch');
        const info = await rpc('account_info', { account });
        assert.equal(info.head, publicIdentity.reportedGenesisHash, 'Test genesis head mismatch');
        assert.equal(info.confirmed_open, publicIdentity.reportedGenesisHash, 'Test genesis confirmation mismatch');
        assert.equal(info.payment_epoch, publicIdentity.reportedGenesisHash, 'Test genesis payment epoch mismatch');
        assert.equal(info.verification_state, 'verified');
        assert.equal(info.trst_balance, '0');
      }
      if (privateTest || publicTest) {
        const response = await fetch(`http://127.0.0.1:${rpcPort}`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ action: 'genesis_endorse', account }),
          signal: AbortSignal.timeout(1000),
        });
        const result = await response.json();
        assert.match(
          JSON.stringify(result.error),
          attempt === 2 ? /cannot endorse the genesis account itself/ : /not the genesis authority/,
          `Unexpected creator authority on attempt ${attempt}`,
        );
      }
      const status = await rpc('verification_status', { account });
      const selectionExpected = experimental && network !== 'Live';
      const appeal = await rpc('appeal_review', { account });
      assert.equal(
        appeal.enabled,
        selectionExpected,
        `${network}: unexpected appeal availability`,
      );
      const capabilities = await rpc('params_hash');
      if (publicTest) {
        // Protocol BRN has 10^18 raw units; the per-second rate rounds up.
        const params = await rpc('protocol_params');
        assert.equal(params.brn_rate, '277777777777778', 'Initial rate must be 1 BRN/hour');
      }
      assert.equal(
        capabilities.full_governance_content,
        selectionExpected,
        `${network}: unexpected full governance content availability`,
      );
      assert.equal(
        capabilities.delegation_key_journal,
        selectionExpected,
        `${network}: unexpected scoped delegation availability`,
      );
      assert.equal(
        status.selection_available,
        selectionExpected,
        `${network}: unexpected certified selection availability`,
      );
      assert.equal(
        status.selection_unavailable_reason,
        selectionExpected
          ? null
          : 'certified_checkpoint_selection_release_gated',
      );
      if (epochDev) {
        for (const report of epochReports) {
          const stored = await rpc('block_info', { hash: report.hash, include_signed_block: true });
          assert.equal(stored.signed_block, report.signedBlock, 'Signed epoch history changed after restart');
        }
        if (epochReports.length) {
          const info = await rpc('account_info', { account });
          assert.equal(info.head, epochReports.at(-1).hash);
          assert.equal(info.trst_balance, '0');
          assert.equal(info.verification_state, 'verified');
        }
        if (attempt < 2) {
          const helper = spawn(resolve(epochHelper), ['--protocol-epoch', attempt === 0 ? 'marker' : 'successor', `http://127.0.0.1:${rpcPort}`], {
            stdio: ['ignore', 'pipe', 'pipe'], env: observerEnv,
          });
          let output = '', errors = '';
          helper.stdout.on('data', chunk => { output += chunk; });
          helper.stderr.on('data', chunk => { errors += chunk; });
          const code = await new Promise((ok, fail) => {
            helper.once('error', fail);
            helper.once('close', ok);
          });
          assert.equal(code, 0, `Epoch helper failed: ${errors}`);
          const report = JSON.parse(output.trim());
          assert.equal(report.account, account);
          epochReports.push(report);
        }
      }
      await stop();
    }
    console.log(
      `PASS ${network}: selection ${experimental && network !== 'Live' ? 'enabled' : 'disabled'}, RPC and same-ledger restart.`,
    );
  }
  console.log(
    'PASS: local RPC, disabled test funding, graceful stop and same-ledger restart.',
  );
  if (epochHelper) console.log('PASS: exact daemon Dev epoch marker, owner successor and two byte-exact history restarts; no public creator seed used.');
} finally {
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM');
    await Promise.race([completion, delay(5000)]);
    if (child.exitCode === null && child.signalCode === null)
      child.kill('SIGKILL');
    await completion;
  }
  await rm(directory, { recursive: true, force: true });
}
