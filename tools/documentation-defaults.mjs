import {createHash} from 'node:crypto';
import {readFileSync, writeFileSync, readdirSync} from 'node:fs';
import {resolve, dirname} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {execFileSync} from 'node:child_process';
import {sections} from './check-spec-coverage.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const artifact = 'website/protocol-defaults.json';
const inputs = ['types/src/params.rs', 'types/src/amount.rs', 'governance/src/params.rs',
  'governance/src/params/view.rs', 'governance/examples/documentation_defaults.rs'];
const hash = text => createHash('sha256').update(text).digest('hex');
const read = name => readFileSync(resolve(root, name), 'utf8');
const write = (name, text) => writeFileSync(resolve(root, name), text);
const marker = /<!-- default:([\w.]+)(?::([\w]+))? -->([\s\S]*?)<!-- \/default -->/g;

function decimal(value, unit) {
  const whole = value / unit;
  const remainder = value % unit;
  return remainder ? `${whole}.${remainder.toString().padStart(unit.toString().length - 1, '0').replace(/0+$/, '')}` : String(whole);
}

export function formatDefault(data, key, format = 'display') {
  const [network, name, ...extra] = key.split('.');
  const row = data[network]?.find?.(entry => entry.name === name);
  if (!row || extra.length) throw new Error(`Unknown protocol default: ${key}`);
  const value = BigInt(row.current);
  if (format === 'raw') return row.current;
  if (format === 'number') return value.toLocaleString('en-US');
  if (format === 'years') {
    if (row.unit !== 'seconds' || value % 31536000n !== 0n) throw new Error(`${key} is not a whole number of 365-day years; revise the sentence`);
    return String(value / 31536000n);
  }
  if (format === 'percent') return decimal(value, 100n) + '%';
  if (format === 'hourly') return decimal(value * 3600n, BigInt(data.units.brn));
  if (format === 'hourly_approx') {
    // Round to six decimal places for prose; raw values remain exact in the reference.
    const unit = BigInt(data.units.brn);
    return decimal((value * 3600n * 1000000n + unit / 2n) / unit, 1000000n);
  }
  if (format !== 'display') throw new Error(`Unknown default format: ${format}`);
  if (row.unit === 'brn' || row.unit === 'trst') return `${decimal(value, BigInt(data.units[row.unit]))} ${row.unit.toUpperCase()}`;
  if (row.unit === 'brn_per_second') return `${value.toLocaleString('en-US')} raw/second`;
  if (row.unit === 'difficulty') return `0x${value.toString(16).padStart(16, '0')}`;
  if (row.unit === 'seconds') {
    if (value === 18446744073709551615n) return 'Effectively unlimited';
    for (const [unit, seconds] of [['year', 31536000n], ['day', 86400n], ['hour', 3600n], ['minute', 60n]]) {
      if (value > 0n && value % seconds === 0n) {
        const count = value / seconds;
        return `${count} ${unit}${count === 1n ? '' : 's'}`;
      }
    }
    return `${value} seconds`;
  }
  return row.current;
}

export function renderDefaults(text, data) {
  const rendered = text.replace(marker, (_, key, format) =>
    `<!-- default:${key}${format ? ':' + format : ''} -->${formatDefault(data, key, format)}<!-- /default -->`);
  const stripped = rendered.replace(marker, '');
  if (/<!--\s*\/?default\b/.test(stripped)) throw new Error('Malformed protocol-default marker');
  return rendered;
}

function generate() {
  return JSON.parse(execFileSync('cargo', ['run', '--locked', '--profile', 'validation', '-q', '-p',
    'burst-governance', '--example', 'documentation_defaults'], {cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit']}));
}

function main(mode) {
  if (!['--check', '--write', '--verify'].includes(mode)) throw new Error('Usage: node tools/documentation-defaults.mjs [--check|--write|--verify]');
  const sources = Object.fromEntries(inputs.map(name => [name, hash(read(name))]));
  const saved = mode === '--write' ? {sources, defaults: generate()} : JSON.parse(read(artifact));
  if (JSON.stringify(saved.sources) !== JSON.stringify(sources)) throw new Error('Protocol defaults changed. Run node tools/documentation-defaults.mjs --write (through limited-run on the desktop).');
  if (mode === '--verify' && JSON.stringify(saved.defaults) !== JSON.stringify(generate())) throw new Error('Exported defaults differ from Rust. Regenerate documentation defaults.');
  const changed = [];
  for (const file of readdirSync(resolve(root, 'docs'), {recursive: true}).filter(name => name.endsWith('.md'))) {
    const name = `docs/${file}`, before = read(name), after = renderDefaults(before, saved.defaults);
    if (before === after) continue;
    if (mode !== '--write') throw new Error(`Stale protocol default in ${name}. Regenerate documentation defaults.`);
    write(name, after);
    changed.push(name);
  }
  if (mode === '--write') {
    write(artifact, JSON.stringify(saved, null, 2) + '\n');
    const coverage = JSON.parse(read('spec/coverage.json'));
    for (const name of changed) {
      const text = read(name);
      coverage.sources[name].sha256 = hash(text);
      const prior = coverage.requirements.filter(entry => entry.source === name);
      const index = coverage.requirements.findIndex(entry => entry.source === name);
      const updated = sections(name, text).map(section => {
        const old = prior.find(entry => entry.title === section.title && entry.sha256 === section.sha256);
        return old ? {...old, ...section} : {...section, status: 'unverified', evidence: [], note: 'Generated default updated from ProtocolParams; no implementation sign-off implied.'};
      });
      coverage.requirements.splice(index, prior.length, ...updated);
    }
    write('spec/coverage.json', JSON.stringify(coverage, null, 2) + '\n');
  }
  console.log(`Protocol defaults ${mode === '--write' ? 'updated' : 'checked'}; ${saved.defaults.live.length} Live parameters.`);
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main(process.argv[2] ?? '--check');
