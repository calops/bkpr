const { readFileSync, writeFileSync } = require('node:fs');
const { resolve } = require('node:path');

function assertVersion(version) {
  if (!/^0\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error('Automatic releases must remain in 0.x; leaving 0.x requires a deliberate policy update.');
  }
}

module.exports = {
  assertVersion,
  prepare: (_, { cwd = process.cwd(), nextRelease }) => {
    assertVersion(nextRelease.version);
    const manifestPath = resolve(cwd, 'Cargo.toml');
    const manifest = readFileSync(manifestPath, 'utf8');
    const previous = manifest.match(/\[workspace\.package\]\s*\nversion = "([^"]+)"/);
    if (!previous) throw new Error('Missing workspace package version');
    const oldVersion = previous[1];
    const members = manifest.match(/^members\s*=\s*\[([^\]]*)\]/m);
    if (!members) throw new Error('Missing explicit workspace members');
    const paths = [...members[1].matchAll(/"([^"]+)"/g)].map(match => match[1]);
    if (!paths.length) throw new Error('Empty workspace members');
    const packages = paths.map(path => {
      const source = readFileSync(resolve(cwd, path, 'Cargo.toml'), 'utf8');
      const name = source.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
      if (!name || !/^version\.workspace\s*=\s*true\s*$/m.test(source)) {
        throw new Error(`${path}: expected a named package inheriting the workspace version`);
      }
      return name;
    });
    const lockPath = resolve(cwd, 'Cargo.lock');
    let lock = readFileSync(lockPath, 'utf8');
    for (const name of packages) {
      const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const pattern = new RegExp(`(\\[\\[package\\]\\]\\nname = "${escapedName}"\\nversion = ")([^"]+)(")`, 'g');
      const matches = [...lock.matchAll(pattern)];
      if (matches.length !== 1 || matches[0][2] !== oldVersion) {
        throw new Error(`Cargo.lock: expected one workspace package ${name} at ${oldVersion}`);
      }
      lock = lock.replace(pattern, (_, prefix, _version, suffix) => `${prefix}${nextRelease.version}${suffix}`);
    }
    // Preflight every workspace crate before writing. Crate manifests inherit this
    // version, as does the native Nix package via builtins.fromTOML Cargo.toml.
    writeFileSync(manifestPath, manifest.replace(
      /(\[workspace\.package\]\s*\nversion = ")[^"]+(")/,
      (_, prefix, suffix) => `${prefix}${nextRelease.version}${suffix}`,
    ));
    writeFileSync(lockPath, lock);
  },
};
