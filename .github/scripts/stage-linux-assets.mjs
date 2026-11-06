import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const [source, stageArg, outputArg, version, revision, tag, epoch] = process.argv.slice(2);
assert(source && stageArg && outputArg && /^0\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)*$/.test(version));
assert(/^[0-9a-f]{40}$/.test(revision) && /^\d+$/.test(epoch));
const stage = resolve(stageArg);
const output = resolve(outputArg);
const rootName = `bkpr-${version}-linux-x86_64`;
const root = join(stage, rootName);
const debRoot = join(stage, 'deb-root');
const commandEnv = { ...process.env, LC_ALL: 'C' };
const run = (command, args) => execFileSync(command, args, { encoding: 'utf8', env: commandEnv }).trim();
const numericVersion = (value) => value.split('.').map(Number);
const atMost = (value, ceiling) => {
  const a = numericVersion(value), b = numericVersion(ceiling);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if ((a[i] ?? 0) !== (b[i] ?? 0)) return (a[i] ?? 0) < (b[i] ?? 0);
  }
  return true;
};
const requirements = {};
const allowed = new Set(['libc.so.6', 'libgcc_s.so.1', 'ld-linux-x86-64.so.2', 'libm.so.6']);
for (const program of ['bkpr', 'bkprd']) {
  const original = join(realpathSync(source), 'bin', program);
  assert(statSync(original).isFile() && readFileSync(original).subarray(0, 4).equals(Buffer.from([127, 69, 76, 70])), `${program}: expected a raw ELF, not a Nix wrapper`);
  const header = run('readelf', ['-h', original]);
  assert(/Class:\s+ELF64/.test(header) && /Machine:\s+Advanced Micro Devices X86-64/.test(header));
  const dynamic = run('readelf', ['-d', original]);
  const needed = [...dynamic.matchAll(/\(NEEDED\).*\[([^\]]+)\]/g)].map((match) => match[1]).sort();
  assert(needed.every((library) => allowed.has(library)), `${program}: new dependencies require a portability review`);
  const versions = [...new Set([...run('readelf', ['--version-info', original]).matchAll(/Name: (\S+)/g)].map((match) => match[1]))].sort();
  for (const symbol of versions) {
    const match = /^(GLIBC|GCC)_([0-9.]+)$/.exec(symbol);
    assert(match && atMost(match[2], match[1] === 'GLIBC' ? '2.39' : '4.2.0'), `${program}: unreviewed symbol requirement ${symbol}`);
  }
  mkdirSync(join(root, 'bin'), { recursive: true });
  const relocated = join(root, 'bin', program);
  cpSync(original, relocated, { dereference: true });
  run('chmod', ['u+w', relocated]);
  // Clearing the value first removes its bytes; deleting only the dynamic tag
  // leaves the old store path behind in .dynstr.
  run('patchelf', ['--set-rpath', '', relocated]);
  run('patchelf', ['--set-interpreter', '/lib64/ld-linux-x86-64.so.2', '--remove-rpath', relocated]);
  assert.equal(run('patchelf', ['--print-interpreter', relocated]), '/lib64/ld-linux-x86-64.so.2');
  assert.equal(run('patchelf', ['--print-rpath', relocated]), '');
  assert(!/\((?:RPATH|RUNPATH)\)/.test(run('readelf', ['-d', relocated])));
  assert(!readFileSync(relocated).includes(Buffer.from('/nix/store/')), `${program}: residual Nix store reference requires review`);
  requirements[program] = { needed, symbolVersions: versions };
}
cpSync(join(realpathSync(source), 'share'), join(root, 'share'), { recursive: true, dereference: true });
function checkData(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) checkData(path);
    else {
      assert(entry.isFile(), `Unexpected staged entry: ${path}`);
      assert(!readFileSync(path).includes(Buffer.from('/nix/store/')), `Nix store reference in ${path}`);
    }
  }
}
checkData(join(root, 'share'));
run('chmod', ['-R', 'u+w', join(root, 'share')]);
assert(statSync(join(root, 'share/licenses/bkpr/LICENSE')).isFile());
mkdirSync(join(root, 'share/doc/bkpr'), { recursive: true });
writeFileSync(join(root, 'share/doc/bkpr/portable-build.json'), `${JSON.stringify({
  sourceRevision: revision, sourceTag: tag || null, version,
  platform: 'linux-x86_64', minimumGlibc: '2.39', debianDependencies: ['libc6 (>= 2.39)', 'libgcc-s1'],
  optionalHostTools: ['git', 'gh', 'ssh'], relocation: 'patchelf; adjacent native ELF executables; no bundled libc or loader wrapper', requirements,
}, null, 2)}\n`);
mkdirSync(join(debRoot, 'usr'), { recursive: true });
cpSync(join(root, 'bin'), join(debRoot, 'usr/bin'), { recursive: true });
cpSync(join(root, 'share'), join(debRoot, 'usr/share'), { recursive: true });
mkdirSync(output, { recursive: true });
run('tar', ['--sort=name', `--mtime=@${epoch}`, '--owner=0', '--group=0', '--numeric-owner', '-czf', join(output, `${rootName}.tar.gz`), '-C', stage, rootName]);
console.log(JSON.stringify({ root, debRoot, archive: join(output, `${rootName}.tar.gz`) }));
