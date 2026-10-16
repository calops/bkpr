import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

const tag = process.argv[2];
if (!tag?.startsWith('v')) throw new Error('Expected a version tag');
const version = tag.slice(1);

let changelog = '';
try {
  changelog = readFileSync('CHANGELOG.md', 'utf8');
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
}
const heading = /^#{1,2} \[?v?(\d+\.\d+\.\d+)\b.*$/gm;
const sections = [...changelog.matchAll(heading)];
const current = sections.findIndex((section) => section[1] === version);
if (current >= 0) {
  const start = sections[current].index;
  const end = sections[current + 1]?.index ?? changelog.length;
  process.stdout.write(`${changelog.slice(start, end).trim()}\n`);
} else {
  // Tags made outside the conventional release workflow have no changelog entry.
  const notes = execFileSync('gh', [
    'api', `repos/${process.env.GITHUB_REPOSITORY}/releases/generate-notes`,
    '-f', `tag_name=${tag}`, '--jq', '.body',
  ]);
  process.stdout.write(notes);
}
