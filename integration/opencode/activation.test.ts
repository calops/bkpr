import { expect, mock, spyOn, test } from 'bun:test';

mock.module('@opencode/plugin', () => ({ Plugin: { define: (value: unknown) => value } }));
mock.module('@opencode/plugin/tui', () => ({ Plugin: { define: (value: unknown) => value } }));
mock.module('solid-js', () => ({
  createEffect() { throw new Error('inactive plugin must not install UI effects'); },
  createRoot() { throw new Error('inactive plugin must not mount UI'); },
}));

// The fixture must install the virtual native SDK boundaries before either consumer module loads.
const { default: server } = await import('./index.ts');
const { default: tui } = await import('./tui.ts');

test('auto-discovered OpenCode server and TUI plugins never touch native services outside a managed launch', async () => {
  const keys = Object.keys(process.env).filter(key => key.startsWith('BKPR_CAPTURE_') || key === 'BKPR_INTEGRATION_FILE');
  const previous = Object.fromEntries(keys.map(key => [key, process.env[key]]));
  for (const key of keys) delete process.env[key];
  let serviceReads = 0;
  const context = new Proxy({ options: {} }, {
    get(target, property) {
      if (property === 'options') return target.options;
      serviceReads += 1;
      throw new Error('inactive Bkpr must not access native service APIs');
    },
  });
  try {
    await server.setup(context as unknown as Parameters<typeof server.setup>[0]);
    await tui.setup(context as unknown as Parameters<typeof tui.setup>[0]);
    expect(serviceReads).toBe(0);
    process.env.BKPR_INTEGRATION_FILE = '/nonexistent/bkpr-context';
    await expect(tui.setup(context as unknown as Parameters<typeof tui.setup>[0])).rejects.toThrow('invalid private native capture setup');
    expect(serviceReads).toBe(0);
  } finally {
    delete process.env.BKPR_INTEGRATION_FILE;
    Object.assign(process.env, previous);
  }
});

test('an invalid present server grant leaves prompt admission closed before stopping the private service', async () => {
  const killed: unknown[][] = [];
  // The production fail-closed boundary terminates the private native service, never this test host.
  const kill = spyOn(process, 'kill').mockImplementation((...args) => { killed.push(args); return true; });
  let prompt: ((event: { sessionID: string }) => Promise<void>) | undefined;
  let disposed = 0;
  const context = {
    options: { captureFile: '/nonexistent/private-bkpr-capture' },
    rpc: { async register() { return { events: { async emit() {} }, async dispose() { disposed += 1; } }; } },
    session: { async hook(_name: string, callback: typeof prompt) { prompt = callback; return { async dispose() { disposed += 1; } }; } },
  };
  try {
    const cleanup = await server.setup(context as unknown as Parameters<typeof server.setup>[0]);
    expect(killed).toEqual([[process.pid, 'SIGTERM']]);
    await expect(prompt!({ sessionID: 'native-root' })).rejects.toThrow('barrier unavailable');
    await cleanup?.();
    expect(disposed).toBe(2);
  } finally { kill.mockRestore(); }
});
