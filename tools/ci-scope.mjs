import { execFileSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function needsRuntime(paths) {
  // Website builds have their own workflow. Other workflows, toolchains,
  // fixtures and scripts require runtime-check selection.
  return paths.some(path => !(
    path === 'spec/coverage.json' ||
    path.startsWith('website/') ||
    path === 'vercel.json' ||
    path === '.github/workflows/website.yml' ||
    (path.endsWith('.md') && (path.startsWith('docs/') || path.startsWith('spec/') || !path.includes('/')))
  ));
}

export function selectChecks(runtime, suite = 'all') {
  if (!['all', 'windows', 'integration', 'node'].includes(suite)) throw Error('Unknown CI suite');
  const full = suite === 'all' && runtime;
  return { runtime: full, windows: suite === 'windows' || full, integration: suite === 'integration' || full };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const base = process.env.CI_BASE;
  let runtime = true;
  if (base && !/^0+$/.test(base)) {
    const paths = execFileSync('git', ['diff', '--name-only', '-z', base, 'HEAD'])
      .toString().split('\0').filter(Boolean);
    runtime = needsRuntime(paths);
  }
  const checks = selectChecks(runtime, process.env.CI_SUITE || 'all');
  appendFileSync(process.env.GITHUB_OUTPUT, Object.entries(checks).map(([key,value]) => `${key}=${value}\n`).join(''));
}
