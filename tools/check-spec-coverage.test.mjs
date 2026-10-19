import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { checkCoverage, sections } from "./check-spec-coverage.mjs";

test("release builds wait for readiness without disabling development CI", () => {
  const release = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  // Check the repository's block-style job declarations, not arbitrary YAML.
  const jobs = new Map([...release.matchAll(/^  ([\w-]+):\n([\s\S]*?)(?=^  [\w-]+:\n|$(?![\s\S]))/gm)]
    .map(([, name, body]) => [name, body]));
  const builds = [...jobs.keys()].filter(name => name.startsWith("build-"));
  assert.equal(builds.length, 5);
  assert.match(jobs.get("validation"), /^    needs: release-readiness$/m);
  for (const name of builds) {
    assert.match(jobs.get(name) ?? "", /^    needs: \[release-readiness, validation\]$/m, name);
    assert.doesNotMatch(jobs.get(name), /^    if:/m, "do not bypass dependency failure");
  }
  assert.match(jobs.get("release-readiness"), /node tools\/check-spec-coverage\.mjs --release/);
  assert.match(jobs.get("release"), /^      - release-readiness$/m);
  assert.doesNotMatch(release, /^  push:/m, "publication requires explicit dispatch");
  assert.match(release, /^  workflow_dispatch:/m);
  const ci = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.match(ci, /^  push:\n    branches: \[main, develop\]$/m);
  assert.doesNotMatch(ci, /needs: release-readiness/);
});

function fixture() {
  const texts = {
    "BURST_WHITEPAPER.md": "# BURST\n\n## Money\nKeep provenance.\n",
    "IMPLEMENTATION_DECISIONS.md": "# Decisions\n\n### 1.1 Scope\nAll payment types.\n",
  };
  const coverage = {
    version: 1,
    sources: Object.fromEntries(Object.entries(texts).map(([name, text]) => [name, { sha256: createHash("sha256").update(text).digest("hex") }])),
    requirements: Object.entries(texts).flatMap(([name, text]) => sections(name, text).map(entry => ({ ...entry, status: "unverified", evidence: [], note: "Needs runtime test." }))),
  };
  return { coverage, texts };
}

test("complete inventory still reports unverified work", () => {
  const { coverage, texts } = fixture();
  const result = checkCoverage(coverage, texts);
  assert.deepEqual(result.errors, []);
  assert.equal(result.open.length, 3);
});

test("missing, duplicate and unknown entries fail", () => {
  for (const mutate of [c => c.requirements.pop(), c => c.requirements.push(c.requirements[0]), c => c.requirements[0].id = "fake"]) {
    const { coverage, texts } = fixture();
    mutate(coverage);
    assert.ok(checkCoverage(coverage, texts).errors.length);
  }
});

test("changing a source cannot silently change the target", () => {
  const { coverage, texts } = fixture();
  texts["BURST_WHITEPAPER.md"] += "Discard provenance.\n";
  const errors = checkCoverage(coverage, texts).errors;
  assert.ok(errors.some(error => error.startsWith("source changed")));
  assert.ok(errors.some(error => error.endsWith("changed sha256")));
});

test("verified needs evidence, a result and a reproduction command", () => {
  const { coverage, texts } = fixture();
  const entry = coverage.requirements[0];
  entry.status = "verified";
  assert.ok(checkCoverage(coverage, texts).errors.length);
  entry.evidence = [{ path: "test.rs", claim: "Provenance survives restart.", result: "pass", command: "cargo test provenance" }];
  assert.ok(checkCoverage(coverage, texts).errors.length, "missing evidence file");
  assert.deepEqual(checkCoverage(coverage, texts, path => path === "test.rs").errors, []);
  entry.evidence[0].result = "fail";
  assert.ok(checkCoverage(coverage, texts, () => true).errors.length);
});

test("exclusions require explicit scope authority", () => {
  const { coverage, texts } = fixture();
  coverage.requirements[0].status = "excluded";
  assert.ok(checkCoverage(coverage, texts).errors.some(error => error.includes("exclusion requires")));
});
