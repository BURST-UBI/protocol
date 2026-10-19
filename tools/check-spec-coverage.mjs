import { createHash } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { dirname, resolve, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const sourceNames = ["BURST_WHITEPAPER.md", "IMPLEMENTATION_DECISIONS.md"];
const statuses = new Set(["unverified", "partial", "decision-conflict", "verified", "excluded"]);
const sha256 = text => createHash("sha256").update(text).digest("hex");

export function sections(name, text) {
  const lines = text.split("\n");
  const starts = [];
  lines.forEach((line, i) => {
    if (name === sourceNames[0] ? /^#{1,3} /.test(line) : /^### \d+\.\d+ /.test(line)) starts.push(i);
  });
  return starts.map((start, i) => {
    const title = lines[start].replace(/^#+ /, "");
    const decision = name === sourceNames[1] && title.match(/^(\d+\.\d+) /);
    return {
      id: decision ? `D-${decision[1]}` : `W-${String(i + 1).padStart(3, "0")}`,
      source: name,
      line: start + 1,
      title,
      sha256: sha256(lines.slice(start, starts[i + 1] ?? lines.length).join("\n")),
    };
  });
}

export function checkCoverage(coverage, sourceTexts, fileExists = () => false) {
  const errors = [];
  if (coverage.version !== 1) errors.push("unsupported coverage version");
  const expected = new Map();
  for (const name of sourceNames) {
    const text = sourceTexts[name];
    if (typeof text !== "string") { errors.push(`missing source ${name}`); continue; }
    if (coverage.sources?.[name]?.sha256 !== sha256(text)) errors.push(`source changed: ${name}`);
    for (const entry of sections(name, text)) {
      if (expected.has(entry.id)) errors.push(`duplicate source section ${entry.id}`);
      expected.set(entry.id, entry);
    }
  }
  if (!Array.isArray(coverage.requirements)) return { errors: [...errors, "requirements must be an array"], open: [] };
  const seen = new Set();
  const open = [];
  for (const entry of coverage.requirements) {
    if (!entry || typeof entry !== "object") { errors.push("invalid requirement record"); continue; }
    const source = expected.get(entry.id);
    if (!source || seen.has(entry.id)) errors.push(`unknown or duplicate entry ${entry.id}`);
    seen.add(entry.id);
    if (source) for (const key of ["source", "line", "title", "sha256"]) {
      if (entry[key] !== source[key]) errors.push(`${entry.id}: changed ${key}`);
    }
    if (!statuses.has(entry.status)) errors.push(`${entry.id}: invalid status`);
    if (typeof entry.note !== "string" || !entry.note.trim()) errors.push(`${entry.id}: missing review note`);
    if (!Array.isArray(entry.evidence)) { errors.push(`${entry.id}: evidence must be an array`); continue; }
    for (const evidence of entry.evidence) {
      if (!evidence || typeof evidence.path !== "string" || !fileExists(evidence.path)) errors.push(`${entry.id}: missing evidence file`);
      if (typeof evidence?.claim !== "string" || !evidence.claim.trim()) errors.push(`${entry.id}: missing evidence claim`);
    }
    if (entry.status === "verified") {
      if (!entry.evidence.length || !entry.evidence.every(e => e.result === "pass" && typeof e.command === "string" && e.command.trim())) {
        errors.push(`${entry.id}: verified requires passing evidence and reproduction commands`);
      }
    } else if (entry.status === "excluded") {
      if (!entry.evidence.length || typeof entry.authorization !== "string" || !entry.authorization.trim()) {
        errors.push(`${entry.id}: exclusion requires evidence and explicit scope authorization`);
      }
    } else open.push(entry.id);
  }
  for (const id of expected.keys()) if (!seen.has(id)) errors.push(`missing entry ${id}`);
  return { errors, open };
}

function main() {
  const args = process.argv.slice(2);
  if (args.some(arg => !["--release", "--list-open"].includes(arg))) throw new Error("usage: node tools/check-spec-coverage.mjs [--release] [--list-open]");
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
  const coverage = JSON.parse(readFileSync(resolve(root, "spec/coverage.json"), "utf8"));
  const sources = Object.fromEntries(sourceNames.map(name => [name, readFileSync(resolve(root, name), "utf8")]));
  const fileExists = path => {
    const local = relative(root, resolve(root, path));
    if (!local || local.startsWith("..") || path.startsWith("/")) return false;
    try { return statSync(resolve(root, path)).isFile(); } catch { return false; }
  };
  const { errors, open } = checkCoverage(coverage, sources, fileExists);
  for (const error of errors) console.error(error);
  if (args.includes("--list-open")) for (const id of open) {
    const entry = coverage.requirements.find(item => item.id === id);
    console.log(`${id}\t${entry.status}\t${entry.title}\t${entry.note}`);
  }
  console.log(`${coverage.requirements.length} source sections indexed; ${open.length} not signed off; ${errors.length} coverage errors.`);
  console.log("Index coverage does not establish implementation completeness or security.");
  if (errors.length) process.exitCode = 1;
  else if (args.includes("--release") && open.length) process.exitCode = 2;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
