import { expect, mock, test } from 'bun:test';
import type { ExtensionAPI, ExtensionContext } from '@earendil-works/pi-coding-agent';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { setImmediate as nextTurn } from 'node:timers/promises';
import type { Json, NativeCallbacks } from '../shared/native.ts';

type Child = EventEmitter & { stdout: PassThrough; stderr: PassThrough; killed: boolean; kill(): boolean };
const children: { args: string[]; child: Child }[] = [];
// Run separately from the other native adapters: only the subscription process boundary is replaced.
mock.module('node:child_process', () => ({
  spawn(_program: string, args: string[]) {
    const child: Child = Object.assign(new EventEmitter(), {
      stdout: new PassThrough(), stderr: new PassThrough(), killed: false,
      kill() { this.killed = true; queueMicrotask(() => child.emit('close', 0)); return true; },
    });
    children.push({ args, child });
    return child;
  },
}));
// Import after installing the process boundary: a static import would bind the real spawn first.
const { registerPiCapture } = await import('./index');

function fixture(name: string, launchId = name) {
  const facts: { command: string; source: string; session: string }[] = [];
  const requests: Json[] = [];
  const messages: Json[] = [];
  const warnings: string[] = [];
  const handlers = new Map<string, (event: Json, ctx: ExtensionContext) => Promise<void> | void>();
  let session = 'native-root-a';
  let bind: (() => Promise<void>) | undefined;
  let validate: (() => Promise<Json>) | undefined;
  let failOutcome = false;
  let throwDispatch = false;
  const native: NativeCallbacks = {
    program: '/private/bkpr', environment: { BKPR_CAPTURE_LAUNCH_ID: launchId },
    async fact(command, source, session) {
      facts.push({ command, source, session });
      if (command === 'bind') await bind?.();
    },
    async request(value, signal) {
      requests.push(value);
      if (signal?.aborted) throw new Error('cancelled');
      if (value.method === 'wake_validate') return validate ? await validate() : { current: true };
      if (failOutcome) throw new Error('outcome unknown');
      return {};
    },
  };
  const api = {
    on(name: string, handler: (event: Json, ctx: ExtensionContext) => Promise<void> | void) { handlers.set(name, handler); },
    registerMcpServer() {},
    sendMessage(message: Json, options: Json) {
      messages.push({ message, options });
      if (throwDispatch) throw new Error('native dispatch may already have happened');
    },
  } as unknown as ExtensionAPI;
  const ctx = { mode: 'tui', sessionManager: { getSessionId: () => session },
    ui: { notify(message: string) { warnings.push(message); } } } as unknown as ExtensionContext;
  registerPiCapture(api, native, `/private/${name}/capture.json`);
  const emit = async (event: string, value: Json = {}) => { await handlers.get(event)?.(value, ctx); };
  const receiver = () => children.at(-1)!.child;
  const handshake = () => receiver().stdout.write('{"receiver_id":"native-receiver"}\n');
  const notify = (id: string, overrides: Json = {}) => receiver().stdout.write(JSON.stringify({ notification: {
    notification_id: id, generation: 1, session_id: session, message_count: 1, unread_count: 2,
    body: 'PRIVATE_PEER_BODY', ...overrides,
  } }) + '\n');
  return { facts, requests, messages, warnings, emit, receiver, handshake, notify,
    setSession(value: string) { session = value; },
    setBind(value: () => Promise<void>) { bind = value; },
    setValidate(value: () => Promise<Json>) { validate = value; },
    failOutcome() { failOutcome = true; },
    throwDispatch() { throwDispatch = true; },
  };
}

async function settle(): Promise<void> {
  await nextTurn();
  await nextTurn();
}

test('startup awaits exact binding and first submitted prompt is launch-wide across native replacement', async () => {
  const first = fixture('lifecycle');
  const gate = Promise.withResolvers<void>();
  first.setBind(() => gate.promise);
  const before = children.length;
  let started = false;
  const startup = first.emit('session_start', { reason: 'startup' }).then(() => { started = true; });
  await settle();
  expect(started).toBe(false);
  expect(children).toHaveLength(before);
  expect(first.facts).toEqual([{ command: 'bind', source: 'pi_extension', session: 'native-root-a' }]);
  gate.resolve();
  await startup;
  expect(children).toHaveLength(before + 1);
  await first.emit('before_agent_start', { messageType: 'custom', message: { content: 'not a user prompt' } });
  expect(first.facts).toHaveLength(1);
  await first.emit('before_agent_start', { messageType: 'user', prompt: 'PRIVATE_PROMPT' });
  await first.emit('before_agent_start', { messageType: 'user', prompt: 'SECOND_PRIVATE_PROMPT' });
  expect(first.facts).toEqual([
    { command: 'bind', source: 'pi_extension', session: 'native-root-a' },
    { command: 'prompt-submitted', source: 'pi_extension', session: 'native-root-a' },
  ]);
  const oldReceiver = first.receiver();
  await first.emit('session_shutdown', { reason: 'resume' });
  expect(oldReceiver.killed).toBe(true);
  const resumed = fixture('lifecycle');
  resumed.setSession('native-root-b');
  await resumed.emit('session_start', { reason: 'resume' });
  await resumed.emit('before_agent_start', { messageType: 'user', prompt: 'PRIVATE_RESUMED_PROMPT' });
  expect(resumed.facts).toEqual([{ command: 'bind', source: 'pi_extension', session: 'native-root-b' }]);
  await resumed.emit('session_shutdown', { reason: 'quit' });
  const fresh = fixture('lifecycle', 'new-launch');
  await fresh.emit('session_start');
  await fresh.emit('before_agent_start', { messageType: 'user' });
  expect(fresh.facts.filter(fact => fact.command === 'prompt-submitted')).toHaveLength(1);
  await fresh.emit('session_shutdown');
});

test('ambiguous bind never redispatches through a replacement runtime', async () => {
  const first = fixture('ambiguous-bind');
  first.setBind(async () => { throw new Error('callback timeout after possible native mutation'); });
  const before = children.length;
  await first.emit('session_start');
  expect(first.facts).toHaveLength(1);
  expect(children).toHaveLength(before);
  await first.emit('session_shutdown', { reason: 'reload' });
  const replacement = fixture('ambiguous-bind');
  await replacement.emit('session_start', { reason: 'reload' });
  await replacement.emit('before_agent_start', { messageType: 'user' });
  expect(replacement.facts).toHaveLength(0);
  expect(children).toHaveLength(before);
  await replacement.emit('session_shutdown');
});

test('body-free validated steering is deduplicated and void or throwing native APIs remain unknown', async () => {
  const item = fixture('steering');
  await item.emit('session_start');
  item.handshake();
  item.notify('offered');
  item.notify('offered');
  await settle();
  expect(item.messages).toHaveLength(1);
  expect(item.messages[0]).toMatchObject({ message: { customType: 'bkpr-inbox', display: true },
    options: { triggerTurn: true, deliverAs: 'steer' } });
  expect(JSON.stringify(item.messages)).not.toContain('PRIVATE_PEER_BODY');
  expect(item.requests.filter(request => request.method === 'wake_outcome')).toEqual([
    expect.objectContaining({ notification_id: 'offered', outcome: expect.objectContaining({ state: 'unknown' }) }),
  ]);
  item.throwDispatch();
  item.notify('dispatch-threw');
  item.notify('dispatch-threw');
  await settle();
  expect(item.messages).toHaveLength(2);
  expect(item.requests.filter(request => request.method === 'wake_outcome')).toHaveLength(2);
  expect(item.requests.every(request => (request.outcome as Json | undefined)?.state !== 'accepted')).toBe(true);
  item.failOutcome();
  item.notify('outcome-unrecorded');
  await settle();
  expect(item.receiver().killed).toBe(true);
  item.notify('must-not-replay');
  await settle();
  expect(item.messages).toHaveLength(3);
  await item.emit('session_shutdown');
});

test('retirement while validating withdraws the receiver and cannot steer a stale native session', async () => {
  const item = fixture('retirement');
  const gate = Promise.withResolvers<Json>();
  item.setValidate(() => gate.promise);
  await item.emit('session_start');
  item.handshake();
  item.notify('in-flight');
  await settle();
  expect(item.requests).toHaveLength(1);
  await item.emit('session_shutdown', { reason: 'fork' });
  expect(item.receiver().killed).toBe(true);
  gate.resolve({ current: true });
  await settle();
  expect(item.messages).toHaveLength(0);
  expect(item.requests.filter(request => request.method === 'wake_outcome')).toHaveLength(0);
});

test('subscription framing and bounded pending capacity fail closed without native replay', async () => {
  for (const failure of ['before-handshake', 'duplicate-handshake', 'wrong-session', 'oversized-frame', 'capacity']) {
    const item = fixture(`bounds-${failure}`);
    await item.emit('session_start');
    if (failure !== 'before-handshake') item.handshake();
    if (failure === 'duplicate-handshake') item.handshake();
    else if (failure === 'wrong-session') item.notify('wrong-session', { session_id: 'unrelated-root' });
    else if (failure === 'oversized-frame') item.receiver().stdout.write(Buffer.alloc(1024 * 1024 + 1, 65));
    else if (failure === 'capacity') {
      const gate = Promise.withResolvers<Json>();
      item.setValidate(() => gate.promise);
      for (let index = 0; index < 65; index++) item.notify(`pending-${index}`);
      gate.resolve({ current: true });
    } else item.notify('not-enrolled');
    await settle();
    expect(item.receiver().killed).toBe(true);
    expect(item.messages).toHaveLength(0);
    await item.emit('session_shutdown');
  }
});
