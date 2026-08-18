#!/usr/bin/env node
'use strict';

const { createHash } = require('node:crypto');
const { readFileSync, realpathSync, statSync } = require('node:fs');
const { dirname, isAbsolute, relative, resolve, sep } = require('node:path');
const { spawnSync } = require('node:child_process');

function fail(message) {
  process.stderr.write(`csa: ${message}\n`);
  process.exit(1);
}

const packageRoot = resolve(__dirname, '..');
const meta = JSON.parse(readFileSync(resolve(packageRoot, 'package.json'), 'utf8'));
const matrix = JSON.parse(readFileSync(resolve(packageRoot, 'platforms.json'), 'utf8'));
if (matrix.schema !== 1 || !Array.isArray(matrix.platforms)) {
  fail('unsupported platform matrix');
}

const selected = matrix.platforms.find(
  (entry) => entry.os === process.platform && entry.arch === process.arch,
);
if (!selected) {
  fail(`unsupported platform ${process.platform}-${process.arch}`);
}

let platformManifestPath;
try {
  platformManifestPath = require.resolve(`${selected.package}/package.json`);
} catch {
  fail(`required platform package ${selected.package}@${meta.version} is not installed`);
}

let platformManifest;
try {
  platformManifest = JSON.parse(readFileSync(platformManifestPath, 'utf8'));
} catch {
  fail(`cannot read platform package ${selected.package}`);
}

const binding = platformManifest.csa;
if (
  platformManifest.name !== selected.package ||
  platformManifest.version !== meta.version ||
  binding?.schema !== 1 ||
  binding.target !== selected.target ||
  binding.binary !== selected.binary ||
  typeof binding.sha256 !== 'string' ||
  !/^[0-9a-f]{64}$/.test(binding.sha256)
) {
  fail(`invalid platform package metadata for ${selected.package}`);
}

const platformRoot = realpathSync(dirname(platformManifestPath));
const binaryPath = resolve(platformRoot, binding.binary);
if (!isAbsolute(binaryPath)) {
  fail('platform binary path is not absolute');
}
const relativeBinary = relative(platformRoot, binaryPath);
if (relativeBinary === '..' || relativeBinary.startsWith(`..${sep}`)) {
  fail('platform binary escapes its package root');
}

let binaryRealPath;
try {
  binaryRealPath = realpathSync(binaryPath);
  if (!statSync(binaryRealPath).isFile()) {
    fail('platform binary is not a regular file');
  }
} catch {
  fail(`platform binary is missing: ${binding.binary}`);
}
const relativeRealBinary = relative(platformRoot, binaryRealPath);
if (relativeRealBinary === '..' || relativeRealBinary.startsWith(`..${sep}`)) {
  fail('platform binary resolves outside its package root');
}

const actualHash = createHash('sha256').update(readFileSync(binaryRealPath)).digest('hex');
if (actualHash !== binding.sha256) {
  fail(`platform binary checksum mismatch for ${selected.package}`);
}

const child = spawnSync(binaryRealPath, process.argv.slice(2), {
  stdio: 'inherit',
  windowsHide: false,
});
if (child.error) {
  fail(`failed to start platform binary: ${child.error.message}`);
}
if (child.signal) {
  try {
    process.kill(process.pid, child.signal);
  } catch {
    process.exit(1);
  }
} else {
  process.exit(child.status ?? 1);
}
