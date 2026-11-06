import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const scripts = dirname(fileURLToPath(import.meta.url));
function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'bkpr-tags-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const remote = join(cwd, 'remote.git');
  const env = {
    ...process.env,
    GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_ALLOW_PROTOCOL: 'file',
    GIT_AUTHOR_NAME: 'Tag fixture', GIT_AUTHOR_EMAIL: 'tag@example.invalid',
    GIT_COMMITTER_NAME: 'Tag fixture', GIT_COMMITTER_EMAIL: 'tag@example.invalid',
  };
  const git = (...args) => execFileSync('git', args, { cwd, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '--bare', '--initial-branch=main', remote);
  git('init', '--initial-branch=main');
  git('remote', 'add', 'origin', remote);
  git('commit', '--allow-empty', '-m', 'initial');
  const first = git('rev-parse', 'HEAD');
  git('push', 'origin', 'HEAD:refs/heads/main', 'HEAD:refs/tags/unstable');
  const policy = (mode, tag) => spawnSync('bash', [resolve(scripts, 'tag-policy.sh'), mode, tag], { cwd, env, encoding: 'utf8' });
  const unstable = () => git('--git-dir', remote, 'rev-parse', 'refs/tags/unstable');
  return { cwd, git, first, policy, unstable };
}

test('nonzero major cannot build, promote unstable, or publish a release', t => {
  const f = fixture(t);
  for (const mode of ['build', 'promote']) {
    const result = f.policy(mode, 'v1.0.0');
    assert.notEqual(result.status, 0);
  }
  assert.equal(f.unstable(), f.first);
  const classified = spawnSync('bash', [join(scripts, 'classify-tag.sh'), 'v1.0.0'], { encoding: 'utf8' });
  assert.notEqual(classified.status, 0);
  assert.equal(classified.stdout, '');
});

test('unstable advances only forward and never moves immutable version tags', t => {
  const f = fixture(t);
  f.git('tag', 'v0.4.2');
  f.git('push', 'origin', 'refs/tags/v0.4.2');
  f.git('commit', '--allow-empty', '-m', 'feat!: native cutover');
  const next = f.git('rev-parse', 'HEAD');
  f.git('tag', 'v0.5.0');
  f.git('push', 'origin', 'refs/tags/v0.5.0');
  let result = f.policy('promote', 'v0.5.0');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.unstable(), next);
  f.git('checkout', '--detach', f.first);
  result = f.policy('promote', 'v0.4.2');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.unstable(), next);
  assert.equal(f.git('--git-dir', join(f.cwd, 'remote.git'), 'rev-parse', 'refs/tags/v0.4.2'), f.first);
});

test('an immutable tag owns a duplicate unstable build', t => {
  const f = fixture(t);
  f.git('tag', 'v0.4.2');
  const result = f.policy('build', 'unstable');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, 'build=false\n');
  assert.equal(f.policy('build', 'v0.4.2').stdout, 'build=true\n');
});

for (const [tag, prerelease] of [['v0.5.0', false], ['v0.5.0-rc.1', true]]) {
  test(`classifies ${tag} without dropping release channels`, () => {
    const result = spawnSync('bash', [join(scripts, 'classify-tag.sh'), tag], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, `release=true\nprerelease=${prerelease}\n`);
  });
}
