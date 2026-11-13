import { expect, mock, spyOn, test } from 'bun:test';
import { EventEmitter } from 'node:events';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { PassThrough } from 'node:stream';
import { setImmediate as nextTurn } from 'node:timers/promises';
import type { Json } from '../shared/native.ts';

test('native TUI deactivation awaits pending binding and CAS-unbinds only its retained generation', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'bkpr-native-tui-'));
  const captureFile = join(directory, 'capture.json');
  await writeFile(captureFile, JSON.stringify({ grant: { kind: 'opencode', launch_id: 'launch', pane_id: 'pane', token: 'private-capability' },
    program: '/private/bkpr', socket: '/private/socket' }), { mode: 0o600 });
  let active: Json | null = null;
  let generation = 0;
  let route: { type: 'home' } | { type: 'session'; sessionID: string } = { type: 'session', sessionID: 'A' };
  let bindGate: { promise: Promise<void>; resolve(): void } | undefined;
  const events: Json[] = [];
  const commands: string[][] = [];
  const lifecycle: Array<{ method: string; owner: string }> = [];
  let activeOwner: string | undefined;
  const receivers: Array<{ killed: boolean }> = [];
  let listener: ((event: { details: Json }) => void) | undefined;
  const replies: Json[] = [];
  let disposed = false;
  let stopped = false;
  let fingerprint: { id: string; created: number } | null = null;
  let recovering = false;
  let exists = true;
  let supersedeOnLookup = false;
  let nativeRpcUnavailable = false;
  let activationFailure = false;
  let enrollmentActive = true;
  let exited = Promise.withResolvers<void>();
  const disconnect = Promise.withResolvers<never>();
  const authorityAtExit: Array<Json | null> = [];
  const lookups: string[] = [];
  const navigation: unknown[] = [];
  const killed: unknown[][] = [];
  spyOn(process, 'kill').mockImplementation((...args) => {
    killed.push(args);
    authorityAtExit.push(structuredClone(active));
    exited.resolve();
    return true;
  });
  mock.module('@opencode/plugin/tui', () => ({ Plugin: { define: (value: unknown) => value } }));
  // The fixture controls the public reactive selection boundary without
  // mounting or inspecting any native renderer/composer implementation.
  mock.module('solid-js', () => ({
    createRoot: (setup: (dispose: () => void) => unknown) => {
      disposed = false;
      return setup(() => { disposed = true; });
    },
    createEffect: (callback: () => void) => { callback(); },
  }));
  mock.module('node:child_process', () => ({ spawn: (_program: string, args: string[]) => {
    commands.push(args);
    const child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(), killed: false,
      kill() { this.killed = true; queueMicrotask(() => this.emit('close', 0)); return true; },
    });
    if (args[1] === 'agent-subscribe') {
      receivers.push(child);
    } else if (args[1] === 'prompt-submitted') {
      queueMicrotask(() => child.emit('close', 0));
    } else {
      let input = '';
      child.stdin.on('data', chunk => { input += chunk.toString(); });
      child.stdin.on('end', async () => {
        const request = JSON.parse(input);
        let reply: Json = { conversation: active, binding_revision: generation };
        if (args[1] === 'event') {
          events.push(request);
          const accepted = request.expected_revision === generation &&
            (request.kind !== 'unbind' || JSON.stringify(request.conversation) === JSON.stringify(active));
          if (accepted) {
            generation += 1;
            active = request.kind === 'bind' ? { id: request.conversation_id, source: 'opencode_plugin', generation } : null;
          }
          reply = { accepted, binding: { revision: generation, conversation: structuredClone(active) } };
          if (request.kind === 'bind') await bindGate?.promise;
        }
        child.stdout.write(JSON.stringify(reply) + '\n');
        child.emit('close', 0);
      });
    }
    return child;
  } }));
  // SDK and subprocess boundaries must be installed before the consumer loads.
  const { default: plugin } = await import('./tui.ts');
  const context = {
    options: { captureFile, requiredSession: undefined as { id: string } | undefined }, location: { directory },
    ui: { router: {
      current: () => route,
      navigate(destination: { type: 'home' } | { type: 'session'; sessionID: string }) { navigation.push(destination); route = destination; },
    }, toast: { show() {} } },
    data: {
      listen(handler: (event: { details: Json }) => void) { listener = handler; stopped = false; return () => { stopped = true; }; },
      location: { default: () => ({ directory }) },
      session: { get: (id: string) => ({ id, location: { directory } }), root: (id: string) => id, async sync() {} },
    },
    client: {
      event: {
        async *subscribe({ signal }: { signal: AbortSignal }) {
          const stopped = Promise.withResolvers<void>();
          const abort = (): void => stopped.resolve();
          signal.addEventListener('abort', abort, { once: true });
          if (signal.aborted) abort();
          try {
            yield { type: 'server.connected' };
            await Promise.race([stopped.promise, disconnect.promise]);
          } finally {
            signal.removeEventListener('abort', abort);
          }
        },
      },
      plugin: { async list() { return { data: [{ id: 'bkpr-native', features: { server: true, rpc: true }, state: { status: 'active' } }] }; } },
      session: {
        async prompt() { throw new Error('unexpected wake'); },
        async get({ sessionID }: { sessionID: string }) {
          lookups.push(sessionID);
          if (supersedeOnLookup) {
            activeOwner = 'replacement-owner';
            // The replacement reserved an unbound epoch and rebound the same
            // launch before the old native recovery getter failed.
            generation += 2;
            active = { id: 'replacement', source: 'opencode_plugin', generation };
            throw new Error('old native recovery lookup was superseded');
          }
          if (!exists) throw new Error('native session not found');
          return { id: sessionID, time: { created: 123 } };
        },
      },
      rpc: () => ({
        async activate(input: { owner: string }) {
          lifecycle.push({ method: 'activate', ...input });
          activeOwner = input.owner;
          enrollmentActive = !activationFailure;
          if (activationFailure) throw new Error('native recovery target unavailable');
          return { requiredFingerprint: fingerprint, recovering };
        },
        async deactivate(input: { owner: string }) {
          lifecycle.push({ method: 'deactivate', ...input });
          if (nativeRpcUnavailable) throw new Error('native transport lost');
          if (input.owner === activeOwner) activeOwner = undefined;
        },
        async check_owner(input: { owner: string }) {
          if (!activeOwner) return 'unowned';
          if (input.owner !== activeOwner) return 'superseded';
          return enrollmentActive ? 'active' : 'inactive';
        },
        async pending() { return []; },
        async answer(reply: Json) { replies.push(reply); },
      }),
    },
  };
  let cleanup: (() => Promise<void> | void) | void;
  try {
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    await nextTurn();
    expect(active).toEqual({ id: 'A', source: 'opencode_plugin', generation: 2 });
    route = { type: 'session', sessionID: 'B' };
    const bindCount = events.filter(event => event.kind === 'bind').length;
    listener!({ details: { type: 'rpc.bkpr-native.challenge', data: { id: 'background-prompt', sessionID: 'A', purpose: 'prompt' },
      location: { directory } } });
    await nextTurn();
    expect(replies.at(-1)).toMatchObject({ id: 'background-prompt', sessionID: null, binding: null, restored: true });
    expect(events.filter(event => event.kind === 'bind')).toHaveLength(bindCount);
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
    listener!({ details: { type: 'rpc.bkpr-native.challenge', data: { id: 'queued-wake', sessionID: 'B', purpose: 'wake' },
      location: { directory } } });
    await nextTurn();
    expect(replies.at(-1)).toMatchObject({ id: 'queued-wake', sessionID: 'B', binding: { id: 'B' } });
    expect(replies.at(-1)?.owner).toBe(activeOwner);
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
    const ownBinding = active;
    const replyCount = replies.length;
    const replacement = { id: 'replacement', source: 'opencode_plugin', generation: ++generation };
    active = replacement;
    await cleanup?.();
    expect(events.at(-1)).toEqual({ kind: 'unbind', conversation: ownBinding, expected_revision: Number(ownBinding?.generation) });
    expect(active).toEqual(replacement);
    expect(stopped).toBe(true);
    expect(disposed).toBe(true);
    expect(receivers[0].killed).toBe(true);
    expect(lifecycle).toEqual([{ method: 'activate', owner: lifecycle[0].owner }, { method: 'deactivate', owner: lifecycle[0].owner }]);

    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    await nextTurn();
    bindGate = Promise.withResolvers<void>();
    route = { type: 'session', sessionID: 'C' };
    // Challenge arrives before the selection effect publishes C. It must
    // independently await the owning TUI selection, not echo backend B.
    listener!({ details: { type: 'rpc.bkpr-native.challenge', data: { id: 'tool-barrier', sessionID: 'child-A', purpose: 'tool' },
      location: { directory } } });
    await nextTurn();
    const pendingGeneration = generation;
    expect(active?.id).toBe('C');
    expect(replies).toHaveLength(replyCount);
    const receiverCount = receivers.length;
    let completed = false;
    const closing = Promise.resolve(cleanup?.()).then(() => { completed = true; });
    await nextTurn();
    expect(stopped).toBe(true);
    expect(disposed).toBe(true);
    expect(completed).toBe(false);
    bindGate.resolve();
    await closing;
    expect(active).toBeNull();
    expect(receivers).toHaveLength(receiverCount);
    expect(events.at(-1)).toEqual({ kind: 'unbind', expected_revision: pendingGeneration,
      conversation: { id: 'C', source: 'opencode_plugin', generation: pendingGeneration } });
    expect(replies).toHaveLength(replyCount); // Deactivation also discards the late answer.
    bindGate = undefined;
    context.options.requiredSession = { id: 'A' }; // The launcher supplies no creation timestamp.
    fingerprint = { id: 'A', created: 123 };
    recovering = true;
    route = { type: 'home' };
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    await nextTurn();
    expect(navigation.at(-1)).toEqual({ type: 'session', sessionID: 'A' });
    expect(lookups).toEqual(['A', 'A', 'A', 'A']);
    expect(active?.id).toBe('A');
    await cleanup?.();
    const navigated = navigation.length;
    const lookedUp = lookups.length;
    recovering = false;
    route = { type: 'session', sessionID: 'B' };
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    await nextTurn();
    expect(navigation).toHaveLength(navigated);
    expect(lookups).toHaveLength(lookedUp);
    expect(active?.id).toBe('B'); // Re-enabling after handoff does not restore A again.
    await cleanup?.();
    recovering = true;
    supersedeOnLookup = true;
    route = { type: 'home' };
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    expect(active?.id).toBe('replacement');
    expect(activeOwner).toBe('replacement-owner');
    expect(killed).toHaveLength(0); // Stale recovery teardown must not terminate a replacement.
    supersedeOnLookup = false;
    exists = false;
    route = { type: 'home' };
    const beforeMissing = events.filter(event => event.kind === 'bind').length;
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    expect(events.filter(event => event.kind === 'bind')).toHaveLength(beforeMissing);
    expect(killed).toEqual([[process.pid, 'SIGTERM']]);
    await cleanup?.();
    exists = true;
    activationFailure = true;
    const exitsBeforeActivationFailure = killed.length;
    const navigationBeforeActivationFailure = navigation.length;
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    expect(killed).toHaveLength(exitsBeforeActivationFailure + 1);
    expect(active).toBeNull();
    expect(navigation).toHaveLength(navigationBeforeActivationFailure);
    await cleanup?.();
    activationFailure = false;
    route = { type: 'home' };
    exited = Promise.withResolvers<void>();
    cleanup = await plugin.setup(context as unknown as Parameters<typeof plugin.setup>[0]);
    await nextTurn();
    expect(active?.id).toBe('A');
    nativeRpcUnavailable = true;
    disconnect.reject(new Error('native lifecycle transport lost'));
    await exited.promise;
    expect(active).toBeNull();
    expect(authorityAtExit.at(-1)).toBeNull();
    cleanup = undefined;
  } finally {
    bindGate?.resolve();
    await cleanup?.();
    mock.restore();
    await rm(directory, { recursive: true, force: true });
  }
});
