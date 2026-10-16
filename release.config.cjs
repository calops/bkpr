module.exports = {
  branches: ['main'],
  tagFormat: 'v${version}',
  plugins: [
    ['@semantic-release/commit-analyzer', { preset: 'conventionalcommits', presetConfig: {} }],
    ['@semantic-release/release-notes-generator', { preset: 'conventionalcommits', presetConfig: {} }],
    '@semantic-release/changelog',
    ['./.github/scripts/version-plugin.cjs'],
    ['@semantic-release/git', {
      assets: ['CHANGELOG.md', 'Cargo.toml', 'Cargo.lock', 'flake.nix'],
      message: 'chore(release): ${nextRelease.version}',
    }],
  ],
};
