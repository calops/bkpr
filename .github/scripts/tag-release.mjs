import semanticRelease from 'semantic-release';
import { publishRelease } from './publish-release.mjs';

const dryRun = process.argv.includes('--dry-run');
// Use the public planning API only. Core publication pushes channel notes after
// the immutable tag, so a notes rejection can strand a tagged release unbuilt.
const result = await semanticRelease({ dryRun: true });
if (result && !dryRun) {
  await publishRelease(result);
} else if (!result) {
  console.log('No releasable conventional commits since the last version tag.');
}
