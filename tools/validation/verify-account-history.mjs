import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

const [reportPath, endpoint] = process.argv.slice(2);
assert(reportPath && endpoint, 'Provide an archival account_history response and target RPC URL');
const expected = JSON.parse(await readFile(reportPath, 'utf8')).result;
assert(expected && Array.isArray(expected.history) && expected.history.length > 0);
assert(!expected.cursor, 'The archival reference must contain the complete account history');
assert.equal(new Set(expected.history.map(entry => entry.hash)).size, expected.history.length);
const rpc = async (action, params = {}) => {
  const response = await fetch(endpoint, {
    method: 'POST', headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({action, account: expected.account, ...params}),
    signal: AbortSignal.timeout(60000),
  });
  assert(response.ok, `HTTP ${response.status}`);
  const reply = await response.json();
  assert(!reply.error && reply.result, JSON.stringify(reply));
  return reply.result;
};
const before = await rpc('account_info');
assert.equal(before.block_count, expected.history.length);
assert.equal(before.confirmation_height, before.block_count);
for (const count of [1, 2, 25]) {
  let cursor;
  const cursors = new Set();
  const entries = [];
  do {
    const page = await rpc('account_history', {count, ...(cursor ? {cursor} : {})});
    assert.equal(page.account, expected.account);
    assert(page.history.length > 0 && page.history.length <= count);
    entries.push(...page.history);
    assert(entries.length <= expected.history.length, 'history repeats entries');
    cursor = page.cursor;
    if (cursor) {
      assert(!cursors.has(cursor), 'pagination did not advance');
      cursors.add(cursor);
    }
  } while (cursor);
  assert.deepEqual(entries, expected.history, `history differs with page size ${count}`);
}
const after = await rpc('account_info');
for (const field of ['head', 'block_count', 'confirmation_height', 'payment_epoch',
  'total_brn_burned', 'total_brn_staked', 'trst_balance', 'trst_expired', 'trst_revoked',
  'verification_state', 'representative']) {
  assert.deepEqual(after[field], before[field], `${field} changed during read-only history recovery`);
}
console.log('PASS complete paginated account history matches archive; balances and authority unchanged');
