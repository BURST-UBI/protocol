#!/usr/bin/env node
import { appendFileSync, readFileSync } from "node:fs";
import { getPriority } from "node:os";

const args = process.argv.slice(2);
const options = args.slice(args.indexOf("--") + 1);
const filters = [], skip = [];
for (let index = 0; index < options.length; index++) {
  const value = options[index];
  if (value === "--skip") skip.push(options[++index]);
  else if (!value.startsWith("--")) filters.push(value);
}
const names = JSON.parse(process.env.MOCK_TEST_NAMES);
const selected = names.filter(name =>
  (!filters.length || filters.some(filter => options.includes("--exact") ? name === filter : name.includes(filter)))
  && !skip.some(filter => name.includes(filter)));
appendFileSync(process.env.MOCK_CARGO_LOG, JSON.stringify({
  args, selected, jobs: process.env.CARGO_BUILD_JOBS,
  tokio: process.env.TOKIO_WORKER_THREADS, priority: getPriority(),
  affinity: process.platform === "linux"
    ? readFileSync("/proc/self/status", "utf8").match(/^Cpus_allowed_list:\s*(.+)$/m)[1] : null,
}) + "\n");
if (process.env.MOCK_CARGO_FAIL === "1") process.exit(1);
if (options.includes("--list")) {
  for (const name of selected) console.log(`${name}: test`);
  console.log(`\n${selected.length} tests, 0 benchmarks`);
}
