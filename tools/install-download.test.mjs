import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const installer = readFileSync(new URL("../install.sh", import.meta.url), "utf8");
assert(installer.indexOf("# Detect platform") > installer.indexOf("download_verified_binary()"), "installer helper boundaries must exist");
const helper = installer.slice(installer.indexOf("download_verified_binary()"), installer.indexOf("# Detect platform"));
const asset = "burst-daemon-linux-amd64";
const binary = "verified release bytes\n";
const hash = createHash("sha256").update(binary).digest("hex");

function download(sums, body = binary, failure = "") {
  const directory = mkdtempSync(join(tmpdir(), "burst-download-"));
  try {
    const mocks = join(directory, "tools");
    mkdirSync(mocks);
    writeFileSync(join(mocks, "curl"), `#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
const args = process.argv.slice(2);
const sums = args.at(-1).endsWith('/SHA256SUMS');
if (!args.includes('--proto') || !args.includes('--proto-redir')) process.exit(88);
writeFileSync(args[args.indexOf('-o') + 1], sums ? process.env.MOCK_SUMS : process.env.MOCK_BINARY);
if (process.env.MOCK_FAILURE === (sums ? 'sums' : 'binary')) process.exit(22);
`, { mode: 0o700 });
    const destination = join(directory, "installed daemon");
    writeFileSync(destination, "original binary", { mode: 0o700 });
    const result = spawnSync("sh", ["-c", `RELEASE_URL=https://example.test/releases\n${helper}\nif download_verified_binary "$1" "$2"; then exit 0; else exit 1; fi`, "test", destination, asset], {
      env: { ...process.env, PATH: `${mocks}:${process.env.PATH}`, MOCK_SUMS: sums, MOCK_BINARY: body, MOCK_FAILURE: failure },
      encoding: "utf8", timeout: 10_000,
    });
    assert.ifError(result.error);
    assert.deepEqual(readdirSync(directory).sort(), ["installed daemon", "tools"], "staging files must be cleaned");
    return { ...result, contents: readFileSync(destination, "utf8"), mode: statSync(destination).mode & 0o777 };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("verified release replaces the binary atomically with executable permissions", () => {
  const result = download(`${hash}  ${asset}\n${"0".repeat(64)}  another-platform\n`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.contents, binary);
  assert.equal(result.mode, 0o755);
});

test("malformed, missing, duplicate and wrong-platform checksums preserve the installed binary", () => {
  for (const sums of ["", `${hash}  ${asset}.extra\n`, `wrong  ${asset}\n`, `${hash}  ${asset}\n${hash}  ${asset}\n`, `${hash}  ${asset}  extra\n`]) {
    const result = download(sums);
    assert.notEqual(result.status, 0, sums);
    assert.equal(result.contents, "original binary");
  }
});

test("mismatch and failed HTTP transfers never replace the old executable", () => {
  for (const [body, failure] of [["tampered", ""], [binary, "sums"], [binary, "binary"]]) {
    const result = download(`${hash}  ${asset}\n`, body, failure);
    assert.notEqual(result.status, 0);
    assert.equal(result.contents, "original binary");
  }
});
