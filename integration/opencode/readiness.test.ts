import { expect, test } from 'bun:test';
import { NativePluginReadiness } from './rpc.ts';

type Event = { type: string; location?: { directory: string } };
type Inventory = { data: Array<{ id: string; features: { server: boolean; rpc: boolean }; state: { status: string; error?: string } }> };
const active: Inventory = { data: [{ id: 'bkpr-native', features: { server: true, rpc: true }, state: { status: 'active' } }] };

function fixture(list: (options: { location: { directory: string } }) => Promise<Inventory>) {
  const queue: Array<{ event?: Event; error?: Error; consumed: ReturnType<typeof Promise.withResolvers<void>> }> = [];
  let changed = Promise.withResolvers<void>();
  return {
    client: {
      plugin: { list },
      event: {
        async *subscribe({ signal }: { signal: AbortSignal }) {
          const abort = (): void => changed.resolve();
          signal.addEventListener('abort', abort, { once: true });
          try {
            while (!signal.aborted) {
              const item = queue.shift();
              if (!item) {
                await changed.promise;
                changed = Promise.withResolvers<void>();
                continue;
              }
              if (item.error) { item.consumed.resolve(); throw item.error; }
              yield item.event!;
              item.consumed.resolve();
            }
          } finally {
            signal.removeEventListener('abort', abort);
          }
        },
      },
    },
    async emit(event: Event) {
      const consumed = Promise.withResolvers<void>();
      queue.push({ event, consumed });
      changed.resolve();
      await consumed.promise;
    },
    async fail(error: Error) {
      const consumed = Promise.withResolvers<void>();
      queue.push({ error, consumed });
      changed.resolve();
      await consumed.promise;
    },
  };
}

test('cold registration waits for a live subscription and only the matching location update', async () => {
  const initialA = Promise.withResolvers<void>();
  const initialB = Promise.withResolvers<void>();
  const inventories = new Map<string, Inventory>();
  const queried: string[] = [];
  const native = fixture(async ({ location }) => {
    queried.push(location.directory);
    if (location.directory === '/a') initialA.resolve(); else initialB.resolve();
    return inventories.get(location.directory) ?? { data: [] };
  });
  const readiness = new NativePluginReadiness(native.client);
  let aReady = false;
  const a = readiness.wait({ directory: '/a' }).then(() => { aReady = true; });
  const b = readiness.wait({ directory: '/b' });
  try {
    expect(queried).toEqual([]);
    await native.emit({ type: 'server.connected' });
    await Promise.all([initialA.promise, initialB.promise]);
    inventories.set('/b', active);
    await native.emit({ type: 'plugin.updated', location: { directory: '/b' } });
    await b;
    expect(aReady).toBe(false);
    expect(queried.filter(directory => directory === '/a')).toEqual(['/a']);
    inventories.set('/a', active);
    await native.emit({ type: 'plugin.updated', location: { directory: '/a' } });
    await a;
    expect(aReady).toBe(true);
  } finally {
    readiness.close();
    await Promise.allSettled([a, b]);
  }
});

test('a lifecycle change during inventory lookup cannot admit a stale active snapshot', async () => {
  const queried = Promise.withResolvers<void>();
  const first = Promise.withResolvers<Inventory>();
  let calls = 0;
  const native = fixture(async () => {
    calls += 1;
    if (calls === 1) { queried.resolve(); return first.promise; }
    return { data: [{ id: 'bkpr-native', features: { server: true, rpc: true }, state: { status: 'failed', error: 'setup rejected' } }] };
  });
  const readiness = new NativePluginReadiness(native.client);
  const pending = readiness.wait({ directory: '/a' });
  const outcome = pending.catch(error => error);
  try {
    await native.emit({ type: 'server.connected' });
    await queried.promise;
    await native.emit({ type: 'plugin.updated', location: { directory: '/a' } });
    first.resolve(active);
    expect(await outcome).toHaveProperty('message', expect.stringContaining('setup rejected'));
  } finally {
    readiness.close();
  }
});

test('stream loss cancels pending lookups and invalidates previously ready locations', async () => {
  const queried = Promise.withResolvers<void>();
  const late = Promise.withResolvers<Inventory>();
  const native = fixture(async ({ location }) => {
    if (location.directory === '/a') return active;
    queried.resolve();
    return late.promise;
  });
  const readiness = new NativePluginReadiness(native.client);
  try {
    const a = readiness.wait({ directory: '/a' });
    await native.emit({ type: 'server.connected' });
    await a;
    const pending = readiness.wait({ directory: '/b' });
    const transport = new Error('native transport lost');
    const outcome = pending.catch(error => error);
    await queried.promise;
    await native.fail(transport);
    expect(await outcome).toBe(transport);
    late.resolve(active);
    expect(await readiness.wait({ directory: '/a' }).catch(error => error)).toBe(transport);
    expect(await readiness.wait({ directory: '/b' }).catch(error => error)).toBe(transport);
  } finally {
    readiness.close();
  }
});

test('disposal releases readiness even when the native lookup ignores cancellation', async () => {
  const queried = Promise.withResolvers<void>();
  const late = Promise.withResolvers<Inventory>();
  const native = fixture(async () => { queried.resolve(); return late.promise; });
  const readiness = new NativePluginReadiness(native.client);
  const pending = readiness.wait({ directory: '/a' });
  const outcome = pending.catch(error => error);
  await native.emit({ type: 'server.connected' });
  await queried.promise;
  readiness.close();
  expect(await outcome).toBeInstanceOf(Error);
  late.resolve(active);
  await expect(readiness.wait({ directory: '/a' })).rejects.toBeInstanceOf(Error);
});
