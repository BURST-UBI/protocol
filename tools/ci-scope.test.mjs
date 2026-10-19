import { test } from 'node:test';
import assert from 'node:assert/strict';
import { needsRuntime } from './ci-scope.mjs';

test('documentation and coverage updates retain inventory but skip Rust', () => {
  assert.equal(needsRuntime(['docs/BURST_FULL_GUIDE.md', 'spec/coverage.json', 'README.md']), false);
});
test('code, build inputs, workflows and unknown paths run runtime validation', () => {
  for (const path of ['node/src/node.rs', 'Cargo.lock', '.cargo/config.toml',
    '.github/workflows/ci.yml', 'tools/test-node.sh', 'new-input', 'node/fixture.md']) {
    assert.equal(needsRuntime(['spec/coverage.json', path]), true, path);
  }
});
