const { readFileSync, writeFileSync } = require('node:fs');
const { resolve } = require('node:path');

function replaceVersion(file, pattern, oldVersion, newVersion) {
  const source = readFileSync(file, 'utf8');
  const matches = [...source.matchAll(pattern)];
  if (matches.length !== 1 || matches[0][2] !== oldVersion) {
    throw new Error(`${file}: expected one package version ${oldVersion}`);
  }
  const updated = source.replace(pattern, (_, prefix, _version, suffix) =>
    `${prefix}${newVersion}${suffix}`);
  writeFileSync(file, updated);
}

module.exports = {
  prepare: (_, { cwd = process.cwd(), nextRelease }) => {
    const manifest = readFileSync(resolve(cwd, 'Cargo.toml'), 'utf8');
    const previous = manifest.match(/\[workspace\.package\]\s*\nversion = "([^"]+)"/);
    if (!previous) throw new Error('Missing workspace package version');
    const oldVersion = previous[1];
    const version = nextRelease.version;

    replaceVersion(resolve(cwd, 'Cargo.toml'), /(\[workspace\.package\]\s*\nversion = ")([^"]+)(")/g, oldVersion, version);
    for (const name of ['bkpr', 'bkpr-zellij']) {
      replaceVersion(resolve(cwd, 'Cargo.lock'), new RegExp(`(\\[\\[package\\]\\]\\nname = "${name}"\\nversion = ")([^"]+)(")`, 'g'), oldVersion, version);
    }
    for (const name of ['bkpr-native', 'bkpr-zellij']) {
      replaceVersion(resolve(cwd, 'flake.nix'), new RegExp(`(pname = "${name}";\\n\\s*version = ")([^"]+)(")`, 'g'), oldVersion, version);
    }
    replaceVersion(resolve(cwd, 'flake.nix'), /(runCommand "bkpr-)([^"]+)(")/g, oldVersion, version);
  },
};
