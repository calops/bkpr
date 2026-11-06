const assert = require('node:assert/strict');
const { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const test = require('node:test');
const { prepare } = require('./version-plugin.cjs');

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'bkpr-version-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const manifest = '[workspace]\nmembers = ["crates/bkpr", "crates/bkpr-server"]\n\n[workspace.package]\nversion = "0.4.2"\n';
  writeFileSync(join(cwd, 'Cargo.toml'), manifest);
  for (const name of ['bkpr', 'bkpr-server']) {
    mkdirSync(join(cwd, 'crates', name), { recursive: true });
    writeFileSync(join(cwd, 'crates', name, 'Cargo.toml'), `[package]\nname = "${name}"\nversion.workspace = true\n`);
  }
  const lock = ['bkpr', 'bkpr-server', 'unrelated'].map(name => `[[package]]\nname = "${name}"\nversion = "0.4.2"\n`).join('\n');
  writeFileSync(join(cwd, 'Cargo.lock'), lock);
  return { cwd, manifest, lock };
}

test('native workspace and daemon versions advance without changing dependencies or inherited manifests', t => {
  const { cwd } = fixture(t);
  prepare({}, { cwd, nextRelease: { version: '0.5.0' } });
  assert.match(readFileSync(join(cwd, 'Cargo.toml'), 'utf8'), /version = "0\.5\.0"/);
  const lock = readFileSync(join(cwd, 'Cargo.lock'), 'utf8');
  for (const name of ['bkpr', 'bkpr-server']) {
    assert.ok(lock.includes(`name = "${name}"\nversion = "0.5.0"`));
    assert.match(readFileSync(join(cwd, 'crates', name, 'Cargo.toml'), 'utf8'), /version\.workspace = true/);
  }
  assert.ok(lock.includes('name = "unrelated"\nversion = "0.4.2"'));
});

test('missing workspace lock entry fails before either version file changes', t => {
  const { cwd, manifest } = fixture(t);
  const lock = '[[package]]\nname = "bkpr"\nversion = "0.4.2"\n';
  writeFileSync(join(cwd, 'Cargo.lock'), lock);
  assert.throws(() => prepare({}, { cwd, nextRelease: { version: '0.5.0' } }), /expected one workspace package bkpr-server/);
  assert.equal(readFileSync(join(cwd, 'Cargo.toml'), 'utf8'), manifest);
  assert.equal(readFileSync(join(cwd, 'Cargo.lock'), 'utf8'), lock);
});

for (const version of ['1.0.0', '0.5', '0.05.0', '0.5.0junk']) {
  test(`invalid automatic version ${version} fails before reading or writing files`, () => {
    assert.throws(() => prepare({}, { cwd: '/nonexistent-bkpr-version-fixture', nextRelease: { version } }), /must remain in 0\.x/);
  });
}
