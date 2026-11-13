import { expect, test } from 'bun:test';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { ExtensionAPI } from '@earendil-works/pi-coding-agent';
import bkpr from './index.ts';
import { optionalNativeCallbacks } from '../shared/native.ts';

function activationEnvironment(): () => void {
  const keys = Object.keys(process.env).filter(key => key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_') || key === 'BKPR_INTEGRATION_FILE');
  const previous = Object.fromEntries(keys.map(key => [key, process.env[key]]));
  for (const key of keys) delete process.env[key];
  return () => {
    for (const key of Object.keys(process.env)) {
      if (key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_') || key === 'BKPR_INTEGRATION_FILE') delete process.env[key];
    }
    Object.assign(process.env, previous);
  };
}

test('native Pi discovery is inert outside Bkpr and invalid present context cannot register MCP or callbacks', async () => {
  const restore = activationEnvironment();
  const handlers: string[] = [];
  const servers: unknown[] = [];
  const api = { on(name: string) { handlers.push(name); }, registerMcpServer(...args: unknown[]) { servers.push(args); } } as unknown as ExtensionAPI;
  try {
    await bkpr(api);
    expect({ handlers, servers }).toEqual({ handlers: [], servers: [] });
    process.env.BKPR_INTEGRATION_FILE = '/nonexistent/private-bkpr-context';
    await expect(bkpr(api)).rejects.toThrow('invalid private native capture setup');
    expect({ handlers, servers }).toEqual({ handlers: [], servers: [] });
  } finally { restore(); }
});

test('the launch hint registers one native Pi MCP with private credentials and explicit native options take precedence', async () => {
  const restore = activationEnvironment();
  const directory = await mkdtemp(join(tmpdir(), 'bkpr-installed-pi-'));
  const captureFile = join(directory, 'capture # %.json');
  const servers: Array<[string, { command: string; args: string[] }]> = [];
  const handlers: string[] = [];
  const api = { on(name: string) { handlers.push(name); }, registerMcpServer(name: string, config: { command: string; args: string[] }) { servers.push([name, config]); } } as unknown as ExtensionAPI;
  try {
    await writeFile(captureFile, JSON.stringify({ grant: { kind: 'pi', launch_id: 'launch', pane_id: 'pane', token: 'private-capability' }, program: '/private/bkpr', socket: '/private/socket' }), { mode: 0o600 });
    process.env.BKPR_INTEGRATION_FILE = captureFile;
    await bkpr(api);
    expect(servers).toEqual([['bkpr', { command: '/private/bkpr', args: ['capture', 'mcp', '--harness', 'pi', '--capture-file', captureFile] }]]);
    expect(handlers).toEqual(['session_start', 'before_agent_start', 'session_shutdown']);
    expect(JSON.stringify(servers)).not.toContain('private-capability');
    // A present explicit native option is never replaced by a valid inherited hint.
    await expect(optionalNativeCallbacks('pi', '/missing/explicit-native-context')).rejects.toThrow('invalid private native capture setup');
    process.env.BKPR_INTEGRATION_FILE = '/missing/inherited-native-context';
    expect((await optionalNativeCallbacks('pi', captureFile))?.captureFile).toBe(captureFile);
    await expect(optionalNativeCallbacks('opencode', captureFile)).rejects.toThrow('invalid private native capture setup');
    await chmod(captureFile, 0o644);
    await expect(optionalNativeCallbacks('pi', captureFile)).rejects.toThrow('invalid private native capture setup');
    expect(servers).toHaveLength(1);
  } finally {
    restore();
    await rm(directory, { recursive: true, force: true });
  }
});

test('incomplete legacy activation proof cannot expose native Pi MCP', async () => {
  const restore = activationEnvironment();
  let registrations = 0;
  const api = {
    on() { registrations += 1; },
    registerMcpServer() { registrations += 1; },
  } as unknown as ExtensionAPI;
  try {
    process.env.BKPR_CAPTURE_HARNESS = 'pi';
    process.env.BKPR_CAPTURE_PROGRAM = '/private/bkpr';
    await expect(bkpr(api)).rejects.toThrow('invalid private native capture setup');
    expect(registrations).toBe(0);
  } finally { restore(); }
});
