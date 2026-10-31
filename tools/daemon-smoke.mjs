import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, open, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { createReadStream } from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const executable = process.argv[2];
// Private-mode validation uses the public disposable [42; 32] fixture, never
// the eventual pilot's founding key. Build the test artifact with its public key.
const privateFixture = process.argv[3] === '--private-testnet-fixture';
const publicIdentityMode = process.argv[3] === '--public-testnet-identity';
const retainedMode = process.argv[3] === '--retained-baseline';
const observeRetainedFailure = process.env.BURST_RETAINED_OBSERVE_FAILURE === '1';
const resourceProbe = process.env.BURST_RETAINED_RESOURCE_PROBE === '1';
const memoryTimeline = process.env.BURST_RETAINED_MEMORY_TIMELINE === '1';
if (memoryTimeline) assert.ok(resourceProbe, 'Memory timeline requires resource observation');
if (resourceProbe) assert.ok(retainedMode && observeRetainedFailure && process.platform === 'linux',
  'Resource observation requires Linux retained mode with failure observation');
if (observeRetainedFailure) assert.ok(retainedMode, 'Failure observation requires retained mode');
const epochHelper = process.env.BURST_EPOCH_VALIDATION_HELPER;
if (epochHelper) assert.ok(publicIdentityMode, 'Epoch qualification requires the public identity check');
const experimental = privateFixture || publicIdentityMode || process.argv[3] === '--experimental-testnet';
if (
  !executable ||
  ((publicIdentityMode || retainedMode) ? process.argv.length !== 5 : process.argv.length > 4) ||
  (process.argv[3] && !experimental && !retainedMode)
) {
  throw new Error(
    'Usage: node tools/daemon-smoke.mjs <daemon> [--experimental-testnet|--private-testnet-fixture|--public-testnet-identity <public-observation.json>|--retained-baseline <prepared-baseline.json>]',
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
const retained = retainedMode ? JSON.parse(await readFile(process.argv[4], 'utf8')) : null;
function hashText(bytes) {
  assert.ok(Array.isArray(bytes) && bytes.length === 32 && bytes.every(byte => Number.isInteger(byte) && byte >= 0 && byte <= 255));
  return Buffer.from(bytes).toString('hex');
}
if (retained) {
  assert.ok(Number.isSafeInteger(retained.report.accounts) && retained.report.accounts > 0);
  hashText(retained.report.epoch);
  hashText(retained.report.params_hash);
  if (retained.report.database_map_size_bytes !== undefined) {
    const bytes = retained.report.database_map_size_bytes;
    assert.ok(Number.isSafeInteger(bytes) && bytes >= 1048576 && bytes % 1048576 === 0,
      'Retained database map must be positive whole MiB');
  }
}
async function fileHash(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}
let retainedPassed = false;
const adminToken = randomBytes(32).toString("hex");
const directory = await mkdtemp(join(tmpdir(), 'burst-daemon-smoke-'));
let child;
let completion;
let logs = '';
const resident = { samples: 0, peak_rss_bytes: 0, peak_hwm_bytes: 0, peak_anon_bytes: 0, peak_file_bytes: 0, initialized: false };
let lastMemorySample = -Infinity;
if (memoryTimeline) resident.timeline = [];
async function sampleResident() {
  if (!resourceProbe || !child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  const status = await readFile(`/proc/${child.pid}/status`, 'utf8');
  const bytes = (field) => {
    const match = status.match(new RegExp(String.raw`^${field}:\s+(\d+) kB$`, 'm'));
    assert.ok(match, `Missing ${field} for daemon process`);
    return Number(match[1]) * 1024;
  };
  resident.samples++;
  resident.peak_rss_bytes = Math.max(resident.peak_rss_bytes, bytes('VmRSS'));
  resident.peak_hwm_bytes = Math.max(resident.peak_hwm_bytes, bytes('VmHWM'));
  resident.peak_anon_bytes = Math.max(resident.peak_anon_bytes, bytes('RssAnon'));
  resident.peak_file_bytes = Math.max(resident.peak_file_bytes, bytes('RssFile'));
  if (memoryTimeline && performance.now() - lastMemorySample >= 500 && resident.timeline.length < 256) {
    resident.timeline.push({ unix_ms: Date.now(), rss: bytes('VmRSS'),
      anonymous: bytes('RssAnon'), file: bytes('RssFile') });
    lastMemorySample = performance.now();
  }
}

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
  const stopBudget = retained ? 15_000 : 30_000;
  const deadline = Date.now() + stopBudget;
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
    throw new Error(`Daemon did not drain within ${stopBudget}ms`);
  }
  const result = await completion;
  assert.deepEqual(
    result,
    { code: 0, signal: null },
    `Unclean shutdown: ${logs}`,
  );
}

try {
  if (retained) {
    const preparationStarted = performance.now();
    let syncMs = 0;
    const ledger = join(directory, 'ledger-Dev');
    await mkdir(ledger);
    for (const name of ['data.mdb', 'resource-probe.bin']) {
      const input = join(retained.report.path, name);
      const expected = retained.files[input];
      assert.ok(expected && /^[a-f0-9]{64}$/.test(expected.sha256));
      assert.equal(await fileHash(input), expected.sha256, `Baseline changed: ${name}`);
      const copied = join(ledger, name);
      await copyFile(input, copied);
      // Retained startup begins from durable file contents, not pending fixture I/O.
      const handle = await open(copied, 'r+');
      try {
        const syncStarted = performance.now();
        await handle.sync();
        syncMs += performance.now() - syncStarted;
      } finally {
        await handle.close();
      }
      assert.equal(await fileHash(copied), expected.sha256, `Copy changed: ${name}`);
    }
    console.log('RETAINED_DAEMON_FIXTURE_PREPARED', JSON.stringify({
      elapsed_ms: performance.now() - preparationStarted, sync_ms: syncMs,
      scope: 'fixture copy and flush before process launch; excluded from launch-to-RPC timing',
    }));
    console.log('RETAINED_DAEMON_WORKING', directory);
  }
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
  async function finishRetained(elapsed) {
    const signing = await rpc('params_hash');
    assert.equal(signing.payment_epoch, hashText(retained.report.epoch));
    assert.equal(signing.params_hash, hashText(retained.report.params_hash));
    await sampleResident();
    await stop();
    for (const [path, expected] of Object.entries(retained.files)) {
      assert.equal(await fileHash(path), expected.sha256, 'Prepared baseline changed');
    }
    resident.initialized = true;
    resident.initialization_ms = elapsed;
  }
  // All runs disable bootstrap peers, representative voting, faucet and NTS.
  // Private Test identity checks below explicitly supply disposable genesis seeds.
  // Live here is an isolated rejection check, never a connection to mainnet.
  for (const network of experimental ? ['Dev', 'Test', 'Live'] : ['Dev']) {
    const epochReports = [];
    const epochDev = Boolean(epochHelper) && network === 'Dev';
    await writeFile(
      config,
      [
        `network = ${JSON.stringify(network)}`,
        `data_dir = ${JSON.stringify(join(directory, `ledger-${network}`))}`,
        ...(retained?.report.database_map_size_bytes === undefined ? []
          : [`database_map_size_bytes = ${retained.report.database_map_size_bytes}`]),
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
        ...(retained ? ['recovery_scratch_bytes = 67108864', 'drand_relays = ["http://127.0.0.1:1"]'] : []),
      ].join('\n'),
    );
    const privateTest = privateFixture && network === 'Test';
    const publicTest = publicIdentityMode && network === 'Test';
    for (let attempt = 0; attempt < (retained ? 1 : privateTest || epochDev ? 3 : 2); attempt++) {
      const env = { ...observerEnv, BURST_RPC_ADMIN_TOKEN: adminToken };
      if ((privateTest || publicTest) && attempt === 1) env.BURST_GENESIS_SEED = '07'.repeat(32);
      if (privateTest && attempt === 2) env.BURST_GENESIS_SEED_FILE = creatorSeedFile;
      const started = performance.now();
      child = spawn(resolve(executable), ['--config', config, 'node', 'run'], {
        stdio: ['ignore', 'pipe', 'pipe'],
        env,
      });
      logs = '';
      const collect = (chunk) => {
        logs = (logs + chunk).slice(-32_768);
        if (retained) process.stdout.write(chunk);
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
      const readyBudget = retained ? 15_000 : 30_000;
      const deadline = started + readyBudget;
      let ready = false;
      while (performance.now() < deadline) {
        await sampleResident();
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
            result.faucet_enabled === false &&
            (!retained || Number(result.account_count) >= retained.report.accounts);
        } catch {
          /* The listener may not be bound yet. */
        }
        if (ready) {
          if (retained && performance.now() > deadline) ready = false;
          break;
        }
        await delay(100);
      }
      if (retained) console.log('RETAINED_DAEMON_READINESS', JSON.stringify({
        ready, elapsed_ms: performance.now() - started, budget_ms: readyBudget,
        clock: 'ordinary daemon clock; no fixture override',
        scope: 'process launch to RPC, includes construction; not the old after-construction timer',
      }));
      if (retained && !ready && observeRetainedFailure) {
        // Record the failed deadline above, then observe without changing `ready`.
        const observationDeadline = performance.now() + 5000;
        let observedReady = false;
        let observedAt;
        while (performance.now() < observationDeadline && child.exitCode === null && child.signalCode === null) {
          await sampleResident();
          try {
            const telemetry = await rpc('telemetry');
            observedReady = telemetry.network === network.toLowerCase()
              && Number(telemetry.account_count) >= retained.report.accounts
              && Number(telemetry.block_count) >= 1 && telemetry.faucet_enabled === false;
          } catch { /* Diagnostic observation of a listener that may still be closed. */ }
          if (observedReady) { observedAt = performance.now() - started; break; }
          await delay(100);
        }
        console.log('RETAINED_DAEMON_AFTER_FAILURE', JSON.stringify({
          observed_ready: observedReady, observed_elapsed_ms: observedAt,
          observation_budget_ms: 5000, acceptance_ready: ready,
          scope: 'diagnostic only; the original readiness failure remains a failure',
        }));
        if (observedReady) {
          await finishRetained(observedAt);
          console.log('RETAINED_DAEMON_DIAGNOSTIC_STOP: expected identity, clean stop, baseline unchanged; readiness still failed.');
        }
      }
      if (resourceProbe && !ready && !resident.initialized) {
        // Separate resource observation; neither deadline result above is changed.
        const resourceDeadline = started + 120_000;
        while (performance.now() < resourceDeadline && child.exitCode === null && child.signalCode === null) {
          await sampleResident();
          let observed = false;
          try {
            const telemetry = await rpc('telemetry');
            observed = telemetry.network === network.toLowerCase()
              && Number(telemetry.account_count) >= retained.report.accounts
              && Number(telemetry.block_count) >= 1 && telemetry.faucet_enabled === false;
          } catch { /* Record memory while initialization has not opened RPC. */ }
          if (observed) { await finishRetained(performance.now() - started); break; }
          await delay(100);
        }
      }
      assert.ok(ready, `Local RPC did not become ready: ${logs}`);
      if (retained) {
        await finishRetained(performance.now() - started);
        retainedPassed = true;
        console.log('PASS retained daemon: 15s launch-to-RPC, expected epoch/params/accounts, 15s clean stop; baseline unchanged.');
        continue;
      }
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
          headers: { 'content-type': 'application/json', authorization: `Bearer ${adminToken}` },
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
    if (!retained) console.log(
      `PASS ${network}: selection ${experimental && network !== 'Live' ? 'enabled' : 'disabled'}, RPC and same-ledger restart.`,
    );
  }
  if (!retained) console.log(
    'PASS: local RPC, disabled test funding, graceful stop and same-ledger restart.',
  );
  if (epochHelper) console.log('PASS: exact daemon Dev epoch marker, owner successor and two byte-exact history restarts; no public creator seed used.');
} finally {
  if (resourceProbe) console.log('RETAINED_DAEMON_RESOURCES', JSON.stringify({
    ...resident, accounts: retained.report.accounts, observation_budget_ms: 120_000,
    memory_target_bytes: 1_000_000_000,
    below_memory_target: resident.samples > 0 && resident.peak_hwm_bytes < 1_000_000_000,
    scope: 'Daemon-process RSS/HWM during retained initialization only; no timing acceptance, steady workload or supported-platform qualification',
  }));
  if (child?.pid && child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM');
    await Promise.race([completion, delay(5000)]);
    if (child.exitCode === null && child.signalCode === null)
      child.kill('SIGKILL');
    await completion;
  }
  if (retained && !retainedPassed) console.log('RETAINED_DAEMON_FAILURE_FIXTURE', directory);
  else await rm(directory, { recursive: true, force: true });
}
