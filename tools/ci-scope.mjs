import { execFileSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function needsRuntime(paths) {
  // Only known documentation paths can skip runtime checks. Unknown inputs
  // (including workflows, toolchains, fixtures and scripts) run the full suite.
  return paths.some(path => !(
    path.startsWith('docs/') || path.startsWith('spec/') ||
    (!path.includes('/') && path.endsWith('.md'))
  ));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const base = process.env.CI_BASE;
  let runtime = true;
  if (base && !/^0+$/.test(base)) {
    const paths = execFileSync('git', ['diff', '--name-only', '-z', base, 'HEAD'])
      .toString().split('\0').filter(Boolean);
    runtime = needsRuntime(paths);
  }
  appendFileSync(process.env.GITHUB_OUTPUT, `runtime=${runtime}\n`);
}
