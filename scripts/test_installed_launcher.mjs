#!/usr/bin/env node

import assert from 'node:assert/strict';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const [prefixValue, expected = 'csa 0.1.8'] = process.argv.slice(2);
if (!prefixValue || !path.isAbsolute(prefixValue)) {
  process.stderr.write('usage: node scripts/test_installed_launcher.mjs <absolute-prefix> [expected]\n');
  process.exit(2);
}
const launcher = path.join(
  prefixValue,
  'node_modules',
  '@dslzl',
  'csa',
  'bin',
  'csa.js',
);
const result = spawnSync(process.execPath, [launcher, '--version'], {
  cwd: prefixValue,
  env: process.env,
  encoding: 'utf8',
});
assert.equal(result.status, 0, result.stderr);
assert.match(result.stdout, new RegExp(expected.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
process.stdout.write(
  `${JSON.stringify({ schema: 1, result: 'pass', launcher, expected }, null, 2)}\n`,
);
