import semanticRelease from 'semantic-release';
import releaseConfig from '../release.config.cjs';
import { publishRelease } from './publish-release.mjs';
import versionPlugin from './version-plugin.cjs';

const dryRun = process.argv.includes('--dry-run');
// Use the public planning API only. Core publication pushes channel notes after
// the immutable tag, so a notes rejection can strand a tagged release unbuilt.
const result = await semanticRelease({ ...releaseConfig, dryRun: true });
if (result) versionPlugin.assertVersion(result.nextRelease.version);
if (result && !dryRun) {
  await publishRelease(result);
} else if (!result) {
  console.log('No releasable conventional commits since the last version tag.');
}
