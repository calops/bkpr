module.exports = {
  branches: ['main'],
  tagFormat: 'v${version}',
  plugins: [
    ['./.github/scripts/release-analyzer.cjs', {
      preset: 'conventionalcommits',
      presetConfig: {},
      forcePatch: process.env.RELEASE_TYPE === 'patch',
    }],
    ['@semantic-release/release-notes-generator', { preset: 'conventionalcommits', presetConfig: {} }],
    '@semantic-release/changelog',
    ['./.github/scripts/version-plugin.cjs'],
    ['@semantic-release/git', {
      assets: ['CHANGELOG.md', 'Cargo.toml', 'Cargo.lock', 'flake.nix'],
      message: 'chore(release): ${nextRelease.version}',
    }],
  ],
};
