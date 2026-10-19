import semanticRelease from 'semantic-release';
import { execFileSync } from 'node:child_process';

const dryRun = process.argv.includes('--dry-run');
const result = await semanticRelease(dryRun ? { dryRun: true } : {});
if (result && !dryRun) {
  const tag = result.nextRelease.gitTag;
  // GITHUB_TOKEN tag pushes do not trigger push workflows; workflow_dispatch does.
  execFileSync('gh', ['workflow', 'run', 'cache.yml', '--ref', tag], { stdio: 'inherit' });
} else if (!result) {
  console.log('No releasable conventional commits since the last version tag.');
}
