// Normal-daemon snapshot/reconnect acceptance on the disposable private Test ledger.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { createHash } from 'node:crypto';
import { mkdtemp, open, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

assert([7, 8, 9].includes(process.argv.length), 'Provide artifacts, source ledger, representative key, lifecycle report, evidence root, optional governance report and optional consumer report');
const [artifacts, ledger, key, report, root] = process.argv.slice(2, 7).map(path => resolve(path));
const governanceReport = process.argv[7] && resolve(process.argv[7]);
const consumerReport = process.argv[8] && resolve(process.argv[8]);
const recoveryReport = process.env.NATIVE_RECOVERY_REPORT && resolve(process.env.NATIVE_RECOVERY_REPORT);
const evidence = await mkdtemp(join(root, 'snapshot-runtime-'));
console.log(`Snapshot runtime evidence: ${evidence}`);
const daemon = join(artifacts, 'burst-daemon');
const verifier = join(artifacts, 'examples/validate_testnet_onboarding');
const paymentProbe = join(artifacts, 'examples/validate_restart_payment');
const hashes = {};
const governanceVerifier = join(artifacts, 'examples/validate_testnet_governance');
const consumerVerifier = join(artifacts, 'examples/verify-consumer.mjs');
const recoveryVerifier = join(artifacts, 'examples/verify-recovery-followthrough.mjs');
for (const path of [daemon, verifier, paymentProbe, report,
  ...(governanceReport ? [governanceReport, governanceVerifier] : []),
  ...(consumerReport ? [consumerReport, consumerVerifier] : []),
  ...(recoveryReport ? [recoveryReport, recoveryVerifier] : [])]) {
  hashes[path] = createHash('sha256').update(await readFile(path)).digest('hex');
}
await writeFile(join(evidence, 'inputs.json'), JSON.stringify(hashes, null, 2), {flag:'wx'});
const children = [];
const cancellation = new AbortController();
const deadline = setTimeout(() => cancellation.abort(new Error('Snapshot journey exceeded 15 minutes')), 900000);
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => cancellation.abort(new Error(signal)));
const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('BURST_')));
async function freePort() {
  const server = createServer();
  await new Promise((done, fail) => { server.once('error', fail); server.listen(0, '127.0.0.1', done); });
  const port = server.address().port;
  await new Promise(done => server.close(done));
  return port;
}
async function launch(binary, args, name, overrides = {}) {
  cancellation.signal.throwIfAborted();
  const log = await open(join(evidence, name), 'a', 0o600);
  const child = spawn(binary, args, {
    cwd:evidence, env:{...env, ...overrides}, stdio:['ignore', log.fd, log.fd],
  });
  const task = {child, done:false, log:join(evidence, name)};
  task.exited = new Promise(done => {
    child.once('error', error => { task.done = true; done({error}); });
    child.once('exit', (code, signal) => { task.done = true; done({code, signal}); });
  });
  children.push(task);
  await log.close();
  return task;
}
async function stop(task, signal = 'SIGTERM') {
  if (!task || task.done) return;
  task.child.kill(signal);
  const force = setTimeout(() => { if (!task.done) task.child.kill('SIGKILL'); }, 5000);
  await task.exited;
  clearTimeout(force);
}
cancellation.signal.addEventListener('abort', () => {
  for (const task of children) if (!task.done) task.child.kill('SIGTERM');
});
async function run(binary, args, log, fails = false) {
  const task = await launch(binary, args, log);
  const result = await task.exited;
  assert(!result.error && !result.signal, `Command did not finish: ${task.log}`);
  if (fails) assert.notEqual(result.code, 0, `Tampered snapshot accepted: ${task.log}`);
  else assert.equal(result.code, 0, `Command failed: ${task.log}`);
}
async function rpc(port, action) {
  const reply = await fetch(`http://127.0.0.1:${port}`, {
    method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({action}),
    signal:AbortSignal.any([cancellation.signal, AbortSignal.timeout(3000)]),
  });
  assert(reply.ok);
  const value = await reply.json();
  assert(!value.error, JSON.stringify(value.error));
  return value.result;
}
async function waitFor(task, port, predicate) {
  const until = Date.now() + 180000;
  while (Date.now() < until) {
    cancellation.signal.throwIfAborted();
    assert(!task.done, `Node exited: ${task.log}`);
    let info;
    try { info = await rpc(port, 'node_info'); } catch (error) {
      cancellation.signal.throwIfAborted();
      if (Date.now() >= until) throw error;
    }
    if (info && predicate(info)) return info;
    await new Promise(done => setTimeout(done, 300));
  }
  assert.fail(`Node did not reach required state: ${task.log}`);
}
const sourcePort = await freePort(), sourceRpc = await freePort();
const targetPort = await freePort(), targetRpc = await freePort();
assert.equal(new Set([sourcePort, sourceRpc, targetPort, targetRpc]).size, 4);
const sourceArgs = ['--network','test','--data-dir',ledger,'--representative-key-file',key,
  '--port',String(sourcePort),'--rpc-port',String(sourceRpc),'--disable-upnp','node','run'];
const snapshot = join(evidence, 'state.snapshot');
const attestation = join(evidence, 'attestation.json');
let source, target;
const sourceEnv = {BURST_GENESIS_SEED_FILE:key};
try {
  source = await launch(daemon, sourceArgs, 'source.log', sourceEnv);
  await waitFor(source, sourceRpc, info => info.network === 'test' && info.verified_count >= 50);
  assert.equal((await rpc(sourceRpc, 'protocol_params')).brn_rate, '100000000000000000000');
  const epoch = (await rpc(sourceRpc, 'params_hash')).payment_epoch;
  assert.match(epoch, /^[a-f0-9]{64}$/);
  const baselineUrl = `http://127.0.0.1:${sourceRpc}`;
  await run(verifier, ['verify-joining',report,baselineUrl], 'source-state.log');
  if (governanceReport) await run(governanceVerifier, ['verify-snapshot',governanceReport,baselineUrl], 'source-governance-state.log');
  if (consumerReport) await run(process.execPath, [consumerVerifier,consumerReport,baselineUrl], 'source-consumer-state.log');
  if (recoveryReport) await run(process.execPath, [recoveryVerifier,recoveryReport,baselineUrl], 'source-recovery-state.log');
  await stop(source);
  await run(daemon, ['--network','test','--data-dir',ledger,'--representative-key-file',key,
    'snapshot','attest','--epoch',epoch,'--output',attestation], 'attest.log');
  await run(daemon, ['--network','test','--data-dir',ledger,'snapshot','export','--epoch',epoch,
    '--attestation',attestation,'--output',snapshot], 'export.log');
  await run(daemon, ['--network','test','snapshot','verify','--input',snapshot,'--expected-epoch',epoch], 'verify.log');
  const bytes = await readFile(snapshot);
  const digest = createHash('sha256').update(bytes).digest('hex');
  const altered = Buffer.from(bytes);
  altered[altered.length - 1] ^= 1;
  const tampered = join(evidence, 'tampered.snapshot');
  await writeFile(tampered, altered, {flag:'wx', mode:0o600});
  await run(daemon, ['--network','test','snapshot','verify','--input',tampered,'--expected-epoch',epoch], 'tampered.log', true);
  assert.match(await readFile(join(evidence, 'tampered.log'), 'utf8'), /snapshot authentication failed/);
  console.log('PASS operator attestation/export, exact-epoch verification and altered-archive rejection');
  const config = join(evidence, 'snapshot.toml');
  await writeFile(config, `network = "Test"\nsnapshot_bootstrap = ${JSON.stringify(snapshot)}\n`, {flag:'wx', mode:0o600});
  const targetArgs = ['--config',config,'--data-dir',join(evidence, 'receiver'),'--no-representative',
    '--port',String(targetPort),'--rpc-port',String(targetRpc),'--disable-upnp',
    '--bootstrap-peers',`127.0.0.1:${sourcePort}`,'node','run'];
  source = await launch(daemon, sourceArgs, 'source.log', sourceEnv);
  await waitFor(source, sourceRpc, info => info.network === 'test');
  target = await launch(daemon, targetArgs, 'receiver.log');
  await waitFor(target, targetRpc, info => info.network === 'test' && info.verified_count >= 50);
  const targetUrl = `http://127.0.0.1:${targetRpc}`, sourceUrl = `http://127.0.0.1:${sourceRpc}`;
  await run(verifier, ['verify-joining',report,targetUrl,sourceUrl], 'state.log');
  // The installed archive carries certified governance state, not every old
  // proposal block. Check that state directly; full-history joining separately
  // checks proposal block confirmation.
  if (governanceReport) await run(governanceVerifier, ['verify-snapshot',governanceReport,targetUrl,sourceUrl], 'governance-state.log');
  if (consumerReport) await run(process.execPath, [consumerVerifier,consumerReport,targetUrl,sourceUrl], 'consumer-state.log');
  if (recoveryReport) await run(process.execPath, [recoveryVerifier,recoveryReport,targetUrl,sourceUrl], 'recovery-state.log');
  await stop(target, 'SIGKILL');
  target = await launch(daemon, targetArgs, 'receiver.log');
  await waitFor(target, targetRpc, info => info.network === 'test');
  await run(verifier, ['verify',report,targetUrl,sourceUrl], 'restart-state.log');
  if (governanceReport) await run(governanceVerifier, ['verify',governanceReport,targetUrl,sourceUrl], 'governance-restart.log');
  if (consumerReport) await run(process.execPath, [consumerVerifier,consumerReport,targetUrl,sourceUrl], 'consumer-restart.log');
  if (recoveryReport) await run(process.execPath, [recoveryVerifier,recoveryReport,targetUrl,sourceUrl], 'recovery-restart.log');
  await stop(source);
  await waitFor(target, targetRpc, info => info.peer_count === 0);
  source = await launch(daemon, sourceArgs, 'source.log', sourceEnv);
  await waitFor(source, sourceRpc, info => info.network === 'test');
  await waitFor(target, targetRpc, info => info.peer_count > 0);
  await run(verifier, ['verify',report,targetUrl,sourceUrl], 'reconnected-state.log');
  if (governanceReport) await run(governanceVerifier, ['verify',governanceReport,targetUrl,sourceUrl], 'governance-reconnected.log');
  if (consumerReport) await run(process.execPath, [consumerVerifier,consumerReport,targetUrl,sourceUrl], 'consumer-reconnected.log');
  if (recoveryReport) await run(process.execPath, [recoveryVerifier,recoveryReport,targetUrl,sourceUrl], 'recovery-reconnected.log');
  await run(paymentProbe, ['--private-testnet',targetUrl,sourceUrl], 'payment.log');
  if (consumerReport) await run(process.execPath, [consumerVerifier,consumerReport,targetUrl,sourceUrl], 'consumer-after-payment.log');
  await writeFile(join(evidence, 'result.json'), JSON.stringify({epoch, snapshot_sha256:digest,
    tamper_rejected:true, installed:true, crash_recovered:true, reconnected:true, new_payment:true,
    governance_recovered:!!governanceReport, consumer_recovered:!!consumerReport,
    recovered_actions_preserved:!!recoveryReport}, null, 2), {flag:'wx'});
  console.log('PASS authenticated snapshot startup, hard restart, disconnect/reconnect, exact lifecycle balances and a new confirmed payment');
} finally {
  clearTimeout(deadline);
  for (const task of children.reverse()) await stop(task);
}
