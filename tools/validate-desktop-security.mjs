// Embedded Tauri/WebKit security and IPC contracts, not protocol acceptance.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, open, readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const [kind, binaryArgument] = process.argv.slice(2);
assert(['wallet', 'node'].includes(kind) && binaryArgument,
  'Usage: node validate-desktop-security.mjs wallet|node <embedded binary>');
const binary = resolve(binaryArgument);
const evidence = await mkdtemp(join(tmpdir(), 'burst-desktop-security-'));
const env = { ...process.env, GDK_BACKEND: 'x11' };
delete env.WAYLAND_DISPLAY;
for (const key of ['XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME']) {
  env[key] = join(evidence, key);
  await mkdir(env[key], { mode: 0o700 });
}
const requests = [];
const fixture = createServer(async (request, response) => {
  try {
    let body = '';
    for await (const chunk of request) {
      body += chunk;
      assert(body.length < 16384);
    }
    const query = JSON.parse(body);
    requests.push(query);
    assert.equal(query.action, 'governance_proposals');
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ proposals: [], cursor: query.cursor == null ? 'next-page' : null }));
  } catch {
    response.writeHead(400).end();
  }
});
await new Promise(ready => fixture.listen(0, '127.0.0.1', ready));
async function freePort() {
  const reservation = createServer();
  await new Promise(ready => reservation.listen(0, '127.0.0.1', ready));
  const port = reservation.address().port;
  await new Promise(done => reservation.close(done));
  return port;
}
const driverPort = await freePort();
const nativePort = await freePort();
assert.notEqual(driverPort, nativePort);
const server = `http://127.0.0.1:${driverPort}`;
const log = await open(join(evidence, 'webdriver.log'), 'w', 0o600);
const driver = spawn('xvfb-run', ['-a', '-s', '-screen 0 1440x1000x24',
  process.env.TAURI_DRIVER || 'tauri-driver', '--port', String(driverPort),
  '--native-port', String(nativePort), '--native-driver', '/usr/bin/WebKitWebDriver'],
{ env, detached: true, stdio: ['ignore', log.fd, log.fd] });
let launchError;
driver.on('error', error => { launchError = error; });
const exited = new Promise(done => driver.once('exit', done));
const deadline = AbortSignal.timeout(120000);
let session;
async function call(path, body) {
  const response = await fetch(`${server}${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.any([deadline, AbortSignal.timeout(15000)]),
  });
  const reply = await response.json();
  assert(response.ok && !reply.value?.error, JSON.stringify(reply));
  return reply.value;
}
async function wait(check) {
  for (let i = 0; i < 150; i++) {
    deadline.throwIfAborted();
    if (launchError) throw launchError;
    assert.equal(driver.exitCode, null, `Driver exited: ${evidence}`);
    if (await check()) return;
    await new Promise(done => setTimeout(done, 200));
  }
  assert.fail(`Readiness timed out: ${evidence}`);
}
try {
  await wait(async () => {
    try { await call('/status'); return true; } catch { return false; }
  });
  session = (await call('/session', { capabilities: { alwaysMatch: {
    'tauri:options': { application: binary },
  } } })).sessionId;
  assert(session);
  const execute = (script, args = [], async = false) => call(
    `/session/${session}/execute/${async ? 'async' : 'sync'}`, { script, args });
  await wait(() => execute('return !!document.querySelector("#root")?.children.length && !!window.__TAURI_INTERNALS__?.invoke'));
  const origin = await execute('return location.href');
  assert.match(origin, /^(tauri:\/\/localhost|https?:\/\/tauri\.localhost)/,
    'Must test embedded assets, not a dev server');
  const result = await execute(`
    const kind = arguments[0], rpcUrl = arguments[1], done = arguments[arguments.length - 1];
    (async () => {
      const violations = [];
      document.addEventListener('securitypolicyviolation', event => violations.push(event.effectiveDirective));
      const injected = document.createElement('script');
      injected.textContent = 'window.__burstUnexpectedInline = true';
      document.body.append(injected);
      const positioned = document.createElement('div');
      positioned.style.width = '17px';
      document.body.append(positioned);
      let blockedFetch = false;
      try { await fetch('https://example.invalid/burst-csp-probe'); } catch { blockedFetch = true; }
      const invoke = window.__TAURI_INTERNALS__.invoke;
      let ipc;
      if (kind === 'wallet') ipc = { locked: await invoke('is_locked') };
      else ipc = {
        first: await invoke('get_governance_proposals', { rpcUrl, cursor: null }),
        next: await invoke('get_governance_proposals', { rpcUrl, cursor: 'next-page' }),
      };
      await new Promise(resolve => setTimeout(resolve, 100));
      return { violations, blockedFetch, inlineRan: !!window.__burstUnexpectedInline, ipc,
        font: getComputedStyle(document.body).fontFamily,
        inlineStyles: getComputedStyle(positioned).width === '17px',
        styled: [...document.styleSheets].some(sheet => !!sheet.href) };
    })().then(done, error => done({ error: String(error) }));
  `, [kind, `http://127.0.0.1:${fixture.address().port}`], true);
  assert(!result.error, result.error);
  assert.equal(result.inlineRan, false);
  assert.equal(result.blockedFetch, true);
  assert(result.violations.some(value => value.startsWith('script-src')), 'Inline script must be blocked by CSP');
  assert(result.violations.includes('connect-src'), 'Network request must be blocked by CSP');
  assert(result.styled, 'Bundled stylesheet must load');
  assert(result.inlineStyles, 'Component positioning styles must remain usable');
  if (kind === 'wallet') assert.equal(result.ipc.locked, true);
  else {
    assert.deepEqual(result.ipc.first, { proposals: [], cursor: 'next-page' });
    assert.deepEqual(result.ipc.next, { proposals: [], cursor: null });
    assert.deepEqual(requests.map(query => query.cursor), [null, 'next-page']);
  }
  const sha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
  await writeFile(join(evidence, 'result.json'), JSON.stringify({ kind, binary, sha256, origin, result }, null, 2));
  console.log(`PASS ${kind}: embedded UI, stylesheet, native IPC, blocked inline script and blocked browser network; ${evidence}`);
} finally {
  if (session) await fetch(`${server}/session/${session}`, {
    method: 'DELETE', signal: AbortSignal.timeout(5000),
  }).catch(error => console.error('Session cleanup:', error.message));
  if (driver.pid && driver.exitCode === null) {
    process.kill(-driver.pid, 'SIGTERM');
    const timer = setTimeout(() => {
      if (driver.exitCode === null) process.kill(-driver.pid, 'SIGKILL');
    }, 5000);
    await exited;
    clearTimeout(timer);
  }
  await log.close();
  fixture.closeAllConnections();
  await new Promise(done => fixture.close(done));
  console.log(`Evidence: ${evidence}`);
}
