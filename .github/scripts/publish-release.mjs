import { execFileSync } from 'node:child_process';
import { prepare as prepareChangelog } from '@semantic-release/changelog';
import versionPlugin from './version-plugin.cjs';

const assets = ['CHANGELOG.md', 'Cargo.toml', 'Cargo.lock', 'flake.nix'];

export async function publishRelease({ nextRelease }, { cwd = process.cwd(), env = process.env } = {}) {
  if (!nextRelease.version.startsWith('0.')) {
    throw new Error('Automatic releases must remain in 0.x; leaving 0.x requires a deliberate policy update.');
  }
  const options = {
    cwd,
    env: {
      ...env,
      GIT_AUTHOR_NAME: env.GIT_AUTHOR_NAME || 'github-actions[bot]',
      GIT_AUTHOR_EMAIL: env.GIT_AUTHOR_EMAIL || '41898282+github-actions[bot]@users.noreply.github.com',
      GIT_COMMITTER_NAME: env.GIT_COMMITTER_NAME || 'github-actions[bot]',
      GIT_COMMITTER_EMAIL: env.GIT_COMMITTER_EMAIL || '41898282+github-actions[bot]@users.noreply.github.com',
    },
    stdio: 'inherit',
  };
  const git = (...args) => execFileSync('git', args, options);
  const { gitTag, version } = nextRelease;

  await prepareChangelog({}, { cwd, nextRelease, logger: console });
  versionPlugin.prepare({}, { cwd, nextRelease });
  git('add', '--', ...assets);
  git('commit', '--only', '-m', `chore(release): ${version}`, '--', ...assets);
  git('tag', gitTag);
  // A single-main release needs no channel notes. Publish only these two refs,
  // atomically and without force: rejection cannot advance main without its tag.
  git('push', '--atomic', 'origin', 'HEAD:refs/heads/main', `refs/tags/${gitTag}:refs/tags/${gitTag}`);

  // GITHUB_TOKEN pushes do not trigger push workflows; dispatch explicitly only
  // after both refs exist remotely. Dispatch errors remain failures, not success.
  try {
    execFileSync('gh', ['workflow', 'run', 'cache.yml', '--ref', gitTag], options);
  } catch (cause) {
    throw new Error(
      `${gitTag} is published, but its cache build was not dispatched. Do not move the tag; run gh workflow run cache.yml --ref ${gitTag}.`,
      { cause },
    );
  }
}
