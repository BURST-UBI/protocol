import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {formatDefault, renderDefaults} from './documentation-defaults.mjs';

const data = {
  units: {brn: '1000000000000000000', trst: '1000000000000000000'},
  live: [
    {name: 'trst_expiry_secs', current: '2207520000', unit: 'seconds'},
    {name: 'brn_rate', current: '277777777777778', unit: 'brn_per_second'},
    {name: 'stake', current: '500000000000000000001', unit: 'brn'},
    {name: 'quorum', current: '1', unit: 'basis_points'},
    {name: 'num_verifiers', current: '7', unit: 'count'},
  ],
};

test('formats raw amounts beyond JavaScript integer precision without rounding', () => {
  assert.equal(formatDefault(data, 'live.stake'), '500.000000000000000001 BRN');
  assert.equal(formatDefault(data, 'live.brn_rate', 'hourly'), '1.0000000000000008');
  assert.equal(formatDefault(data, 'live.brn_rate', 'hourly_approx'), '1');
  assert.equal(formatDefault(data, 'live.quorum', 'percent'), '0.01%');
});

test('updates marked defaults while leaving hypothetical examples alone', () => {
  const input = 'Default: <!-- default:live.trst_expiry_secs:years -->30<!-- /default --> years. Example: 30 years.';
  const result = renderDefaults(input, data);
  assert.equal(result, 'Default: <!-- default:live.trst_expiry_secs:years -->70<!-- /default --> years. Example: 30 years.');
  assert.equal(renderDefaults(result, data), result);
});

test('fails closed for unknown fields, formats and malformed markers', () => {
  assert.throws(() => formatDefault(data, 'live.typo'), /Unknown protocol default/);
  assert.throws(() => formatDefault(data, 'live.num_verifiers', 'typo'), /Unknown default format/);
  assert.throws(() => renderDefaults('<!-- default:live.num_verifiers -->7', data), /Malformed/);
});

test('requires a prose review when a duration no longer fits whole years', () => {
  const changed = structuredClone(data);
  changed.live[0].current = '100';
  assert.throws(() => formatDefault(changed, 'live.trst_expiry_secs', 'years'), /revise the sentence/);
  changed.live[0].current = '18446744073709551615';
  assert.equal(formatDefault(changed, 'live.trst_expiry_secs'), 'Effectively unlimited');
  assert.throws(() => formatDefault(changed, 'live.trst_expiry_secs', 'years'), /revise the sentence/);
});

test('every exported Live parameter has a marked default in the reference table', () => {
  const saved = JSON.parse(readFileSync(new URL('../website/protocol-defaults.json', import.meta.url)));
  const markdown = readFileSync(new URL('../docs/protocol/PARAMETERS.md', import.meta.url), 'utf8');
  const names = [...markdown.matchAll(/^\| `([a-z_]+)` \| <!-- default:live\.([a-z_]+)(?::\w+)? -->/gm)];
  for (const match of names) assert.equal(match[1], match[2]);
  assert.deepEqual(names.map(match => match[1]).sort(), saved.defaults.live.map(row => row.name).sort());
});

test('build gate rejects both stale Markdown values and changed Rust sources', async () => {
  const {mkdtempSync, mkdirSync, copyFileSync, writeFileSync, rmSync} = await import('node:fs');
  const {tmpdir} = await import('node:os');
  const {join, dirname} = await import('node:path');
  const {execFileSync} = await import('node:child_process');
  const directory = mkdtempSync(join(tmpdir(), 'burst-doc-defaults-'));
  const saved = JSON.parse(readFileSync(new URL('../website/protocol-defaults.json', import.meta.url)));
  try {
    for (const file of [...Object.keys(saved.sources), 'website/protocol-defaults.json',
      'tools/documentation-defaults.mjs', 'tools/check-spec-coverage.mjs']) {
      mkdirSync(dirname(join(directory, file)), {recursive: true});
      copyFileSync(new URL('../' + file, import.meta.url), join(directory, file));
    }
    mkdirSync(join(directory, 'docs'));
    const key = '<!-- default:live.trst_expiry_secs:years -->';
    const correct = `Expiry is ${key}${formatDefault(saved.defaults, 'live.trst_expiry_secs', 'years')}<!-- /default --> years.`;
    const document = join(directory, 'docs/example.md');
    writeFileSync(document, correct);
    const check = () => execFileSync(process.execPath, [join(directory, 'tools/documentation-defaults.mjs'), '--check'], {encoding: 'utf8', stdio: 'pipe'});
    assert.match(check(), /Protocol defaults checked/);
    writeFileSync(document, correct.replace(key, key + '999'));
    assert.throws(check, /Stale protocol default/);
    writeFileSync(document, correct);
    writeFileSync(join(directory, 'types/src/params.rs'), '// changed source');
    assert.throws(check, /Protocol defaults changed/);
  } finally {
    rmSync(directory, {recursive: true, force: true});
  }
});
