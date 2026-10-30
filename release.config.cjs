module.exports = {
  // Version planning only until native packaging and release acceptance exist.
  dryRun: true,
  branches: ['main'],
  tagFormat: 'v${version}',
  plugins: [
    ['./.github/scripts/release-analyzer.cjs', {
      preset: 'conventionalcommits',
      presetConfig: {},
      forcePatch: process.env.RELEASE_TYPE === 'patch',
    }],
    ['@semantic-release/release-notes-generator', { preset: 'conventionalcommits', presetConfig: {} }],
  ],
};
