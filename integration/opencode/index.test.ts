import { expect, mock, spyOn, test } from 'bun:test';
import { EventEmitter } from 'node:events';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { setImmediate as nextTurn } from 'node:timers/promises';
import type { Json } from '../shared/native.ts';

test('native prompt barriers await the selected TUI and attest only actual native descendants', async () => {
  const calls: Json[] = [];
  const emitted: Json[] = [];
  let failures = 0;
  let catalogs = 0;
  let registrations = 0;
  const hooks = new Map<string, (event: Json) => Promise<void>>();
  let handlers: Record<string, (input: Json) => Promise<unknown>>;
  let created = 123;
  let missing = false;
  let held: { promise: Promise<void>; resolve(): void } | undefined;
  let selected: string | null = 'root-A';
  let generation = 7;
  let autoTools = true;
  let autoWakes = true;
  let answeringOwner = 'tui-owner';
  const binding = () => selected === null ? null : { id: selected, source: 'opencode_plugin', generation };
  spyOn(process, 'kill').mockImplementation(() => true);
  const directory = await mkdtemp(join(tmpdir(), 'bkpr-native-boundary-'));
  const captureFile = join(directory, 'capture.json');
  const capture = JSON.stringify({ grant: { kind: 'opencode', launch_id: 'launch', pane_id: 'pane', token: 'private-capability' },
    program: '/private/bkpr', socket: '/private/socket' });
  await writeFile(captureFile, capture, { mode: 0o600 });
  mock.module('@opencode/plugin', () => ({ Plugin: { define: (value: unknown) => value } }));
  mock.module('node:child_process', () => ({ spawn: (_program: string, args: string[]) => {
    const child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(),
      kill() { queueMicrotask(() => child.emit('close', 0)); return true; },
    });
    let input = '';
    child.stdin.on('data', chunk => { input += chunk.toString(); });
    child.stdin.on('end', () => {
      const request = JSON.parse(input);
      calls.push(request);
      child.stdout.write(JSON.stringify(args[1] === 'agent' ? { conversation: { id: 'root-A', source: 'opencode_plugin', generation: 7 } } : { accepted: true }) + '\n');
      child.emit('close', 0);
    });
    return child;
  } }));
  // This fixture owns virtual SDK/native subprocess boundaries; static loading
  // would resolve the SDK and callbacks before those boundaries are installed.
  const { default: plugin } = await import('./index.ts');
  const context = {
    options: { captureFile, requiredSession: { id: 'root-A' } },
    rpc: { async register(_contract: unknown, value: typeof handlers) {
      registrations += 1;
      handlers = value;
      return { events: { async emit(_name: string, event: Json) {
        if (_name === 'recovery_failed') { failures += 1; return; }
        emitted.push(event);
        if ((event.purpose === 'tool' && autoTools) || (event.purpose === 'wake' && autoWakes)) {
          await handlers.answer({ id: event.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
        }
      } }, async dispose() {} };
    } },
    session: {
      async hook(name: string, callback: (event: Json) => Promise<void>) { hooks.set(name, callback); return { async dispose() { hooks.delete(name); } }; },
      async get({ sessionID }: { sessionID: string }) {
        if (sessionID === 'held-child') await held?.promise;
        if (sessionID === 'root-A' && missing) throw new Error('native session not found');
        return { id: sessionID, time: { created }, ...(['child-A', 'held-child'].includes(sessionID) ? { parentID: 'root-A' } : {}),
          ...(sessionID === 'child-B' ? { parentID: 'root-B' } : {}) };
      },
    },
    tool: {
      async hook(name: string, callback: (event: Json) => Promise<void>) { hooks.set(name, callback); return { async dispose() { hooks.delete(name); } }; },
      async list() { catalogs += 1; return [
        { id: 'bkpr_whoami', options: { namespace: 'bkpr' } },
        { id: 'bkpr_native_action', options: { namespace: 'another_native_namespace' } },
      ]; },
    },
  };
  let launches = 0;
  const start = async () => {
    if (launches > 0) {
      context.options.captureFile = join(directory, `capture-${launches}.json`);
      await writeFile(context.options.captureFile, capture, { mode: 0o600 });
    }
    launches += 1;
    return plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
  };
  let cleanup: (() => Promise<void> | void) | void;
  const locationCleanups: Array<() => Promise<void> | void> = [];
  try {
    cleanup = await start();
    await handlers!.deactivate({ owner: 'stale-startup-owner' });
    let admitted = false;
    const pending = hooks.get('prompt')!({ sessionID: 'root-A', prompt: { text: 'PRIVATE_USER_BODY' } }).then(() => { admitted = true; });
    await nextTurn();
    expect(admitted).toBe(false);
    expect(emitted).toHaveLength(1);
    expect(JSON.stringify(await handlers!.pending({}))).not.toContain('PRIVATE_USER_BODY');
    await expect(handlers!.answer({ id: emitted[0].id, owner: answeringOwner, sessionID: 'root-A', restored: true, binding: binding() })).rejects.toThrow('not active');
    expect(admitted).toBe(false); // Starting is not the deliberately disabled state.
    await handlers!.activate({ owner: 'tui-owner' });
    await handlers!.answer({ id: emitted[0].id, owner: answeringOwner, sessionID: 'root-A', restored: true, binding: binding() });
    await pending;
    expect(admitted).toBe(true);
    await hooks.get('prompt')!({ sessionID: 'root-A', metadata: { 'bkpr.nativeWake': true } });
    expect(emitted).toHaveLength(2);
    expect(emitted.at(-1)!.purpose).toBe('wake');
    await expect(hooks.get('prompt')!({ sessionID: 'root-B', metadata: { 'bkpr.nativeWake': true } })).rejects.toThrow('no longer owns');
    autoTools = false;
    const fenced = hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A', input: { root: 'forged-model-root' } });
    await nextTurn();
    expect(emitted.at(-1)!.purpose).toBe('tool');
    expect(calls).toHaveLength(0); // A's last backend binding is not a TUI selection barrier.
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await fenced;
    autoTools = true;
    expect(calls.find(item => item.kind === 'native_context')).toEqual({ kind: 'native_context',
      conversation: { id: 'root-A', source: 'opencode_plugin', generation: 7 }, context_id: 'child-A' });
    await expect(hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-B' })).rejects.toThrow('not the current native root');
    expect(calls.filter(item => item.kind === 'native_context')).toHaveLength(1);
    expect(JSON.stringify(calls)).not.toContain('PRIVATE_USER_BODY');
    selected = 'root-B';
    generation = 8;
    await expect(hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' })).rejects.toThrow('binding was superseded');
    selected = null;
    await expect(hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' })).rejects.toThrow('no selected native root');
    expect(calls.filter(item => item.kind === 'native_context')).toHaveLength(1);
    selected = 'root-A';
    generation = 7;
    held = Promise.withResolvers<void>();
    const stopped = hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'held-child' });
    await nextTurn();
    await cleanup?.();
    const cancelled = stopped.catch((error: unknown) => error);
    held.resolve();
    const cancellation = await cancelled;
    expect(cancellation).toBeInstanceOf(Error);
    expect(cancellation).toHaveProperty('message', expect.stringContaining('cancelled before dispatch'));
    expect(calls.filter(item => item.kind === 'native_context')).toHaveLength(1);
    cleanup = await start();
    await handlers!.activate({ owner: 'tui-owner' });
    const notRestored = hooks.get('prompt')!({ sessionID: 'root-A' });
    await nextTurn();
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: null, restored: false, binding: null });
    await expect(notRestored).rejects.toThrow('recovery has not completed');
    created = 124;
    const count = emitted.length;
    await expect(hooks.get('prompt')!({ sessionID: 'fresh-home-fallback' })).rejects.toThrow('missing or changed');
    expect(emitted).toHaveLength(count);
    expect(failures).toBe(1);
    await cleanup?.();
    created = 123;
    cleanup = await start();
    await handlers!.activate({ owner: 'tui-owner' });
    // Setup/TUI route binding already completed, but no ordinary native
    // prompt has crossed the recovery fence when A is deleted.
    missing = true;
    const beforeDeletedFallback = emitted.length;
    await expect(hooks.get('prompt')!({ sessionID: 'fresh-home-fallback' })).rejects.toThrow('not found');
    expect(emitted).toHaveLength(beforeDeletedFallback);
    await cleanup?.();
    missing = false;
    cleanup = await start();
    await handlers!.activate({ owner: 'tui-owner' });
    missing = true;
    await expect(hooks.get('prompt')!({ sessionID: 'root-B', metadata: { 'bkpr.nativeWake': true } })).rejects.toThrow('not found');
    await cleanup?.();
    missing = false;
    cleanup = await start();
    await handlers!.activate({ owner: 'tui-owner' });
    // NativeRoute's family request may fail after setup's final GET. Hold
    // the actual TUI challenge while that deletion/home fallback happens.
    const deletedDuringBarrier = hooks.get('prompt')!({ sessionID: 'fresh-home-fallback' });
    await nextTurn();
    selected = 'root-B';
    generation = 8;
    missing = true;
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await expect(deletedDuringBarrier).rejects.toThrow('not found');
    expect(failures).toBe(4);
    await cleanup?.();
    created = 123;
    missing = false;
    selected = 'root-A';
    generation = 7;
    cleanup = await start();
    await handlers!.activate({ owner: 'tui-owner' });
    autoWakes = false;
    const restored = hooks.get('prompt')!({ sessionID: 'root-A', metadata: { 'bkpr.nativeWake': true } });
    await nextTurn();
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await restored;
    autoWakes = true;
    // A completed public pre-admission fence does not pin subsequent user
    // navigation to the recovery root or its continued existence.
    created = 124;
    selected = 'root-B';
    generation = 8;
    const ordinary = hooks.get('prompt')!({ sessionID: 'root-B' });
    await nextTurn();
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await ordinary;
    const activeBackground = hooks.get('prompt')!({ sessionID: 'background-child-A' });
    await nextTurn();
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: null, restored: true, binding: null });
    await activeBackground; // Foreground B does not prohibit ordinary native A work.
    autoTools = false;
    const background = hooks.get('prompt')!({ sessionID: 'background-child-A' });
    const disablingTool = hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' });
    const denied = disablingTool.catch((error: unknown) => error);
    await nextTurn();
    await handlers!.deactivate({ owner: 'tui-owner' });
    await background;
    const denial = await denied;
    expect(denial).toBeInstanceOf(Error);
    expect(denial).toHaveProperty('message', expect.stringContaining('inactive'));
    const inactiveCount = emitted.length;
    await hooks.get('prompt')!({ sessionID: 'ordinary-native-background' });
    const nativeCatalogs = catalogs;
    await hooks.get('execute.before')!({ tool: 'native_read', sessionID: 'ordinary-native-background' });
    expect(catalogs).toBe(nativeCatalogs);
    await hooks.get('execute.before')!({ tool: 'bkpr_native_action', sessionID: 'ordinary-native-background' });
    await expect(hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' })).rejects.toThrow('inactive');
    expect(emitted).toHaveLength(inactiveCount);
    expect(await handlers!.pending({})).toEqual([]);
    const locationHooks = new Map<string, (event: Json) => Promise<void>>();
    let locationHandlers: typeof handlers;
    const siblingContext = {
      ...context,
      rpc: { async register(_contract: unknown, value: typeof handlers) {
        locationHandlers = value;
        return { events: { async emit() { throw new Error('inactive location must not challenge'); } }, async dispose() {} };
      } },
      session: { ...context.session, async hook(name: string, callback: (event: Json) => Promise<void>) {
        locationHooks.set(name, callback); return { async dispose() { locationHooks.delete(name); } };
      } },
      tool: { ...context.tool, async hook(name: string, callback: (event: Json) => Promise<void>) {
        locationHooks.set(name, callback); return { async dispose() { locationHooks.delete(name); } };
      } },
    };
    const siblingCleanup = await plugin.setup(siblingContext as unknown as Parameters<typeof plugin.setup>[0]);
    if (siblingCleanup) locationCleanups.push(siblingCleanup);
    await locationHooks.get('prompt')!({ sessionID: 'new-native-location' });
    await expect(locationHooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' })).rejects.toThrow('inactive');
    expect(await locationHandlers!.activate({ owner: 'sibling-owner' })).toEqual({
      requiredFingerprint: { id: 'root-A', created: 123 }, recovering: false,
    });
    await locationHandlers!.deactivate({ owner: 'sibling-owner' });
    await siblingCleanup?.();
    await handlers!.activate({ owner: 'replacement-owner' });
    answeringOwner = 'replacement-owner';
    await handlers!.deactivate({ owner: 'tui-owner' });
    let replacementPassed = false;
    const replacementPrompt = hooks.get('prompt')!({ sessionID: 'root-B' }).then(() => { replacementPassed = true; });
    await nextTurn();
    expect(replacementPassed).toBe(false); // Stale teardown cannot disable the replacement.
    await expect(handlers!.answer({ id: emitted.at(-1)!.id, owner: 'tui-owner',
      sessionID: selected, restored: true, binding: binding() })).rejects.toThrow('enrollment was superseded');
    expect(replacementPassed).toBe(false); // A delayed old answer cannot be relabeled as the new TUI.
    expect(await handlers!.check_owner({ owner: 'tui-owner' })).toBe('superseded');
    expect(await handlers!.check_owner({ owner: answeringOwner })).toBe('active');
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await replacementPrompt;
    selected = 'root-A';
    generation = 7;
    held = Promise.withResolvers<void>();
    const oldTool = hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'held-child' });
    const superseded = oldTool.catch((error: unknown) => error);
    await nextTurn();
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await nextTurn();
    const attestations = calls.filter(item => item.kind === 'native_context').length;
    await handlers!.deactivate({ owner: 'replacement-owner' });
    await handlers!.activate({ owner: 'next-owner' });
    answeringOwner = 'next-owner';
    held.resolve();
    const supersession = await superseded;
    expect(supersession).toBeInstanceOf(Error);
    expect(supersession).toHaveProperty('message', expect.stringContaining('enrollment was superseded'));
    expect(calls.filter(item => item.kind === 'native_context')).toHaveLength(attestations);
    selected = 'root-B';
    generation = 8;
    await handlers!.deactivate({ owner: 'next-owner' });
    await cleanup?.();
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    const zeroContextChallenges = emitted.length;
    await hooks.get('prompt')!({ sessionID: 'root-B' });
    await expect(hooks.get('execute.before')!({ tool: 'bkpr_whoami', sessionID: 'child-A' })).rejects.toThrow('inactive');
    expect(emitted).toHaveLength(zeroContextChallenges);
    expect(await handlers!.activate({ owner: 'restarted-owner' })).toEqual({
      requiredFingerprint: { id: 'root-A', created: 123 }, recovering: false,
    }); // Zero-Location intervals retain only phase/canonical fingerprint, not callbacks.
    answeringOwner = 'restarted-owner';
    let restartedPassed = false;
    const restarted = hooks.get('prompt')!({ sessionID: 'root-B' }).then(() => { restartedPassed = true; });
    await nextTurn();
    expect(restartedPassed).toBe(false);
    await handlers!.answer({ id: emitted.at(-1)!.id, owner: answeringOwner, sessionID: selected, restored: true, binding: binding() });
    await restarted;
    await cleanup?.();
    const beforeCapacity = registrations;
    const leases: Array<() => Promise<void> | void> = [];
    const boundedContext = { ...context, options: { captureFile: '', requiredSession: undefined } };
    for (let index = 0; index < 64; index += 1) {
      boundedContext.options.captureFile = join(directory, `bounded-${index}.json`);
      await writeFile(boundedContext.options.captureFile, capture, { mode: 0o600 });
      const lease = await plugin.setup(boundedContext as unknown as Parameters<typeof plugin.setup>[0]);
      if (lease) { leases.push(lease); locationCleanups.push(lease); }
    }
    boundedContext.options.captureFile = join(directory, 'bounded-overflow.json');
    await writeFile(boundedContext.options.captureFile, capture, { mode: 0o600 });
    await expect(plugin.setup(boundedContext as unknown as Parameters<typeof plugin.setup>[0])).rejects.toThrow('capacity exhausted');
    expect(registrations).toBe(beforeCapacity + 64); // Reject before allocating/registering another live identity.
    await leases[0]();
    const overflowLease = await plugin.setup(boundedContext as unknown as Parameters<typeof plugin.setup>[0]);
    if (overflowLease) locationCleanups.push(overflowLease);
    expect(registrations).toBe(beforeCapacity + 65); // Evict only the now-unreferenced identity.
  } finally {
    held?.resolve();
    await cleanup?.();
    for (const dispose of locationCleanups) await dispose();
    mock.restore();
    await rm(directory, { recursive: true, force: true });
  }
});
