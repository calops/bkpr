import semanticRelease from 'semantic-release';
import { execFileSync } from 'node:child_process';

const result = await semanticRelease();
if (result) {
  const tag = result.nextRelease.gitTag;
  // GITHUB_TOKEN tag pushes do not trigger push workflows; workflow_dispatch does.
  execFileSync('gh', ['workflow', 'run', 'cache.yml', '--ref', tag], { stdio: 'inherit' });
} else {
  console.log('No releasable conventional commits since the last version tag.');
}
