import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat, readdir, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const hash = value => createHash('sha256').update(value).digest('hex');
const equal = (a, b) => JSON.stringify(a) === JSON.stringify(b);
// Conservative retry guard only; path-sensitive inventories still govern reuse.
const contentInventory = entries => entries.map(entry => JSON.stringify(entry.slice(1))).sort();
function requireValue(ok, message) { if (!ok) throw new Error(message); }

export async function inventory(paths) {
  requireValue(Array.isArray(paths) && paths.length > 0, 'Explicit input paths are required');
  const entries = [];
  async function visit(path) {
    const stat = await lstat(path);
    requireValue(!stat.isSymbolicLink(), `Declare the real input path, not a symlink: ${path}`);
    if (stat.isDirectory()) {
      entries.push([path, 'directory']);
      for (const name of (await readdir(path)).sort()) await visit(resolve(path, name));
    } else {
      requireValue(stat.isFile(), `Not a regular input file: ${path}`);
      const digest = createHash('sha256');
      for await (const bytes of createReadStream(path)) digest.update(bytes);
      entries.push([path, stat.mode & 0o777, digest.digest('hex')]);
    }
  }
  for (const path of [...new Set(paths.map(path => resolve(path)))].sort()) await visit(path);
  return entries;
}

export async function begin(plan) {
  requireValue(typeof plan.requirement === 'string' && plan.requirement.trim(), 'Requirement is required');
  requireValue(typeof plan.scope === 'string' && plan.scope.trim(), 'Test scope is required');
  requireValue(Array.isArray(plan.command) && plan.command.length > 0 &&
    plan.command.every(arg => typeof arg === 'string'), 'Exact command arguments are required');
  for (const key of ['toolchain', 'features', 'network', 'topology', 'limits']) {
    requireValue(typeof plan.context?.[key] === 'string' && plan.context[key].trim(), `Context ${key} is required`);
  }
  return { version: 1, plan, cwd: process.cwd(), inputs: await inventory(plan.inputs), started: new Date().toISOString() };
}

export async function finish(start, result, recordInvalidated = false) {
  requireValue(start.version === 1 && start.cwd === process.cwd(), 'Use the original working directory');
  const observedInputs = await inventory(start.plan.inputs);
  const inputsChanged = !equal(start.inputs, observedInputs);
  requireValue(!inputsChanged || recordInvalidated, 'Inputs changed during validation');
  requireValue(['pass', 'fail', 'incomplete'].includes(result.outcome), 'Explicit outcome is required');
  requireValue(Number.isInteger(result.exitCode), 'Terminal exit code is required');
  requireValue(Number.isInteger(result.assertions) && result.assertions >= 0, 'Executed assertion count is required');
  requireValue(typeof result.review === 'string' && result.review.trim(), 'Evidence review is required');
  requireValue(result.outcome !== 'pass' || (result.exitCode === 0 && result.assertions > 0),
    'A pass requires exit zero and executed assertions');
  if (recordInvalidated) {
    requireValue(inputsChanged, 'Use ordinary finish when inputs did not change');
    requireValue(result.outcome !== 'pass', 'Invalidated inputs can never qualify a pass');
    requireValue(typeof result.inputMutationReason === 'string' && result.inputMutationReason.trim(),
      'Explicit inputMutationReason is required to preserve an invalidated attempt');
  }
  return { ...start, result, ...(recordInvalidated ? {
    invalidation: { reason: result.inputMutationReason, observedInputs },
  } : {}), evidence: await inventory(result.logs), finished: new Date().toISOString() };
}

export async function check(plan, receipt) {
  requireValue(!receipt.invalidation, 'Inputs changed during validation; invalidated attempt cannot qualify reuse');
  const current = await begin(plan);
  requireValue(receipt.version === 1 && receipt.cwd === current.cwd && equal(receipt.plan, plan),
    'Scope, command, context or working directory changed');
  requireValue(equal(receipt.inputs, current.inputs), 'Input contents or inventory changed');
  requireValue(equal(receipt.evidence, await inventory(receipt.result.logs)), 'Evidence changed');
  requireValue(['pass', 'fail', 'incomplete'].includes(receipt.result.outcome), 'Invalid recorded outcome');
  if (receipt.result.outcome === 'pass') {
    requireValue(receipt.result.exitCode === 0 && receipt.result.assertions > 0, 'Invalid pass record');
  }
  return { outcome: receipt.result.outcome, scope: plan.scope,
    fingerprint: hash(JSON.stringify(current.inputs)),
    action: receipt.result.outcome === 'pass' ? 'Reuse only this reviewed scope; no release approval implied.' :
      'Do not repeat unchanged validation. Diagnose or change inputs; this is not a pass.' };
}

export async function prepare(plan, receipt) {
  const current = await begin(plan);
  requireValue(receipt.version === 1 && receipt.plan && Array.isArray(receipt.inputs),
    'Invalid prior receipt; inspect it before scheduling validation');
  requireValue(equal(receipt.evidence, await inventory(receipt.result.logs)), 'Prior evidence changed');
  const changed = [];
  if (receipt.cwd !== current.cwd) changed.push('working directory');
  if (!equal(receipt.plan, plan)) changed.push('scope, command or context');
  if (!equal(receipt.inputs, current.inputs)) changed.push('input contents or inventory');
  if (changed.length === 0) {
    const decision = await check(plan, receipt);
    throw new Error(`Unchanged validation (${decision.outcome}). ${decision.action}`);
  }
  let retrySequence;
  if (equal(contentInventory(receipt.inputs), contentInventory(current.inputs))) {
    requireValue(typeof plan.rerun?.reason === 'string' && plan.rerun.reason.trim(),
      'Unchanged inputs require an explicit rerun.reason; renaming a scope is not justification');
    requireValue(Number.isInteger(plan.rerun?.attemptBudget) && plan.rerun.attemptBudget > 0,
      'Unchanged inputs require a positive rerun.attemptBudget');
    const previous = receipt.retrySequence;
    if (previous) {
      requireValue(Number.isInteger(previous.used) && previous.used > 0 &&
        Number.isInteger(previous.budget) && previous.budget >= previous.used,
      'Invalid prior retry sequence');
      requireValue(plan.rerun.attemptBudget === previous.budget,
        'Cannot raise or reset the retry budget for unchanged inputs');
    }
    retrySequence = { used: (previous?.used ?? 0) + 1,
      budget: previous?.budget ?? plan.rerun.attemptBudget };
    requireValue(retrySequence.used <= retrySequence.budget,
      'Retry budget exhausted; diagnose or change inputs before another run');
  }
  return { ...current, ...(retrySequence ? { retrySequence } : {}), previousAttempt: {
    started: receipt.started, finished: receipt.finished,
    changed,
  } };
}

// Report stages independently: a failed scenario cannot invalidate its build.
export async function audit(stages) {
  requireValue(Array.isArray(stages) && stages.length > 0, 'Explicit stages are required');
  const names = new Set();
  for (const stage of stages) {
    requireValue(typeof stage.name === 'string' && stage.name.trim() && !names.has(stage.name),
      'Stage names must be nonempty and unique');
    names.add(stage.name);
    requireValue(typeof stage.plan === 'string' && typeof stage.receipt === 'string',
      'Each stage needs plan and latest receipt paths');
  }
  const results = [];
  for (const stage of stages) {
    try {
      const plan = JSON.parse(await readFile(stage.plan, 'utf8'));
      const receipt = JSON.parse(await readFile(stage.receipt, 'utf8'));
      const decision = await check(plan, receipt);
      results.push({ name: stage.name, status: decision.outcome === 'pass' ? 'reuse' : 'diagnose',
        ...decision });
    } catch (error) {
      results.push({ name: stage.name, status: 'unverified', reason: error.message,
        action: 'Inspect changed or missing evidence before scheduling work. No automatic rerun.' });
    }
  }
  return results;
}

async function main(args) {
  const [mode, ...paths] = args;
  const json = async path => JSON.parse(await readFile(path, 'utf8'));
  let value;
  if (mode === 'begin' && paths.length === 2) value = await begin(await json(paths[0]));
  else if (mode === 'prepare' && paths.length === 3) value = await prepare(await json(paths[0]), await json(paths[1]));
  else if (mode === 'finish' && paths.length === 3) value = await finish(await json(paths[0]), await json(paths[1]));
  else if (mode === 'finish-invalidated' && paths.length === 3) value = await finish(await json(paths[0]), await json(paths[1]), true);
  else if (mode === 'check' && paths.length === 2) {
    console.log(JSON.stringify(await check(await json(paths[0]), await json(paths[1])), null, 2));
    return;
  } else if (mode === 'audit' && paths.length === 1) {
    const results = await audit(await json(paths[0]));
    console.log(JSON.stringify(results, null, 2));
    process.exitCode = results.every(stage => stage.status === 'reuse') ? 0 : 1;
    return;
  } else throw new Error('Usage: begin PLAN START | prepare PLAN PRIOR_RECEIPT START | finish PLAN_START RESULT RECEIPT | finish-invalidated PLAN_START FAILED_RESULT RECEIPT | check PLAN RECEIPT | audit STAGES');
  await writeFile(paths.at(-1), `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
