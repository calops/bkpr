import { expect, mock, test } from 'bun:test';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { setImmediate as nextTurn } from 'node:timers/promises';
import type { Json, NativeCallbacks } from '../shared/native.ts';

// Import NativeSession inside fixtures: a static import would capture the real
// subprocess implementation before each test installs its module boundary.

/** Stateful Core boundary: conditional misses do not mutate or publish. */
class BindingBackend implements NativeCallbacks {
  program = '/private/bkpr';
  environment = {};
  revision = 0;
  active: Json | null = null;
  effects = 0;
  facts: unknown[][] = [];
  events: Json[] = [];
  requests: Json[] = [];
  dispatch?: (value: Json, commit: () => Json) => Promise<Json>;
  afterSnapshot?: () => Promise<void>;
  snapshot(): Json { return structuredClone({ revision: this.revision, conversation: this.active }); }
  async fact(...args: Parameters<NativeCallbacks['fact']>): Promise<void> {
    if (args[0] === 'bind') throw new Error('unconditional native bind is forbidden');
    this.facts.push(args);
  }
  commit(value: Json): Json {
    if (value.expected_revision !== this.revision ||
        (value.kind === 'unbind' && JSON.stringify(value.conversation) !== JSON.stringify(this.active))) {
      return { accepted: false, binding: this.snapshot() };
    }
    this.revision += 1;
    this.active = value.kind === 'bind' ?
      { id: value.conversation_id, source: 'opencode_plugin', generation: this.revision } : null;
    this.effects += 1;
    return { accepted: true, binding: this.snapshot() };
  }
  async event(value: Json): Promise<Json> {
    this.events.push(structuredClone(value));
    return this.dispatch ? this.dispatch(value, () => this.commit(value)) : this.commit(value);
  }
  async request(value: Json): Promise<Json> {
    this.requests.push(value);
    if (value.method !== 'whoami') return value.method === 'wake_validate' ? { current: true } : {};
    const snapshot = this.snapshot();
    await this.afterSnapshot?.();
    return { binding_revision: snapshot.revision, conversation: snapshot.conversation };
  }
}

function receiverBoundary(): Array<EventEmitter & { stdout: PassThrough; killed: boolean }> {
  const children: Array<EventEmitter & { stdout: PassThrough; killed: boolean }> = [];
  mock.module('node:child_process', () => ({ spawn: () => {
    const child = Object.assign(new EventEmitter(), {
      stdout: new PassThrough(), stderr: new PassThrough(), killed: false,
      kill() { this.killed = true; queueMicrotask(() => this.emit('close', 0)); return true; },
    });
    children.push(child);
    return child;
  } }));
  return children;
}

test('native root binding, first prompt and queued notifications keep separate facts', async () => {
  const children: Array<EventEmitter & { stdout: PassThrough; killed: boolean }> = [];
  const commands: string[][] = [];
  function child() {
    return Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(), killed: false,
      kill() { this.killed = true; queueMicrotask(() => this.emit('close', 0)); return true; },
    });
  }
  mock.module('node:child_process', () => ({ spawn: (_program: string, args: string[]) => {
    commands.push(args);
    const process = child();
    children.push(process);
    queueMicrotask(() => process.stdout.write('{"receiver_id":"receiver"}\n'));
    return process;
  } }));
  // Import after installing the native subprocess boundary for this fixture.
  const { NativeSession, validateQueueReceipt, requiredRequest, requiredSession, recoveryIdentity, validateRecovery } = await import('./session.ts');
  const receipt = { id: 'msg_exact', sessionID: 'ses_exact', type: 'user', delivery: 'queue' };
  for (const value of [null, [], {}, { ...receipt, sessionID: 'other' }, { ...receipt, id: 'other' },
    { ...receipt, type: 'synthetic' }, { ...receipt, delivery: 'steer' }]) {
    expect(() => validateQueueReceipt(value, 'ses_exact', 'msg_exact')).toThrow();
  }
  const required = requiredSession({ id: 'ses_exact', created: 123 })!;
  expect(requiredRequest({ id: 'ses_exact' })).toEqual({ id: 'ses_exact' });
  expect(recoveryIdentity({ id: 'ses_exact', time: { created: 123 } }, { id: 'ses_exact' })).toEqual(required);
  expect(() => recoveryIdentity({ id: 'ses_exact', parentID: 'parent', time: { created: 123 } }, { id: 'ses_exact' })).toThrow();
  expect(() => recoveryIdentity({ id: 'ses_exact', time: { created: 1.5 } }, { id: 'ses_exact' })).toThrow();
  expect(() => validateRecovery({ id: 'ses_exact', time: { created: 124 } }, required)).toThrow();
  expect(() => validateRecovery({ id: 'fresh_fallback', time: { created: 123 } }, required)).toThrow();
  const native = new BindingBackend();
  const { facts, requests, events } = native;
  const queued: Json[] = [];
  let root: string | null = 'native-A';
  let malformed = false;
  let hold: ((value: unknown) => void) | undefined;
  const session = new NativeSession(native, () => root, async (input, signal) => {
    queued.push(input);
    // Native queued preparation re-enters this TUI's fence. It must not
    // await its own delivery chain or manufacture a user-prompt fact.
    expect((await session.barrier())?.id).toBe(input.sessionID);
    if (hold) {
      const { promise, resolve } = Promise.withResolvers<unknown>();
      hold = resolve;
      signal.addEventListener('abort', () => resolve({}), { once: true });
      return promise;
    }
    return { id: input.id, sessionID: input.sessionID, type: 'user', delivery: malformed ? 'steer' : 'queue' };
  }, () => {}, async () => true);
  const notice = async (id: string, generation: number) => {
    children.at(-1)!.stdout.write(JSON.stringify({ notification: { notification_id: id, generation,
      session_id: root, message_count: 2, unread_count: 3, text: 'PRIVATE_PEER_BODY' } }) + '\n');
    await nextTurn(); await nextTurn();
  };
  try {
    await session.select(root);
    expect(commands[0]).toEqual(['capture', 'agent-subscribe', 'native-A', '--mode', 'future_turn']);
    await session.prompt('native-child');
    expect(facts.filter(fact => fact[0] === 'prompt-submitted')).toHaveLength(0);
    await session.prompt('native-A');
    await session.prompt('native-A');
    expect(facts.filter(fact => fact[0] === 'prompt-submitted')).toHaveLength(1);
    await notice('accepted', 1);
    await notice('accepted', 1);
    expect(queued).toHaveLength(1);
    expect(queued[0].delivery).toBe('queue');
    expect(queued[0].metadata).toEqual({ 'bkpr.nativeWake': true });
    expect(requests.find(item => item.notification_id === 'accepted' && item.method === 'wake_outcome')?.outcome).toEqual({ state: 'accepted' });
    malformed = true;
    await notice('ambiguous', 2);
    await notice('ambiguous', 2);
    expect(queued).toHaveLength(2);
    expect(requests.find(item => item.notification_id === 'ambiguous' && item.method === 'wake_outcome')?.outcome).toMatchObject({ state: 'unknown' });
    hold = () => {};
    await notice('cancelled', 3);
    root = 'native-B';
    const beforeBackground = facts.length;
    await session.prompt('native-A');
    expect(facts).toHaveLength(beforeBackground);
    await session.select(root);
    await nextTurn(); await nextTurn();
    expect(children[0].killed).toBe(true);
    expect(requests.find(item => item.notification_id === 'cancelled' && item.method === 'wake_outcome')?.outcome).toMatchObject({ state: 'unknown' });
    await session.prompt('native-B');
    root = null;
    await session.select(root);
    expect(events.at(-1)).toEqual({ kind: 'unbind', expected_revision: 3,
      conversation: { id: 'native-B', source: 'opencode_plugin', generation: 3 } });
    root = 'native-A';
    await session.select(root);
    await session.prompt('native-A');
    expect(facts.filter(fact => fact[0] === 'prompt-submitted')).toHaveLength(1);
    expect(events.filter(event => event.kind === 'bind').map(event => event.conversation_id)).toEqual(['native-A', 'native-B', 'native-A']);
    expect(JSON.stringify({ facts, requests, queued })).not.toContain('PRIVATE_PEER_BODY');
    root = `ses_${'x'.repeat(1020)}`;
    hold = undefined;
    malformed = false;
    await session.select(root);
    await notice('long-native-root', 4);
    expect(queued.at(-1)?.sessionID).toBe(root);
    expect(requests.find(item => item.notification_id === 'long-native-root' && item.method === 'wake_outcome')?.outcome).toEqual({ state: 'accepted' });
    const beforeInvalidRoot = facts.length;
    expect(() => session.select(`${root}x`)).toThrow('invalid native session ID');
    expect(facts).toHaveLength(beforeInvalidRoot);
  } finally {
    await session.close();
    mock.restore();
  }
});

test('selection barriers serialize bind/home and old close cannot revoke a same-root replacement', async () => {
  const children = receiverBoundary();
  const { NativeSession } = await import('./session.ts');
  const native = new BindingBackend();
  let root: string | null = 'A';
  let owner = 'first';
  let gate: { promise: Promise<void>; resolve(): void } | undefined;
  const make = (id: string) => new NativeSession(native, () => root, async () => {
    throw new Error('unexpected wake');
  }, () => {}, async () => owner === id);
  const first = make('first');
  let replacement: InstanceType<typeof NativeSession> | undefined;
  try {
    expect(await first.barrier()).toEqual({ id: 'A', source: 'opencode_plugin', generation: 2 });
    expect(native.facts).toHaveLength(0);
    const stable = native.events.length;
    await first.barrier();
    expect(native.events).toHaveLength(stable); // Same selection does not rotate its own lease.
    gate = Promise.withResolvers<void>();
    native.dispatch = async (value, commit) => {
      const result = commit();
      if (value.kind === 'bind') await gate?.promise;
      return result;
    };
    root = 'B';
    let admitted = false;
    const barrier = first.barrier().then(value => { admitted = true; return value; });
    const changed = barrier.catch((error: unknown) => error);
    await nextTurn();
    expect(admitted).toBe(false);
    root = null;
    const home = first.select(null);
    gate.resolve();
    expect(await changed).toHaveProperty('message', expect.stringContaining('selection changed'));
    await home;
    expect(native.active).toBeNull();
    expect(native.events.at(-1)).toEqual({ kind: 'unbind', expected_revision: 3,
      conversation: { id: 'B', source: 'opencode_plugin', generation: 3 } });
    expect(children).toHaveLength(1);
    gate = undefined;
    root = 'A';
    await first.barrier();
    const ownRevision = native.revision;
    owner = 'replacement';
    replacement = make(owner);
    const current = await replacement.barrier();
    expect(current?.id).toBe('A');
    expect(current?.generation).toBe(ownRevision + 2); // Enrollment reserves an epoch, including the same root.
    const effects = native.effects;
    await first.close();
    expect(native.effects).toBe(effects);
    expect(native.active?.generation).toBe(current?.generation);
    expect(native.events.at(-1)?.expected_revision).toBe(ownRevision);
    await expect(first.barrier()).rejects.toThrow('closed');
    const count = native.events.length;
    await first.close();
    expect(native.events).toHaveLength(count);
  } finally {
    gate?.resolve();
    await first.close();
    await replacement?.close();
    mock.restore();
  }
});

test('unknown initial binds fence both possible states and never replay a late Core mutation', async () => {
  receiverBoundary();
  const { NativeSession } = await import('./session.ts');
  try {
    for (const committed of [false, true]) {
      const native = new BindingBackend();
      let late: (() => Json) | undefined;
      native.dispatch = async (value, commit) => {
        if (value.kind !== 'bind') return commit();
        if (committed) commit(); else late = commit;
        throw new Error('callback disconnected after Core dispatch; outcome unknown');
      };
      const session = new NativeSession(native, () => 'A', async () => {}, () => {}, async () => true);
      await expect(session.select('A')).rejects.toThrow('outcome unknown');
      expect(native.active).toBeNull();
      expect(native.events.filter(value => value.kind === 'bind')).toHaveLength(1);
      expect(native.events.slice(-2)).toEqual([
        { kind: 'unbind', expected_revision: 1, conversation: null },
        { kind: 'unbind', expected_revision: 2, conversation: { id: 'A', source: 'opencode_plugin', generation: 2 } },
      ]);
      const effects = native.effects;
      if (late) expect(late().accepted).toBe(false);
      const original = native.events.find(value => value.kind === 'bind')!;
      expect(native.commit(original).accepted).toBe(false); // Bound and unbound revocation both fence stale producers.
      expect(native.effects).toBe(effects);
      expect(native.requests).toHaveLength(2); // Cleanup never discovers a newer owner's revision.
      await expect(session.select('B')).rejects.toThrow('outcome unknown');
      await session.close();
    }
  } finally { mock.restore(); }
});

test('unknown enrollment fails closed and fences a queued unbound reservation without a fresh lookup', async () => {
  const { NativeSession } = await import('./session.ts');
  const native = new BindingBackend();
  let late: (() => Json) | undefined;
  native.dispatch = async (_value, commit) => {
    if (!late) { late = commit; throw new Error('enrollment outcome unknown'); }
    return commit();
  };
  const session = new NativeSession(native, () => null, async () => {}, () => {}, async () => true);
  await expect(session.enroll()).rejects.toThrow('outcome unknown');
  expect(native.requests).toHaveLength(1);
  expect(native.active).toBeNull();
  const effects = native.effects;
  expect(late!().accepted).toBe(false);
  expect(native.effects).toBe(effects);
  await expect(session.barrier()).rejects.toThrow('outcome unknown');
  await session.close();
});

test('late old bind ACK or unknown outcome cannot undo a new same-root enrollment', async () => {
  const children = receiverBoundary();
  const { NativeSession } = await import('./session.ts');
  try {
    for (const unknown of [false, true]) {
      const native = new BindingBackend();
      let owner = 'old';
      const gate = Promise.withResolvers<void>();
      let delayed = false;
      native.dispatch = async (value, commit) => {
        const result = commit();
        if (value.kind === 'bind' && !delayed) {
          delayed = true;
          await gate.promise;
          if (unknown) throw new Error('old bind outcome unknown');
        }
        return result;
      };
      const make = (id: string) => new NativeSession(native, () => 'A', async () => {}, () => {}, async () => owner === id);
      const old = make(owner);
      const binding = old.select('A').catch((error: unknown) => error);
      await nextTurn();
      expect(native.active?.generation).toBe(2);
      owner = 'new';
      const replacement = make(owner);
      expect((await replacement.barrier())?.generation).toBe(4);
      const effects = native.effects;
      const receivers = children.length;
      gate.resolve();
      expect(await binding).toBeInstanceOf(Error);
      await old.close();
      expect(native.effects).toBe(effects);
      expect(native.active?.generation).toBe(4);
      expect(children).toHaveLength(receivers);
      await replacement.close();
    }
  } finally { mock.restore(); }
});

test('only a known CAS miss refreshes the snapshot and owner check follows each Core snapshot', async () => {
  receiverBoundary();
  const { NativeSession } = await import('./session.ts');
  const native = new BindingBackend();
  let owner = true;
  let raced = false;
  native.dispatch = async (value, commit) => {
    if (value.kind === 'bind' && !raced) {
      raced = true;
      native.commit({ kind: 'unbind', conversation: null, expected_revision: native.revision });
    }
    const effects = native.effects;
    const result = commit();
    if (result.accepted === false) expect(native.effects).toBe(effects);
    return result;
  };
  const session = new NativeSession(native, () => 'A', async () => {}, () => {}, async () => owner);
  try {
    expect((await session.barrier())?.generation).toBe(3);
    expect(native.events.filter(event => event.kind === 'bind').map(event => event.expected_revision)).toEqual([1, 2]);
    await session.close();
    native.afterSnapshot = async () => { owner = false; };
    const stale = new NativeSession(native, () => 'A', async () => {}, () => {}, async () => owner);
    const count = native.events.length;
    await expect(stale.enroll()).rejects.toThrow('superseded');
    expect(native.events).toHaveLength(count); // A revision fetched before supersession grants no authority.
    await stale.close();
  } finally {
    await session.close();
    mock.restore();
  }
});
