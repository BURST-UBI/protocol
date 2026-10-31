// Run with BURST_DAEMON pointing to the selected validation artifact.
// Disposable local ledgers only; keep wallet keys out of diagnostics.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, open } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';

const binary = resolve(process.env.BURST_DAEMON || 'target/release/burst-daemon');
// Fixture nodes must never inherit a real signer, bootstrap peer, or RPC policy.
const daemonEnv = Object.fromEntries(Object.entries(process.env)
  .filter(([name]) => !name.startsWith('BURST_')));
const evidence = await mkdtemp(join(tmpdir(), 'burst-test-tools-'));
const children = [];
const actions = ['faucet', 'wallet_create_full', 'burn_simple', 'send_simple',
  'receive_simple', 'change_rep_simple', 'governance_propose_simple',
  'governance_propose_eviction', 'governance_vote_simple'];
console.log(`Evidence: ${evidence}`);

async function port() {
  const server = createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const value = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return value;
}
async function rpc(node, action, args = {}) {
  let response;
  try {
    response = await fetch(node.url, { method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action, ...args }), signal: AbortSignal.timeout(45000) });
  } catch (cause) {
    // Identify the failed action, never include private-key-bearing arguments.
    throw new Error(`${node.name}: ${action} RPC failed (inspect node log before retrying)`, { cause });
  }
  assert.equal(response.status, 200);
  return response.json();
}
async function ok(node, action, args = {}) {
  const response = await rpc(node, action, args);
  assert.equal(response.error, undefined, `${action}: ${response.error}`);
  assert.ok(response.result);
  return response.result;
}
async function eventually(check, description) {
  let last;
  for (let i = 0; i < 120; i++) {
    try { await check(); return; } catch (error) { last = error; }
    await delay(250);
  }
  throw new Error(description, { cause: last });
}
async function start(name, network, faucet, bootstrap, previous) {
  const node = previous || { name, network, faucet, bootstrap, p2p: await port(), rpc: await port() };
  node.url = `http://127.0.0.1:${node.rpc}`;
  const log = await open(join(evidence, `${name}.log`), 'a');
  const args = ['--network', network, '--data-dir', join(evidence, name),
    '--port', String(node.p2p), '--rpc-port', String(node.rpc), '--disable-upnp',
    '--log-level', 'debug'];
  if (faucet) args.push('--faucet');
  if (bootstrap) args.push('--bootstrap-peers', `127.0.0.1:${bootstrap.p2p}`, '--no-representative');
  node.child = spawn(binary, [...args, 'node', 'run'], { env: daemonEnv, stdio: ['ignore', log.fd, log.fd] });
  children.push(node.child);
  node.child.on('error', error => { node.spawnError = error; });
  await log.close();
  await eventually(async () => {
    if (node.spawnError) throw node.spawnError;
    assert.equal((await ok(node, 'node_info')).network, network);
  }, `${name} failed to start`);
  return node;
}
async function disabled(node) {
  assert.equal((await ok(node, 'node_info')).faucet_enabled, false);
  const before = await ok(node, 'node_info');
  assert.match((await rpc(node, 'governance_vote', {
    voter: 'claimed-public-address', proposal_hash: '00'.repeat(32), vote: 'yea',
  })).error, /require a signed GovernanceVote/);
  for (const action of actions) {
    const response = await rpc(node, action);
    assert.match(response.error, /test tools require/, `${node.network}/${action} must fail before parsing`);
  }
  const after = await ok(node, 'node_info');
  for (const field of ['block_count', 'account_count', 'verified_count', 'trst_circulating']) {
    assert.equal(after[field], before[field], `disabled action changed ${field}`);
  }
  console.log(`PASS ${node.name}: all ${actions.length} test-only actions denied without state changes`);
}
async function confirmed(nodes, hash) {
  for (const node of nodes) await eventually(async () => {
    assert.equal((await ok(node, 'block_info', { hash })).confirmed, true);
  }, `block did not confirm on ${node.name}`);
}
async function state(node, account) {
  const info = await ok(node, 'account_info', { account });
  delete info.brn_balance;
  const portfolio = await ok(node, 'trst_portfolio', { account });
  portfolio.batches.sort((a, b) => a.id.localeCompare(b.id));
  const pending = await ok(node, 'account_pending', { account });
  pending.pending.sort((a, b) => a.hash.localeCompare(b.hash));
  return { info, portfolio, pending };
}
async function assertStates(nodes, accounts, expected) {
  for (const node of nodes) for (let i = 0; i < accounts.length; i++) {
    await eventually(async () => assert.deepEqual(await state(node, accounts[i]), expected[i]),
      `ledger/portfolio/pending differs on ${node.name}`);
  }
}
async function stop(child, signal = 'SIGTERM') {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise(resolve => child.once('exit', resolve));
  child.kill(signal);
  const timer = setTimeout(() => child.kill('SIGKILL'), 3000);
  await exited;
  clearTimeout(timer);
}

try {
  const live = await start('live-faucet-requested', 'live', true);
  await disabled(live);
  assert.match((await rpc(live, 'genesis_endorse')).error, /not the genesis authority/);
  console.log('PASS isolated live peer cannot manufacture genesis authority');
  await stop(live.child);
  const testOff = await start('test-disabled', 'test', false);
  await disabled(testOff);
  await stop(testOff.child);
  const test = await start('test-enabled', 'test', true);
  const testWallet = await ok(test, 'wallet_create_full');
  const testFunds = await ok(test, 'faucet', { account: testWallet.address });
  assert.equal(testFunds.status, 'pending_receive');
  await ok(test, 'receive_simple', { private_key: testWallet.private_key, send_block_hash: testFunds.send_block_hash });
  assert.equal((await ok(test, 'trst_portfolio', { account: testWallet.address })).total_active, testFunds.amount);
  assert.equal((await ok(test, 'account_info', { account: testWallet.address })).verification_state, 'unverified');
  const testGenesis = (await ok(test, 'block_info', { hash: testFunds.send_block_hash })).account;
  await ok(test, 'change_rep_simple', { private_key: testWallet.private_key, new_representative: testGenesis });
  await ok(test, 'genesis_endorse', { account: testWallet.address });
  assert.equal((await ok(test, 'account_info', { account: testWallet.address })).verification_state, 'verified');
  console.log('PASS Test network bootstrap, funding and provenance-backed receive');
  await stop(test.child);

  let seed = await start('dev-seed', 'dev', true);
  let peer = await start('dev-peer-no-faucet', 'dev', false, seed);
  await disabled(peer);
  let nodes = [seed, peer];
  const alice = await ok(seed, 'wallet_create_full');
  const funding = await ok(seed, 'faucet', { account: alice.address });
  assert.equal(funding.status, 'pending_receive');
  await confirmed(nodes, funding.send_block_hash);
  for (const node of nodes) {
    assert.match((await rpc(node, 'account_info', { account: alice.address })).error, /account not found/,
      'faucet must neither verify nor open the recipient account');
    assert.equal((await ok(node, 'node_info')).verified_count, 1);
    const pending = await ok(node, 'account_pending', { account: alice.address });
    assert.equal(pending.pending.find(p => p.hash === funding.send_block_hash).amount, funding.amount);
  }
  const received = await ok(seed, 'receive_simple', { private_key: alice.private_key, send_block_hash: funding.send_block_hash });
  await confirmed(nodes, received.block_hash);
  for (const node of nodes) assert.equal((await ok(node, 'account_info', { account: alice.address })).verification_state, 'unverified');
  const genesis = (await ok(seed, 'block_info', { hash: funding.send_block_hash })).account;
  const delegated = await ok(seed, 'change_rep_simple', { private_key: alice.private_key, new_representative: genesis });
  await confirmed(nodes, delegated.block_hash);
  const endorsement = await ok(seed, 'genesis_endorse', { account: alice.address });
  await confirmed(nodes, endorsement.block_hash);
  for (const node of nodes) assert.equal(
    (await ok(node, 'block_info', { hash: endorsement.block_hash })).target_head,
    delegated.block_hash, 'identity admission must commit the signed representative choice',
  );
  const verifiedAt = (await ok(seed, 'account_info', { account: alice.address })).verified_at;
  assert.ok(verifiedAt);
  for (const node of nodes) assert.equal((await ok(node, 'account_info', { account: alice.address })).verification_state, 'verified');
  // Voting weights are refreshed asynchronously by the normal node task.
  await eventually(async () => {
    const reps = (await ok(seed, 'representatives')).representatives;
    assert.equal(reps.find(r => r.account === genesis).weight, '20000');
  }, 'expected exactly two verified humans delegated to genesis');
  const duplicate = await rpc(seed, 'genesis_endorse', { account: alice.address });
  assert.match(duplicate.error, /already verified/);
  const moreFunds = await ok(seed, 'faucet', { account: alice.address });
  await confirmed(nodes, moreFunds.send_block_hash);
  assert.equal((await ok(seed, 'account_info', { account: alice.address })).verified_at, verifiedAt);
  const secondReceive = await ok(seed, 'receive_simple', { private_key: alice.private_key, send_block_hash: moreFunds.send_block_hash });
  await confirmed(nodes, secondReceive.block_hash);
  assert.match((await rpc(seed, 'receive_simple', { private_key: alice.private_key, send_block_hash: moreFunds.send_block_hash })).error, /no pending receive/);
  console.log('PASS faucet replicates to non-faucet peer; explicit receive; repeat grant preserves identity; double receive denied');

  const bob = await ok(seed, 'wallet_create_full');
  const sent = await ok(seed, 'send_simple', { private_key: alice.private_key, destination: bob.address,
    amount: '1000000000000000', origin: funding.send_block_hash });
  await confirmed(nodes, sent.block_hash);
  const accepted = await ok(seed, 'receive_simple', { private_key: bob.private_key, send_block_hash: sent.block_hash });
  await confirmed(nodes, accepted.block_hash);
  assert.equal((await ok(peer, 'account_info', { account: bob.address })).verification_state, 'unverified');
  assert.match((await rpc(seed, 'send_simple', { private_key: bob.private_key, destination: alice.address,
    amount: '1', origin: funding.send_block_hash })).error, /must be verified/);
  const burn = await ok(seed, 'burn_simple', { private_key: alice.private_key, amount: '1' });
  await confirmed(nodes, burn.block_hash);
  const pocketBurn = await ok(seed, 'receive_simple', { private_key: alice.private_key, send_block_hash: burn.block_hash });
  await confirmed(nodes, pocketBurn.block_hash);
  assert.equal(burn.origin, burn.block_hash);
  console.log('PASS test send/receive/burn preserve provenance across peers; unverified receive allowed, spend denied');

  // Public disposable Dev genesis key only. These RPCs must not rewrite a
  // private engine or account table: peers must derive the same result.
  const publicDevKey = '00'.repeat(32);
  const proposal = await ok(seed, 'governance_propose_simple', {
    private_key: publicDevKey, param: 'brn_rate', new_value: '1',
  });
  await confirmed(nodes, proposal.block_hash);
  const proposalInfo = await ok(seed, 'governance_proposal_info', { hash: proposal.proposal_hash });
  assert.equal(proposalInfo.proposed_value, '1');
  for (const node of nodes) {
    await eventually(async () => assert.deepEqual(
      await ok(node, 'governance_proposal_info', { hash: proposal.proposal_hash }), proposalInfo,
    ), 'proposal must replicate without a local governance shortcut');
  }
  const optedIn = await ok(peer, 'verifier_opt_in', { private_key: publicDevKey });
  await confirmed(nodes, optedIn.block_hash);
  for (const node of nodes) assert.ok((await ok(node, 'account_info', { account: genesis })).verifier_opted_in_at);
  const optedOut = await ok(peer, 'verifier_opt_out', { private_key: publicDevKey });
  await confirmed(nodes, optedOut.block_hash);
  for (const node of nodes) assert.equal((await ok(node, 'account_info', { account: genesis })).verifier_opted_in_at, null);
  console.log('PASS signed governance proposal replication and verifier opt-in/out on a non-faucet node');

  const simultaneous = await Promise.all([1, 2].map(() =>
    ok(seed, 'faucet', { account: alice.address })));
  assert.notEqual(simultaneous[0].send_block_hash, simultaneous[1].send_block_hash);
  for (const grant of simultaneous) await confirmed(nodes, grant.send_block_hash);
  for (const node of nodes) {
    const pending = (await ok(node, 'account_pending', { account: alice.address })).pending;
    for (const grant of simultaneous) assert.equal(
      pending.find(p => p.hash === grant.send_block_hash)?.amount, grant.amount,
    );
    assert.equal((await ok(node, 'account_info', { account: alice.address })).verified_at, verifiedAt);
  }
  console.log('PASS concurrent faucet calls serialize genesis authoring; both pending grants replicate without resetting identity');

  const accounts = [genesis, alice.address, bob.address];
  const expected = await Promise.all(accounts.map(a => state(seed, a)));
  await assertStates(nodes, accounts, expected);
  await Promise.all(nodes.map(n => stop(n.child, 'SIGKILL')));
  const rejectedLog = await open(join(evidence, 'wrong-network-restart.log'), 'a');
  const wrongNetwork = spawn(binary, ['--network', 'live', '--data-dir', join(evidence, seed.name),
    '--port', String(seed.p2p), '--rpc-port', String(seed.rpc), '--disable-upnp', 'node', 'run'],
    { env: daemonEnv, stdio: ['ignore', rejectedLog.fd, rejectedLog.fd] });
  children.push(wrongNetwork);
  await rejectedLog.close();
  await eventually(async () => {
    assert.notEqual(wrongNetwork.exitCode, null, 'wrong-network daemon must exit');
    assert.notEqual(wrongNetwork.exitCode, 0, 'wrong-network daemon must fail');
  }, 'Dev data directory was accepted by Live');
  console.log('PASS Live startup refuses an existing Dev ledger directory');
  seed = await start(seed.name, seed.network, seed.faucet, seed.bootstrap, seed);
  peer = await start(peer.name, peer.network, peer.faucet, seed, peer);
  nodes = [seed, peer];
  await assertStates(nodes, accounts, expected);
  const late = await start('dev-latejoin', 'dev', false, seed);
  await assertStates([late], accounts, expected);
  for (const node of [...nodes, late]) assert.deepEqual(
    await ok(node, 'governance_proposal_info', { hash: proposal.proposal_hash }), proposalInfo,
  );
  console.log('PASS hard restart and fresh non-faucet peer reconstruct identical faucet and payment state');
  const crossNetwork = await start('test-cross-network', 'test', false, seed);
  await delay(2000);
  assert.equal((await ok(crossNetwork, 'node_info')).peer_count, 0);
  assert.equal((await ok(crossNetwork, 'node_info')).block_count, 1);
  console.log('PASS Test node cannot join or import the Dev ledger');
  console.log('PASS test-tool network isolation and replay suite');
} finally {
  await Promise.all(children.map(child => stop(child)));
}
