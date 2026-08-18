#!/usr/bin/env node

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const launcher = path.resolve(repository, 'npm', 'meta', 'bin', 'csa.js');
const meta = JSON.parse(readFileSync(path.resolve(repository, 'npm', 'meta', 'package.json'), 'utf8'));
const matrix = JSON.parse(readFileSync(path.resolve(repository, 'npm', 'meta', 'platforms.json'), 'utf8'));
const selected = matrix.platforms.find(
  (platform) => platform.os === process.platform && platform.arch === process.arch,
);
assert.ok(selected, `test host ${process.platform}-${process.arch} is unsupported`);

async function testProcessGroupSignal(env, cwd) {
  if (process.platform === 'win32') {
    return 'not_verified_on_windows';
  }
  const child = spawn(
    process.execPath,
    [launcher, '-e', 'process.stdout.write("ready\\n");setInterval(() => {}, 1000)'],
    { cwd, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let timer;
  try {
    await Promise.race([
      new Promise((resolveReady, rejectReady) => {
        child.stdout.once('data', (data) => {
          if (data.toString().includes('ready')) resolveReady();
          else rejectReady(new Error(`unexpected signal probe output: ${data}`));
        });
        child.once('exit', (code, signal) =>
          rejectReady(new Error(`signal probe exited early: code=${code} signal=${signal}`)),
        );
      }),
      new Promise((_, rejectTimeout) => {
        timer = setTimeout(() => rejectTimeout(new Error('signal probe startup timed out')), 5000);
      }),
    ]);
    clearTimeout(timer);
    const closed = new Promise((resolveClose) =>
      child.once('close', (code, signal) => resolveClose({ code, signal })),
    );
    process.kill(-child.pid, 'SIGTERM');
    const outcome = await Promise.race([
      closed,
      new Promise((_, rejectTimeout) => {
        timer = setTimeout(() => rejectTimeout(new Error('signal probe shutdown timed out')), 5000);
      }),
    ]);
    assert.equal(outcome.code, null);
    assert.equal(outcome.signal, 'SIGTERM');
    return 'pass';
  } finally {
    clearTimeout(timer);
    if (child.exitCode === null && child.signalCode === null) {
      process.kill(-child.pid, 'SIGKILL');
    }
  }
}

const temporary = mkdtempSync(path.join(os.tmpdir(), 'csa-launcher-'));
try {
  const packageRoot = path.join(temporary, 'node_modules', ...selected.package.split('/'));
  const binary = path.resolve(packageRoot, selected.binary);
  mkdirSync(path.dirname(binary), { recursive: true });
  copyFileSync(process.execPath, binary);
  if (process.platform !== 'win32') {
    chmodSync(binary, 0o755);
  }
  const sha256 = createHash('sha256').update(readFileSync(binary)).digest('hex');
  const platformManifest = {
    name: selected.package,
    version: meta.version,
    csa: {
      schema: 1,
      target: selected.target,
      binary: selected.binary,
      sha256,
    },
  };
  const manifestPath = path.join(packageRoot, 'package.json');
  writeFileSync(manifestPath, `${JSON.stringify(platformManifest, null, 2)}\n`);

  const env = {
    ...process.env,
    NODE_PATH: path.join(temporary, 'node_modules'),
    CSA_LAUNCHER_MARKER: 'marker value',
  };
  const probe = [
    '-e',
    'process.stdout.write(JSON.stringify({args:process.argv.slice(1),cwd:process.cwd(),marker:process.env.CSA_LAUNCHER_MARKER}));process.stderr.write("stderr-ok")',
    '--',
    'space value',
    '--literal=$()',
  ];
  const forwarded = spawnSync(process.execPath, [launcher, ...probe], {
    cwd: temporary,
    env,
    encoding: 'utf8',
  });
  assert.equal(forwarded.status, 0, forwarded.stderr);
  assert.equal(forwarded.stderr, 'stderr-ok');
  assert.deepEqual(JSON.parse(forwarded.stdout), {
    args: ['space value', '--literal=$()'],
    cwd: temporary,
    marker: 'marker value',
  });

  const exit = spawnSync(process.execPath, [launcher, '-e', 'process.exit(37)'], {
    env,
    encoding: 'utf8',
  });
  assert.equal(exit.status, 37);
  const signal = await testProcessGroupSignal(env, temporary);

  platformManifest.csa.sha256 = '0'.repeat(64);
  writeFileSync(manifestPath, `${JSON.stringify(platformManifest, null, 2)}\n`);
  const drift = spawnSync(process.execPath, [launcher, '--version'], { env, encoding: 'utf8' });
  assert.equal(drift.status, 1);
  assert.match(drift.stderr, /checksum mismatch/);

  rmSync(packageRoot, { recursive: true, force: true });
  const missing = spawnSync(process.execPath, [launcher, '--version'], { env, encoding: 'utf8' });
  assert.equal(missing.status, 1);
  assert.match(missing.stderr, /required platform package .* is not installed/);

  process.stdout.write(
    `${JSON.stringify({ schema: 1, argv_env_cwd_stdio: 'pass', exit_code: 'pass', checksum_drift: 'pass', missing_platform: 'pass', signal }, null, 2)}\n`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
