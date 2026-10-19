const assert = require('node:assert/strict');
const test = require('node:test');
const { analyzeCommits } = require('./release-analyzer.cjs');

for (const [name, messages, forcePatch, expected] of [
  ['maintenance stays unreleased by default', ['test: synchronize shutdown fixture'], false, null],
  ['explicit patch releases maintenance', ['test: synchronize shutdown fixture'], true, 'patch'],
  ['explicit patch requires new commits', [], true, null],
  ['explicit patch preserves minor releases', ['feat: add clipboard support'], true, 'minor'],
  ['explicit patch preserves major releases', ['feat!: change the protocol\n\nBREAKING CHANGE: old clients are unsupported'], true, 'major'],
]) {
  test(name, async () => {
    const result = await analyzeCommits(
      { preset: 'conventionalcommits', presetConfig: {}, forcePatch },
      {
        cwd: process.cwd(),
        commits: messages.map(message => ({ message })),
        logger: { log() {} },
      },
    );
    assert.equal(result, expected);
  });
}
