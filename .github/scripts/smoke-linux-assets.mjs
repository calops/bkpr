import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const [binArg, version, evidenceArg] = process.argv.slice(2);
assert(binArg && version && evidenceArg);
assert(!existsSync('/nix'), 'Smoke must execute with /nix absent, not merely removed from PATH');
const bin = realpathSync(binArg);
const evidence = resolve(evidenceArg);
mkdirSync(evidence, { recursive: true });
const fixture = mkdtempSync('/tmp/b-');
const home = join(fixture, 'h');
const readyPrompt = process.getuid() === 0 ? /#(?=[\x1b \r\n])/ : /\$(?=[\x1b \r\n])/;
for (const path of ['h', 'config', 'cache', 'state', 'run']) mkdirSync(join(fixture, path), { mode: 0o700 });
const env = {
  PATH: '/usr/bin:/bin', HOME: home, SHELL: '/bin/sh', TERM: 'xterm-256color', LANG: 'C.UTF-8',
  XDG_CONFIG_HOME: join(fixture, 'config'), XDG_CACHE_HOME: join(fixture, 'cache'),
  XDG_STATE_HOME: join(fixture, 'state'), XDG_RUNTIME_DIR: join(fixture, 'run'),
};
const bkpr = join(bin, 'bkpr');
const run = (program, args) => execFileSync(program, args, { env, encoding: 'utf8', timeout: 10000 }).trim();
const call = (...args) => run(bkpr, ['--session', 'release-smoke', ...args]);
const identity = (pid) => {
  try {
    // A container may also have root-owned installer/entrypoint processes.
    // Only this fixture user's processes can be candidates or cleanup targets.
    if (statSync(`/proc/${pid}`).uid !== process.getuid()) return null;
    const stat = readFileSync(`/proc/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ');
    if (fields[0] === 'Z') return null;
    return { pid: Number(pid), birth: fields[19], parent: Number(fields[1]), exe: readlinkSync(`/proc/${pid}/exe`) };
  } catch (error) {
    if (['ENOENT', 'ESRCH'].includes(error.code)) return null;
    throw error;
  }
};
const same = (process) => process && identity(process.pid)?.birth === process.birth;
async function until(description, action) {
  const deadline = Date.now() + 20000;
  while (true) {
    const result = action();
    if (result) return result;
    assert(Date.now() < deadline, `Timed out waiting for ${description}`);
    await delay(50);
  }
}
const owned = [];
let terminal;
let terminalExit;
let transcript = '';
const result = { version, nixVisible: false, binaryDirectory: bin, loaders: {}, processes: [] };
try {
  for (const program of ['bkpr', 'bkprd']) {
    const executable = join(bin, program);
    assert(run(executable, ['--version']).includes(version));
    const loader = run('/usr/bin/ldd', [executable]);
    assert(!loader.includes('/nix/') && !loader.includes('not found'), loader);
    result.loaders[program] = loader;
  }
  // Do not pass --bkprd, a command, or a shell override: exercise default TUI
  // startup and current_exe()-based adjacent companion discovery.
  const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;
  const command = `stty rows 30 cols 100; exec ${[bkpr, '--session', 'release-smoke', '--home', home, '--cwd', home].map(quote).join(' ')}`;
  terminal = spawn('/usr/bin/script', ['--quiet', '--return', '--command', command, '/dev/null'], { env, stdio: ['pipe', 'pipe', 'pipe'] });
  terminalExit = new Promise((resolveExit, reject) => {
    terminal.once('error', reject);
    terminal.once('exit', (code, signal) => resolveExit({ code, signal }));
  });
  terminal.stdout.on('data', (bytes) => { transcript += bytes.toString(); });
  terminal.stderr.on('data', (bytes) => { transcript += bytes.toString(); });
  const daemon = await until('adjacent native daemon', () => {
    for (const pid of readdirSync('/proc').filter((name) => /^\d+$/.test(name))) {
      const process = identity(pid);
      if (process?.exe === join(bin, 'bkprd')) return process;
    }
    return null;
  });
  owned.push(daemon);
  // A daemon PID exists before its pane/view is ready to accept TUI input.
  // Require the literal shell prompt followed by terminal output/whitespace;
  // '$p' in a capability query and a partial query are not prompt readiness.
  await until('default shell prompt rendered by the TUI', () => readyPrompt.test(transcript));
  const marker = join(home, 'pane-pid');
  const contextFile = join(home, 'pane-context.json');
  terminal.stdin.write(`'${bkpr}' context --json > '${contextFile}' && printf '%s\\n' "$$" > '${marker}'\n`);
  const pane = await until('default interactive shell marker', () => {
    if (!existsSync(marker)) return null;
    return identity(Number(readFileSync(marker, 'utf8').trim()));
  });
  assert(pane && pane.exe === realpathSync('/bin/sh'), 'Default pane must use the host shell');
  owned.push(pane);
  let ancestor = pane;
  const ancestors = [];
  while (ancestor && ancestor.pid !== daemon.pid && ancestors.length < 32) {
    ancestors.push(ancestor);
    ancestor = identity(ancestor.parent);
  }
  assert.equal(ancestor?.pid, daemon.pid, 'Pane must descend from this exact adjacent daemon');
  const inspection = JSON.parse(call('api', 'call', 'session.inspect', '--arguments', '{}'));
  assert.equal(inspection.registry.panes.filter((entry) => entry.status === 'running').length, 1);
  assert.equal(inspection.registry.workspaces.length, 1);
  const context = JSON.parse(readFileSync(contextFile, 'utf8'));
  assert.equal(context.pane_id, inspection.registry.panes.find((entry) => entry.status === 'running').id, 'Native in-pane helper must resolve its authenticated process/PTY membership');
  result.paneContext = context;
  for (const process of [daemon, pane]) {
    assert(same(process));
    assert(!readFileSync(`/proc/${process.pid}/maps`, 'utf8').includes('/nix/'));
  }
  result.processes = { daemon, pane, ancestors };
  result.inspection = inspection;
  call('stop');
  await until('daemon and pane retirement', () => owned.every((process) => !same(process)));
  const exit = await Promise.race([terminalExit, delay(10000, undefined, { ref: false }).then(() => { throw new Error('TUI did not retire after stop'); })]);
  assert.deepEqual(exit, { code: 0, signal: null });
  result.terminalExit = exit;
  result.passed = true;
} finally {
  // The container owns the remaining process namespace as a final safety net;
  // direct cleanup still targets only exact births observed by this fixture.
  try {
    for (const process of owned) if (same(process)) {
      try { globalThis.process.kill(process.pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
    await until('owned cleanup', () => owned.every((process) => !same(process)));
  } finally {
    if (terminal?.exitCode === null) terminal.kill('SIGTERM');
    result.survivingOwnedBirths = owned.filter(same);
    writeFileSync(join(evidence, 'acceptance.json'), `${JSON.stringify(result, null, 2)}\n`);
    writeFileSync(join(evidence, 'terminal.log'), transcript);
    rmSync(fixture, { recursive: true, force: true });
  }
}
assert.equal(result.survivingOwnedBirths.length, 0, 'Fixture leaked owned processes');
console.log(JSON.stringify(result));
