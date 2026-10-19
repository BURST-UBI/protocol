import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { strict as assert } from 'node:assert';
import { test } from 'node:test';

async function endpoint(t, response) {
  let calls = 0;
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', chunk => { body += chunk; });
    req.on('end', () => {
      assert.deepEqual(JSON.parse(body), { action: 'node_info' });
      res.end(response(++calls));
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  return { url: `http://127.0.0.1:${server.address().port}`, calls: () => calls };
}

function readiness(budget, urls) {
  return new Promise((resolve, reject) => {
    const child = spawn('bash', ['-c', 'source network/tests/nat/common.sh; wait_for_node_rpcs dev "$@"', 'bash', String(budget), ...urls], {
      cwd: new URL('..', import.meta.url),
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: 5000,
    });
    let stderr = '';
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.on('error', reject);
    child.on('close', code => resolve({ code, stderr }));
  });
}

test('waits for every RPC, including a delayed client after the seed is ready', async t => {
  const seed = await endpoint(t, () => '{"result":{"network":"dev"}}');
  const client = await endpoint(t, calls => calls < 3 ? '{"error":"starting"}' : '{"result":{"network":"dev"}}');
  assert.equal((await readiness(3, [seed.url, client.url])).code, 0);
  assert.ok(client.calls() >= 3);
  assert.ok(seed.calls() >= 3);
});

for (const response of ['{"result":{"network":"live"}}', 'not json', '{"error":"not ready"}']) {
  test(`rejects an unavailable or wrong-network RPC: ${response}`, async t => {
    const node = await endpoint(t, () => response);
    const result = await readiness(1, [node.url]);
    assert.equal(result.code, 1);
    assert.match(result.stderr, /RPC readiness failed/);
    assert.ok(node.calls() > 0);
  });
}
