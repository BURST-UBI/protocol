import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, copyFileSync, chmodSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const slow = "confirmed_replay::tests::certified_votes::challenges::deadline::challenge_deadline_and_pending_expiry_preserve_fraud_and_inactivity_accounting";
const names = [
  "block_processor::tests::ordinary", "checkpoint_sync::input::tests::fairness",
  slow,
  "certified_network_tests::pending_expiry::return_after_restart",
  "confirmed_replay::tests::certified_votes::installation::process_crash::publication::publication_crashes_restore_caches_and_admit_only_the_installed_epoch",
  "confirmed_replay::tests::checkpoint_service_agrees_over_tcp_only_after_seven_of_nine_humans_join",
  "confirmed_replay::tests::snapshot_export::snapshot_peer_transfer_stages_authenticated_archive_over_tcp_and_restarts",
  "confirmed_replay::tests::checkpoint_runtime_builds_candidates_and_advances_late_nodes_over_tcp",
  "confirmed_replay::tests::certified_votes::challenges::outcomes::transport::clockless",
  "checkpoint_store::transfer::wire::chunks",
];
const script = fileURLToPath(new URL("./test-node.sh", import.meta.url));

function run(args, overrides = {}, affinity, prebuilt = false) {
  const dir = mkdtempSync(join(tmpdir(), "burst-test-runner-"));
  try {
    const cargo = join(dir, "cargo"), log = join(dir, "calls.jsonl");
    copyFileSync(new URL("./fixtures/mock-cargo.mjs", import.meta.url), cargo);
    chmodSync(cargo, 0o700);
    const env = { ...process.env };
    delete env.BURST_NODE_TEST_BINARY;
    for (const key of ["BURST_TEST_CPUS", "CARGO_BUILD_JOBS", "TOKIO_WORKER_THREADS", "BURST_TEST_NETWORK_CONCURRENCY", "BURST_TEST_NETWORK_SHARD", "BURST_TEST_NETWORK_SHARDS"]) delete env[key];
    Object.assign(env, { PATH: `${dir}:${env.PATH}`, MOCK_CARGO_LOG: log, MOCK_TEST_NAMES: JSON.stringify(names) }, overrides);
    if (prebuilt) {
      const binary = join(dir, 'node-tests');
      copyFileSync(cargo, binary);
      chmodSync(binary, 0o700);
      env.BURST_NODE_TEST_BINARY = binary;
    }
    const command = affinity ? "taskset" : "bash";
    const commandArgs = affinity ? ["-c", affinity, "bash", script, ...args] : [script, ...args];
    const result = spawnSync(command, commandArgs, { env, encoding: "utf8", timeout: 10_000 });
    assert.ifError(result.error);
    let calls = [];
    try { calls = readFileSync(log, "utf8").trim().split("\n").filter(Boolean).map(JSON.parse); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    return { ...result, calls };
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

function cpus(list) {
  return list.split(",").flatMap(range => {
    const [first, last = first] = range.split("-").map(Number);
    return Array.from({ length: last - first + 1 }, (_, index) => first + index);
  });
}

test("all groups cover the catalog once and carry a low-priority CPU budget", () => {
  const result = run(["all", "--list"]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.calls.length, 3);
  assert.deepEqual(result.calls.map(call => call.selected.length), [2, 1, 7]);
  assert.deepEqual(result.calls.flatMap(call => call.selected).sort(), [...names].sort());
  for (const call of result.calls) {
    assert(call.args.includes("--locked"));
    assert.equal(call.args[call.args.indexOf("--profile") + 1], "validation");
    assert.equal(call.jobs, "1");
    assert(call.priority >= 15);
    if (call.affinity) {
      const count = cpus(call.affinity).length;
      assert(count >= 1 && count <= 2);
      assert.equal(call.tokio, String(count));
    }
  }
});

test("focused mode verifies an exact name before executing only that test", () => {
  const result = run(["focused", names[0]]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.calls.length, 2);
  assert(result.calls[0].args.includes("--list"));
  for (const call of result.calls) assert.deepEqual(call.selected, [names[0]]);
  assert(result.calls[1].args.includes("--test-threads=1"));
  const missing = run(["focused", "ordinary"]);
  assert.equal(missing.status, 2);
  assert.equal(missing.calls.length, 1);
  assert.match(missing.stderr, /No exact node library test/);
  const listing = run(["focused", names[0], "--list"]);
  assert.equal(listing.status, 0);
  assert.equal(listing.calls.length, 1);
});

test('prebuilt execution preserves filters, sharding and limits without invoking Cargo', () => {
  for (const args of [['all', '--list'], ['focused', names[0]], ['network']]) {
    const env = args[0] === 'network'
      ? { BURST_TEST_NETWORK_SHARDS: '3', BURST_TEST_NETWORK_SHARD: '1' } : {};
    const built = run(args, env);
    const reused = run(args, env, undefined, true);
    assert.equal(reused.status, 0, reused.stderr);
    assert.deepEqual(reused.calls.map(c => c.selected), built.calls.map(c => c.selected));
    for (const call of reused.calls) {
      assert(!call.args.includes('--locked'));
      assert(!call.args.includes('--profile'));
      assert(!call.args.includes('--'));
      assert(call.priority >= 15);
    }
  }
  const failed = run(['all'], { MOCK_CARGO_FAIL: '1' }, undefined, true);
  assert.equal(failed.status, 1);
  assert.equal(failed.calls.length, 1);
  for (const binary of ['relative', '/missing/burst-node-tests']) {
    const invalid = run(['fast'], { BURST_NODE_TEST_BINARY: binary });
    assert.equal(invalid.status, 2);
    assert.equal(invalid.calls.length, 0);
  }
});

test("invalid arguments and budgets never start Cargo", () => {
  for (const [args, env] of [
    [["unknown"], {}], [["focused"], {}], [["fast", "extra"], {}],
    [["focused", names[0], "--other"], {}], [["all", "--list", "extra"], {}],
    [["focused", `missing\n${names[0]}`], {}],
    [["fast"], { BURST_TEST_CPUS: "0" }], [["fast"], { BURST_TEST_CPUS: "-1" }],
    [["fast"], { BURST_TEST_CPUS: "2x" }], [["network"], { BURST_TEST_NETWORK_CONCURRENCY: "3" }],
    [["network"], { BURST_TEST_NETWORK_SHARDS: "0" }],
    [["network"], { BURST_TEST_NETWORK_SHARDS: "2", BURST_TEST_NETWORK_SHARD: "2" }],
    [["network"], { BURST_TEST_NETWORK_SHARD: "-1" }],
    [["all"], { BURST_TEST_NETWORK_SHARDS: "2" }],
  ]) {
    const result = run(args, env);
    assert.equal(result.status, 2, JSON.stringify({ args, env, stderr: result.stderr }));
    assert.equal(result.calls.length, 0);
  }
});

test("network shards cover the same catalog exactly once with exact names", () => {
  const whole = run(["network", "--list"]);
  const executions = [];
  for (let shard = 0; shard < 3; shard++) {
    const env = { BURST_TEST_NETWORK_SHARDS: "3", BURST_TEST_NETWORK_SHARD: String(shard) };
    const result = run(["network"], env);
    assert.equal(result.status, 0, result.stderr);
    const listed = run(["network", "--list"], env);
    assert.equal(listed.status, 0, listed.stderr);
    assert.equal(listed.calls.length, 1, "listing must never execute a test");
    const selected = result.calls.slice(1).flatMap(call => {
      assert(call.args.includes("--exact"));
      assert(call.args.includes("--locked"));
      assert.equal(call.selected.length, 1);
      return call.selected;
    });
    assert.deepEqual(listed.stdout.trim().split("\n").filter(Boolean).map(line => line.replace(/: test$/, "")), selected);
    executions.push(...selected);
  }
  assert.deepEqual(executions.sort(), whole.calls[0].selected.sort());
  assert.equal(new Set(executions).size, executions.length);
  const empty = run(["network"], { BURST_TEST_NETWORK_SHARDS: "9", BURST_TEST_NETWORK_SHARD: "8" });
  assert.equal(empty.status, 0, empty.stderr);
  assert.equal(empty.calls.length, 1);
});

test("a failed network case does not hide the rest of its shard", () => {
  const env = {BURST_TEST_NETWORK_SHARDS:"2", BURST_TEST_NETWORK_SHARD:"0"};
  const baseline = run(["network"], env);
  const failing = baseline.calls[1].selected[0];
  for (const prebuilt of [false, true]) {
    const result = run(["network"], {...env, MOCK_CARGO_FAIL_TEST:failing}, undefined, prebuilt);
    assert.equal(result.status, 1);
    assert.deepEqual(result.calls.map(call => call.selected), baseline.calls.map(call => call.selected));
    assert(result.stderr.includes(`Failed network test: ${failing}`));
  }
});

test("all stops immediately when a group fails", () => {
  const result = run(["all"], { MOCK_CARGO_FAIL: "1" });
  assert.equal(result.status, 1);
  assert.equal(result.calls.length, 1);
});

test("CPU selection respects an existing restricted affinity", { skip: process.platform !== "linux" }, () => {
  const allowed = cpus(readFileSync("/proc/self/status", "utf8").match(/^Cpus_allowed_list:\s*(.+)$/m)[1]);
  const subset = [allowed.at(-1)];
  const result = run(["fast", "--list"], {}, subset.join(","));
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(cpus(result.calls[0].affinity), subset);
  assert.equal(result.calls[0].tokio, "1");
  assert(result.calls[0].args.includes("--test-threads=1"));
});

test("explicit build and runtime settings survive the CPU cap", () => {
  const result = run(["network", "--list"], {
    CARGO_BUILD_JOBS: "2", TOKIO_WORKER_THREADS: "3", BURST_TEST_CPUS: "1",
    BURST_TEST_NETWORK_CONCURRENCY: "2",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.calls[0].jobs, "2");
  assert.equal(result.calls[0].tokio, "3");
  assert(result.calls[0].args.includes("--test-threads=2"));
  if (result.calls[0].affinity) assert.equal(cpus(result.calls[0].affinity).length, 1);
});
