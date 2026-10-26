const assert = require('node:assert/strict');
const test = require('node:test');
const { analyzeCommits } = require('./release-analyzer.cjs');

for (const [name, messages, forcePatch, expected, previousVersion] of [
  ['maintenance stays unreleased by default', ['test: synchronize shutdown fixture'], false, null],
  ['explicit patch releases maintenance', ['test: synchronize shutdown fixture'], true, 'patch'],
  ['explicit patch requires new commits', [], true, null],
  ['explicit patch preserves minor releases', ['feat: add clipboard support'], true, 'minor'],
  ['explicit patch preserves major releases', ['feat!: change the protocol\n\nBREAKING CHANGE: old clients are unsupported'], true, 'major'],
  ['breaking changes stay pre-1.0', ['feat!: change the protocol\n\nBREAKING CHANGE: old clients are unsupported'], false, 'minor', '0.2.2'],
  ['explicit patch keeps pre-1.0 breaking changes minor', ['feat!: change the protocol'], true, 'minor', '0.2.2'],
  ['stable breaking changes remain major', ['feat!: change the protocol'], false, 'major', '1.2.3'],
]) {
  test(name, async () => {
    const result = await analyzeCommits(
      { preset: 'conventionalcommits', presetConfig: {}, forcePatch },
      {
        cwd: process.cwd(),
        commits: messages.map(message => ({ message })),
        lastRelease: previousVersion ? { version: previousVersion } : {},
        logger: { log() {} },
      },
    );
    assert.equal(result, expected);
  });
}
