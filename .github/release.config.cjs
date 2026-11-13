module.exports = {
  branches: ['main'],
  tagFormat: 'v${version}',
  plugins: [
    [require.resolve('./scripts/release-analyzer.cjs'), {
      preset: 'conventionalcommits',
      presetConfig: {},
      forcePatch: process.env.RELEASE_TYPE === 'patch',
    }],
    [require.resolve('@semantic-release/release-notes-generator'), { preset: 'conventionalcommits', presetConfig: {} }],
  ],
};
