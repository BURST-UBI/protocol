#!/usr/bin/env node
import { writeFileSync } from 'node:fs';
import { basename } from 'node:path';
if (basename(process.argv[1]) === 'awk') {
  console.log(process.env.MOCK_AVAILABLE_KIB);
} else {
  writeFileSync(process.env.MOCK_LIMITED_LOG, JSON.stringify(process.argv.slice(2)));
  process.exit(Number(process.env.MOCK_LIMITED_STATUS || 0));
}
