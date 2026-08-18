#!/usr/bin/env node

import { createHash } from 'node:crypto';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

function usage() {
  return 'usage: node scripts/stage_npm_packages.mjs --out <new-absolute-dir> --binary <platform-id>=<absolute-binary> [--binary ...]';
}

function die(message) {
  process.stderr.write(`${message}\n${usage()}\n`);
  process.exit(2);
}

function copyText(source, destination) {
  const text = readFileSync(source, 'utf8').replace(/\r\n?/g, '\n');
  writeFileSync(destination, text);
}

const args = process.argv.slice(2);
let output;
const binaryArgs = [];
for (let index = 0; index < args.length; index += 1) {
  if (args[index] === '--out') {
    output = args[++index];
  } else if (args[index] === '--binary') {
    binaryArgs.push(args[++index]);
  } else {
    die(`unknown argument: ${args[index]}`);
  }
}
if (!output || !isAbsolute(output) || binaryArgs.length === 0) {
  die('an absolute --out and at least one --binary are required');
}
output = resolve(output);
if (existsSync(output)) {
  die(`output already exists: ${output}`);
}

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const metaSource = resolve(repository, 'npm', 'meta');
const matrix = JSON.parse(readFileSync(resolve(metaSource, 'platforms.json'), 'utf8'));
const metaManifest = JSON.parse(readFileSync(resolve(metaSource, 'package.json'), 'utf8'));
if (matrix.schema !== 1 || !Array.isArray(matrix.platforms)) {
  die('unsupported platform matrix');
}
const expectedOptional = Object.fromEntries(
  matrix.platforms.map((platform) => [platform.package, metaManifest.version]),
);
if (JSON.stringify(metaManifest.optionalDependencies) !== JSON.stringify(expectedOptional)) {
  die('meta optionalDependencies do not match platforms.json');
}

const binaries = new Map();
for (const binding of binaryArgs) {
  const separator = binding.indexOf('=');
  if (separator < 1) {
    die(`invalid --binary binding: ${binding}`);
  }
  const id = binding.slice(0, separator);
  const source = binding.slice(separator + 1);
  if (binaries.has(id) || !isAbsolute(source)) {
    die(`duplicate platform or non-absolute binary: ${binding}`);
  }
  binaries.set(id, source);
}

const inputs = [];
for (const [id, source] of binaries) {
  const platform = matrix.platforms.find((candidate) => candidate.id === id);
  if (!platform) {
    die(`unknown platform id: ${id}`);
  }
  const sourceReal = realpathSync(source);
  if (lstatSync(source).isSymbolicLink() || !statSync(sourceReal).isFile()) {
    die(`platform binary must be a non-symlink regular file: ${source}`);
  }
  inputs.push({ platform, sourceReal });
}

mkdirSync(output, { recursive: false });
const metaOutput = resolve(output, 'meta');
mkdirSync(resolve(metaOutput, 'bin'), { recursive: true });
for (const relative of [
  'package.json',
  'platforms.json',
  'README.md',
  'bin/csa.js',
]) {
  copyText(resolve(metaSource, relative), resolve(metaOutput, relative));
}
for (const asset of ['LICENSE', 'THIRD_PARTY_NOTICES.md']) {
  copyText(resolve(repository, asset), resolve(metaOutput, asset));
}

const staged = [];
for (const { platform, sourceReal } of inputs) {
  const { id } = platform;

  const packageRoot = resolve(output, 'platforms', id);
  const destination = resolve(packageRoot, platform.binary);
  const relativeDestination = relative(packageRoot, destination);
  if (relativeDestination === '..' || relativeDestination.startsWith(`..${sep}`)) {
    die(`platform binary escapes package root: ${id}`);
  }
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(sourceReal, destination);
  if (platform.os !== 'win32') {
    chmodSync(destination, 0o755);
  }

  const bytes = readFileSync(destination);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const manifest = {
    name: platform.package,
    version: metaManifest.version,
    description: `Rust manager binary for ${platform.target}`,
    license: metaManifest.license,
    os: [platform.os],
    cpu: [platform.arch],
    files: ['bin/', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.md'],
    csa: {
      schema: 1,
      target: platform.target,
      binary: platform.binary,
      sha256,
    },
  };
  if (platform.libc) {
    manifest.libc = [platform.libc];
  }
  writeFileSync(resolve(packageRoot, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  writeFileSync(
    resolve(packageRoot, 'README.md'),
    `# ${platform.package}\n\nPlatform binary for \`${platform.target}\`. Install \`@dslzl/csa\`; do not invoke this package directly.\n`,
  );
  for (const asset of ['LICENSE', 'THIRD_PARTY_NOTICES.md']) {
    copyText(resolve(repository, asset), resolve(packageRoot, asset));
  }
  staged.push({
    id,
    package: platform.package,
    target: platform.target,
    binary: destination,
    size: bytes.length,
    sha256,
  });
}

writeFileSync(
  resolve(output, 'stage-results.json'),
  `${JSON.stringify({ schema: 1, version: metaManifest.version, meta: metaOutput, platforms: staged }, null, 2)}\n`,
);
process.stdout.write(`${JSON.stringify({ output, platforms: staged }, null, 2)}\n`);
