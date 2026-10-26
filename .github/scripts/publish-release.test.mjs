import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { publishRelease } from './publish-release.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const cli = join(root, '.github/scripts/tag-release.mjs');

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'bkpr-release-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const cwd = join(directory, 'work');
  const remote = join(directory, 'remote.git');
  const bin = join(directory, 'bin');
  for (const path of [cwd, bin, join(directory, 'home')]) mkdirSync(path);
  const env = {
    PATH: `${bin}:${process.env.PATH}`,
    HOME: join(directory, 'home'),
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_ALLOW_PROTOCOL: 'file',
    GIT_AUTHOR_NAME: 'Release fixture',
    GIT_AUTHOR_EMAIL: 'release@example.invalid',
    GIT_COMMITTER_NAME: 'Release fixture',
    GIT_COMMITTER_EMAIL: 'release@example.invalid',
    RELEASE_TYPE: 'patch',
    FIXTURE_REMOTE: remote,
    DISPATCH_LOG: join(directory, 'dispatch.log'),
    DISPATCH_TAG: join(directory, 'dispatch-tag'),
  };
  const git = (...args) => execFileSync('git', args, { cwd, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '--bare', '--initial-branch=main', remote);
  git('init', '--initial-branch=main');
  git('remote', 'add', 'origin', remote);
  // Keep real release-note URL generation, but forbid every network transport.
  const repositoryUrl = 'https://release-fixture.invalid/fixture/bkpr';
  git('config', `url.file://${remote}.insteadOf`, repositoryUrl);
  writeFileSync(join(cwd, 'release.config.cjs'), `
const config = require(${JSON.stringify(join(root, 'release.config.cjs'))});
config.plugins[0][0] = ${JSON.stringify(join(root, '.github/scripts/release-analyzer.cjs'))};
config.repositoryUrl = ${JSON.stringify(repositoryUrl)};
module.exports = config;
`);
  writeFileSync(join(cwd, 'Cargo.toml'), '[workspace.package]\nversion = "0.1.0"\n');
  writeFileSync(join(cwd, 'Cargo.lock'), ['bkpr', 'bkpr-zellij'].map(name => `[[package]]\nname = "${name}"\nversion = "0.1.0"\n`).join('\n'));
  writeFileSync(join(cwd, 'flake.nix'), 'pname = "bkpr-native";\nversion = "0.1.0";\npname = "bkpr-zellij";\nversion = "0.1.0";\nrunCommand "bkpr-0.1.0"\n');
  writeFileSync(join(cwd, 'CHANGELOG.md'), '# Changelog\n\n## 0.1.0\n');
  git('add', '.');
  git('commit', '-m', 'feat: initial fixture');
  git('tag', 'v0.1.0');
  git('commit', '--allow-empty', '-m', 'test: synchronize fixture startup');
  git('push', '--tags', '--set-upstream', 'origin', 'main');
  const head = git('rev-parse', 'HEAD');
  writeFileSync(join(remote, 'hooks/update'), `#!/bin/sh
case "$1" in
  refs/notes/*) echo 'missing necessary objects (fixture notes rejection)' >&2; exit 1 ;;
  refs/tags/*) if [ "\${REJECT_TAG:-}" = 1 ]; then exit 1; fi ;;
esac
`, { mode: 0o755 });
  writeFileSync(join(bin, 'gh'), `#!/bin/sh
set -eu
git --git-dir="$FIXTURE_REMOTE" rev-parse "refs/tags/$5" > "$DISPATCH_TAG"
printf '%s\\n' "$@" > "$DISPATCH_LOG"
[ "\${FAIL_DISPATCH:-}" != 1 ]
`, { mode: 0o755 });
  const run = (...args) => spawnSync(process.execPath, [cli, ...args], { cwd, env, encoding: 'utf8' });
  const remoteRef = ref => git('--git-dir', remote, 'rev-parse', '--verify', ref);
  const noRemoteRef = ref => assert.throws(() => remoteRef(ref));
  return { cwd, remote, env, git, head, run, remoteRef, noRemoteRef };
}

function published(f) {
  const tag = f.remoteRef('refs/tags/v0.1.1');
  assert.notEqual(tag, f.head);
  assert.equal(f.remoteRef('refs/heads/main'), tag);
  assert.equal(readFileSync(f.env.DISPATCH_TAG, 'utf8').trim(), tag);
  assert.equal(readFileSync(f.env.DISPATCH_LOG, 'utf8'), 'workflow\nrun\ncache.yml\n--ref\nv0.1.1\n');
  assert.match(readFileSync(join(f.cwd, 'Cargo.toml'), 'utf8'), /version = "0.1.1"/);
  assert.equal((readFileSync(join(f.cwd, 'Cargo.lock'), 'utf8').match(/version = "0.1.1"/g) || []).length, 2);
  assert.equal((readFileSync(join(f.cwd, 'flake.nix'), 'utf8').match(/0\.1\.1/g) || []).length, 3);
  assert.match(readFileSync(join(f.cwd, 'CHANGELOG.md'), 'utf8'), /0\.1\.1/);
  assert.equal(f.git('show', '-s', '--format=%s', tag), 'chore(release): 0.1.1');
}

test('real planning and publication dispatch despite a remote that rejects channel notes', t => {
  const f = fixture(t);
  f.git('notes', '--ref=semantic-release-v0.1.0', 'add', '-m', '{"channels":[null]}', 'v0.1.0');
  assert.throws(() => f.git('push', 'origin', 'refs/notes/semantic-release-v0.1.0'), /missing necessary objects/);
  f.git('tag', 'unrelated-local-tag');
  const result = f.run();
  assert.equal(result.status, 0, result.stdout + result.stderr);
  published(f);
  f.noRemoteRef('refs/notes/semantic-release-v0.1.1');
  f.noRemoteRef('refs/tags/unrelated-local-tag');
});

test('dry run leaves versions, main, tags and dispatch untouched', t => {
  const f = fixture(t);
  const result = f.run('--dry-run');
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /0\.1\.1/);
  assert.equal(f.remoteRef('refs/heads/main'), f.head);
  assert.equal(f.git('rev-parse', 'HEAD'), f.head);
  assert.equal(f.git('status', '--porcelain'), '');
  f.noRemoteRef('refs/tags/v0.1.1');
  assert.throws(() => readFileSync(f.env.DISPATCH_LOG));
});

test('tag rejection atomically leaves remote main unchanged and never dispatches', t => {
  const f = fixture(t);
  f.env.REJECT_TAG = '1';
  const result = f.run();
  assert.notEqual(result.status, 0, result.stdout + result.stderr);
  assert.equal(f.remoteRef('refs/heads/main'), f.head);
  f.noRemoteRef('refs/tags/v0.1.1');
  assert.throws(() => readFileSync(f.env.DISPATCH_LOG));
});

test('a concurrently published immutable tag cannot be overwritten or advance main', async t => {
  const f = fixture(t);
  f.git('--git-dir', f.remote, 'update-ref', 'refs/tags/v0.1.1', f.head);
  await assert.rejects(publishRelease({ nextRelease: {
    version: '0.1.1', gitTag: 'v0.1.1', notes: '## 0.1.1\n',
  } }, f));
  assert.equal(f.remoteRef('refs/tags/v0.1.1'), f.head);
  assert.equal(f.remoteRef('refs/heads/main'), f.head);
  assert.throws(() => readFileSync(f.env.DISPATCH_LOG));
});

test('dispatch failure stays visible without moving the published tag', t => {
  const f = fixture(t);
  f.env.FAIL_DISPATCH = '1';
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Do not move the tag; run gh workflow run cache.yml --ref v0\.1\.1/);
  published(f);
});

test('breaking commits plan and publish the next 0.x minor release', t => {
  const f = fixture(t);
  f.git('commit', '--allow-empty', '-m', 'feat!: change the fixture protocol');
  f.git('push', 'origin', 'main');
  const result = f.run();
  assert.equal(result.status, 0, result.stdout + result.stderr);
  const tag = f.remoteRef('refs/tags/v0.2.0');
  assert.equal(f.remoteRef('refs/heads/main'), tag);
  assert.match(readFileSync(join(f.cwd, 'Cargo.toml'), 'utf8'), /version = "0\.2\.0"/);
  assert.equal(readFileSync(f.env.DISPATCH_LOG, 'utf8'), 'workflow\nrun\ncache.yml\n--ref\nv0.2.0\n');
  f.noRemoteRef('refs/tags/v1.0.0');
});

test('nonzero-major releases are rejected before any mutation or dispatch', async t => {
  for (const version of ['1.0.0', '1.1.0', '2.0.0']) {
    const f = fixture(t);
    await assert.rejects(
      publishRelease({ nextRelease: { version, gitTag: `v${version}`, notes: '' } }, f),
      /Automatic releases must remain in 0\.x/,
    );
    assert.equal(f.remoteRef('refs/heads/main'), f.head);
    assert.equal(f.git('rev-parse', 'HEAD'), f.head);
    assert.equal(f.git('status', '--porcelain'), '');
    f.noRemoteRef(`refs/tags/v${version}`);
    assert.throws(() => readFileSync(f.env.DISPATCH_LOG));
  }
});
