import { expect, mock, spyOn, test } from 'bun:test';
import * as childProcess from 'node:child_process';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import * as nodeTimers from 'node:timers/promises';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { createNativeCallbacks, parseInboxPointer } from '../shared/native.ts';

type FixtureContext = {
  agent: { kind: string };
  sessionManager: { getSessionId(): string };
  ui: { notify(message: string): void };
};
type Handler = (event: Record<string, unknown>, context: FixtureContext) => Promise<unknown>;

type FixtureChild = EventEmitter & {
  stdin: PassThrough;
  stdout: PassThrough;
  stderr: PassThrough;
  killed: boolean;
  closed: boolean;
  finish(code?: number): void;
  kill(): boolean;
};
type FixtureTimer = { delay: number | undefined; fire(): void };
type Fixture = {
  commands: string[][];
  subscribers: FixtureChild[];
  requests: Record<string, unknown>[];
  callbacks: FixtureChild[];
  facts: { command: string; child: FixtureChild }[];
  messages: { content: string; details: unknown }[];
  warnings: string[];
  timers: Map<unknown, FixtureTimer>;
  callbackResponse?: string;
  holdCallback?: string | true;
  holdFact?: string;
  deferSubscriberClose?: boolean;
  callbackKills: number;
  session: string;
  emit(event: string, value?: Record<string, unknown>, kind?: string): Promise<unknown>;
};

async function withFixture(run: (fixture: Fixture) => Promise<void>): Promise<void> {
  const handlers = new Map<string, Handler>();
  const fixture: Fixture = {
    commands: [], subscribers: [], requests: [], callbacks: [], facts: [], messages: [], warnings: [],
    timers: new Map(), callbackKills: 0, session: 'native-one',
    async emit(event, value = {}, kind = 'main') {
      return handlers.get(event)!(
        value, { agent: { kind }, sessionManager: { getSessionId: () => fixture.session },
          ui: { notify(message: string) { fixture.warnings.push(message); } } },
      );
    },
  };
  const realSpawn = childProcess.spawn;
  const spawnFixture = (program: string, args: string[], options: childProcess.SpawnOptions) => {
    if (program !== '/private/fixture/bkpr') return realSpawn(program, args, options);
    fixture.commands.push(args);
    const command = args[1];
    const child: FixtureChild = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(), killed: false, closed: false,
      finish(code = 0) {
        if (this.closed) return;
        this.closed = true;
        this.emit('close', code);
      },
      kill() {
        this.killed = true;
        if (command === 'agent') fixture.callbackKills += 1;
        if (command !== 'agent-subscribe' || !fixture.deferSubscriberClose) queueMicrotask(() => child.finish());
        return true;
      },
    });
    if (command === 'agent-subscribe') {
      fixture.subscribers.push(child);
      // A same-binding native registration can return the same receiver ID after restart.
      queueMicrotask(() => child.stdout.write('{"receiver_id":"receiver"}\n'));
    } else if (command === 'agent') {
      fixture.callbacks.push(child);
      let input = '';
      child.stdin.on('data', chunk => { input += chunk.toString(); });
      child.stdin.on('end', () => {
        const request = JSON.parse(input);
        fixture.requests.push(request);
        if (fixture.holdCallback === true ||
            (fixture.holdCallback !== undefined && fixture.holdCallback === request.method)) return;
        child.stdout.write(fixture.callbackResponse ?? JSON.stringify(request.method === 'wake_validate' ? { current: true } : {}) + '\n');
        child.finish();
      });
    } else {
      fixture.facts.push({ command, child });
      if (fixture.holdFact !== command) queueMicrotask(() => child.finish());
    }
    return child;
  };
  const spawn = spyOn(childProcess, 'spawn').mockImplementation(spawnFixture as typeof childProcess.spawn);
  const schedule = spyOn(nodeTimers, 'setTimeout').mockImplementation((delay, value, options) =>
    new Promise((resolve, reject) => {
      const signal = options?.signal;
      const abort = () => {
        fixture.timers.delete(timer);
        reject(signal?.reason);
      };
      const timer = { delay, fire() {
        if (!fixture.timers.delete(timer)) return;
        signal?.removeEventListener('abort', abort);
        resolve(value);
      } };
      fixture.timers.set(timer, timer);
      if (signal?.aborted) abort();
      else signal?.addEventListener('abort', abort, { once: true });
    }));
  mock.module('@oh-my-pi/pi-tui', () => ({
    ImageProtocol: { Kitty: '\x1b_G' },
    isImageProtocolForced: () => false,
    setTerminalImageProtocol() {},
  }));
  const launchKey = (key: string) => key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_') ||
    key === 'BKPR_SOCKET' || key === 'BKPR_PANE_ID' || key === 'BKPR_INTEGRATION_FILE';
  const previous = Object.fromEntries(Object.entries(process.env).filter(([key]) => launchKey(key)));
  for (const key of Object.keys(previous)) delete process.env[key];
  process.env.BKPR_CAPTURE_HARNESS = 'omp';
  process.env.BKPR_CAPTURE_PROGRAM = '/private/fixture/bkpr';
  process.env.BKPR_CAPTURE_TOKEN = 'private-fixture-capability';
  process.env.BKPR_CAPTURE_LAUNCH_ID = 'private-fixture-launch';
  process.env.BKPR_PANE_ID = 'private-fixture-pane';
  process.env.BKPR_SOCKET = '/private/fixture/socket';
  const api = {
    on(event: string, handler: Handler) { handlers.set(event, handler); },
    sendMessage(message: { content: string; details: unknown }) { fixture.messages.push(message); },
  };
  try {
    // Load the extension after installing the fake harness package and subprocess boundary.
    const { default: extension } = await import('./index');
    await extension(api as unknown as Parameters<typeof extension>[0]);
    await run(fixture);
  } finally {
    try {
      fixture.deferSubscriberClose = false;
      for (const child of fixture.subscribers) if (child.killed) child.finish();
      if (handlers.has('session_shutdown')) await fixture.emit('session_shutdown');
    } finally {
      for (const key of Object.keys(process.env)) if (launchKey(key)) delete process.env[key];
      Object.assign(process.env, previous);
      schedule.mockRestore();
      spawn.mockRestore();
    }
  }
}

function notify(child: FixtureChild, session: string, generation: number, id = 'notice'): void {
  child.stdout.write(JSON.stringify({ notification: {
    notification_id: id, generation, session_id: session,
    channel_wake: { batch_id: id, channel_id: 'channel', pending_count: 2 },
    text: 'PRIVATE_PEER_BODY',
  } }) + '\n');
}

async function settle(): Promise<void> {
  await nextTurn();
  await nextTurn();
}

test('native root prompt facts and batched wake pointers remain body-free and launch-scoped', async () => withFixture(async fixture => {
  const { commands, requests, messages, warnings } = fixture;
  await fixture.emit('session_start');
  await fixture.emit('before_agent_start', { reason: 'revive', prompt: 'PRIVATE_PROMPT' });
  expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
  await fixture.emit('before_agent_start', { reason: 'prompt', prompt: 'PRIVATE_PROMPT' }, 'subagent');
  expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
  for (let index = 0; index < 2; index += 1) {
    await fixture.emit('before_agent_start', { reason: 'prompt', prompt: 'PRIVATE_PROMPT' });
  }
  expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(1);
  notify(fixture.subscribers[0], 'native-one', 1);
  await settle();
  expect(messages).toHaveLength(1);
  expect(messages[0].content).toBe(JSON.stringify({
    channel_wake: { batch_id: 'notice', channel_id: 'channel', pending_count: 2 },
  }));
  expect(requests.find(request => request.method === 'wake_outcome')).toMatchObject({
    notification_id: 'notice', outcome: { state: 'unknown' },
  });
  const wake = { batch_id: 'batch', channel_id: 'channel', pending_count: 3 };
  fixture.subscribers[0].stdout.write(JSON.stringify({ notification: {
    notification_id: 'channel-notice', generation: 2, session_id: 'native-one',
    channel_wake: wake, text: 'PRIVATE_PEER_BODY', recipients: ['PRIVATE_RECIPIENT'],
  } }) + '\n');
  await settle();
  expect(messages).toHaveLength(2);
  expect(messages[1].content).toBe(JSON.stringify({ channel_wake: wake }));
  expect(requests.slice(-2)).toMatchObject([
    { method: 'wake_validate', notification_id: 'channel-notice', generation: 2 },
    { method: 'wake_outcome', notification_id: 'channel-notice', generation: 2, outcome: { state: 'unknown' } },
  ]);
  fixture.session = 'native-two';
  await fixture.emit('session_switch');
  await fixture.emit('before_agent_start', { reason: 'prompt', prompt: 'PRIVATE_PROMPT' });
  expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(1);
  expect(JSON.stringify({ commands, requests, messages })).not.toContain('PRIVATE_PROMPT');
  expect(warnings).toEqual([]);

  const validPointer = { notification_id: 'notice', generation: 1, session_id: 'native-two', channel_wake: wake };
  expect(parseInboxPointer({ ...validPointer, text: 'PRIVATE_PEER_BODY' })).toEqual(validPointer);
  for (const invalid of [
    null, [], { ...validPointer, notification_id: '\x1bsecret' },
    { ...validPointer, notification_id: 'x'.repeat(513) },
    { ...validPointer, generation: Number.MAX_SAFE_INTEGER + 1 },
    { ...validPointer, session_id: '' },
    { ...validPointer, session_id: '\n' },
    { ...validPointer, channel_wake: undefined },
    { ...validPointer, channel_wake: { ...wake, pending_count: 0 } },
    { ...validPointer, channel_wake: null },
    { ...validPointer, channel_wake: { ...wake, pending_count: 0x100000000 } },
    { ...validPointer, channel_wake: { ...wake, addressed_message_id: '\x1bsecret' } },
  ]) expect(() => parseInboxPointer(invalid)).toThrow();

  process.env.BKPR_CAPTURE_HARNESS = 'omp';
  process.env.BKPR_CAPTURE_PROGRAM = '/private/fixture/bkpr';
  process.env.BKPR_CAPTURE_TOKEN = 'private-fixture-capability';
  expect(createNativeCallbacks('opencode')).toBeUndefined();
  expect(process.env.BKPR_CAPTURE_TOKEN).toBe('private-fixture-capability');
  const native = createNativeCallbacks('omp')!;
  expect(native.environment.BKPR_CAPTURE_TOKEN).toBe('private-fixture-capability');
  expect(process.env.BKPR_CAPTURE_TOKEN).toBeUndefined();
  const beforeDispatch = commands.length;
  await expect(native.request({ text: 'x'.repeat(1024 * 1024) })).rejects.toThrow('request exceeds limit');
  const cancelled = new AbortController();
  cancelled.abort();
  await expect(native.request({}, cancelled.signal)).rejects.toThrow('cancelled before dispatch');
  expect(commands).toHaveLength(beforeDispatch);
  for (const response of ['{}', '{}\n{}\n', '{}\n\n', '[]\n', 'not-json\n']) {
    fixture.callbackResponse = response;
    await expect(native.request({})).rejects.toThrow('invalid native callback response');
  }
  fixture.callbackResponse = 'x'.repeat(8 * 1024 * 1024 + 1);
  const killsBeforeLimit = fixture.callbackKills;
  await expect(native.request({})).rejects.toThrow('response exceeds limit');
  expect(fixture.callbackKills).toBe(killsBeforeLimit + 1);
  fixture.holdCallback = true;
  const dispatched = new AbortController();
  const pending = native.request({}, dispatched.signal);
  const killsBeforeAbort = fixture.callbackKills;
  dispatched.abort();
  await expect(pending).rejects.toThrow('mutation outcome unknown, do not replay');
  expect(fixture.callbackKills).toBe(killsBeforeAbort + 1);
}));

test('unexpected exit restarts the same binding without replaying claims or asides', async () => withFixture(async fixture => {
  const { commands, requests, subscribers, timers, messages } = fixture;
  await fixture.emit('session_start');
  const original = subscribers[0];
  notify(original, 'native-one', 1, 'offered');
  await settle();
  expect(messages).toHaveLength(1);

  fixture.holdCallback = 'wake_validate';
  notify(original, 'native-one', 2, 'validating');
  await settle();
  const validating = fixture.callbacks.at(-1)!;
  expect(requests.at(-1)).toMatchObject({ method: 'wake_validate', notification_id: 'validating' });

  original.emit('exit', 1);
  notify(original, 'native-one', 4, 'old-pipe');
  await settle();
  expect(validating.killed).toBe(true);
  expect(timers.size).toBe(0);
  original.finish(1);
  notify(original, 'native-one', 5, 'old-pipe');
  await fixture.emit('turn_end');
  expect(subscribers).toHaveLength(1);
  const [retry] = timers.values();
  expect(retry.delay).toBe(250);
  fixture.holdCallback = undefined;
  retry.fire();
  await settle();

  expect(commands.filter(args => args[1] === 'bind')).toEqual([[
    'capture', 'bind', '--source', 'omp_extension', '--session-id', 'native-one',
  ]]);
  expect(commands.filter(args => args[1] === 'agent-subscribe')).toEqual([
    ['capture', 'agent-subscribe', 'native-one'], ['capture', 'agent-subscribe', 'native-one'],
  ]);
  original.stdout.write('{"receiver_id":"stale-receiver"}\n');
  notify(original, 'native-one', 6, 'retired-process');
  validating.stdout.write('{"current":true}\n');
  notify(subscribers[1], 'native-one', 1, 'offered');
  notify(subscribers[1], 'native-one', 2, 'validating');
  notify(subscribers[1], 'foreign-session', 4, 'foreign');
  notify(subscribers[1], 'native-one', 3, 'fresh');
  notify(subscribers[1], 'native-one', 3, 'fresh');
  await settle();
  expect(requests.map(request => [request.method, request.notification_id])).toEqual([
    ['wake_validate', 'offered'], ['wake_outcome', 'offered'], ['wake_validate', 'validating'],
    ['wake_validate', 'fresh'], ['wake_outcome', 'fresh'],
  ]);
  expect(requests.at(-1)).toMatchObject({
    receiver_id: 'receiver', generation: 3, outcome: { state: 'unknown' },
  });
  expect(messages.map(message => message.details)).toEqual([{ id: 'offered', generation: 1 }, { id: 'fresh', generation: 3 }]);
}));

test('fail-fast subscribers stop at the retry cap and only a real prompt rearms the same binding', async () => withFixture(async fixture => {
  const { commands, subscribers, timers, warnings } = fixture;
  await fixture.emit('session_start');
  await fixture.emit('before_agent_start', { reason: 'prompt' });
  subscribers[0].emit('error', new Error('fixture spawn error'));
  expect(timers.size).toBe(0);
  subscribers[0].finish(1);
  await fixture.emit('before_agent_start', { reason: 'prompt' });
  expect(subscribers).toHaveLength(1);
  for (const delay of [250, 500, 1000, 2000, 4000]) {
    expect([...timers.values()].map(timer => timer.delay)).toEqual([delay]);
    const [retry] = timers.values();
    retry.fire();
    await settle(); // Even a successful handshake is not permission for an unbounded restart loop.
    subscribers.at(-1)!.finish(1);
  }
  expect(subscribers).toHaveLength(6);
  expect(timers.size).toBe(0);
  expect(warnings).toHaveLength(2);
  expect(warnings[1]).toContain('recovery exhausted');
  await fixture.emit('turn_end');
  await fixture.emit('before_agent_start', { reason: 'revive' });
  await fixture.emit('before_agent_start', { reason: 'prompt' }, 'subagent');
  expect(subscribers).toHaveLength(6);
  await fixture.emit('before_agent_start', { reason: 'prompt' });
  expect(subscribers).toHaveLength(7);
  expect(commands.filter(args => args[1] === 'bind')).toHaveLength(1);
  expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(1);
  subscribers.at(-1)!.finish(1);
  expect([...timers.values()].map(timer => timer.delay)).toEqual([250]);
}));

test('rebind and shutdown discard timers, drain the prior child, and cancel ambiguous outcomes', async () => withFixture(async fixture => {
  const { subscribers, requests, timers, messages } = fixture;
  await fixture.emit('session_start');
  subscribers[0].finish(1);
  const [oldRetry] = timers.values();
  fixture.session = 'native-two';
  await fixture.emit('session_switch');
  expect(timers.size).toBe(0);
  oldRetry.fire();
  expect(subscribers).toHaveLength(2);

  fixture.holdCallback = 'wake_outcome';
  const second = subscribers[1];
  notify(second, 'native-two', 1, 'ambiguous');
  await settle();
  const outcome = fixture.callbacks.at(-1)!;
  expect(requests.at(-1)).toMatchObject({ method: 'wake_outcome', outcome: { state: 'unknown' } });
  expect(messages).toHaveLength(1);
  fixture.deferSubscriberClose = true;
  fixture.session = 'native-three';
  const switching = fixture.emit('session_branch');
  notify(second, 'native-three', 2, 'stale-new-session');
  await settle();
  expect(second.killed).toBe(true);
  expect(outcome.killed).toBe(true);
  expect(subscribers).toHaveLength(2);
  expect(messages).toHaveLength(1);
  second.finish();
  await switching;
  expect(subscribers).toHaveLength(3);
  expect(timers.size).toBe(0);

  subscribers[2].finish(1);
  const [shutdownRetry] = timers.values();
  await fixture.emit('session_shutdown');
  expect(timers.size).toBe(0);
  shutdownRetry.fire();
  await fixture.emit('turn_end');
  expect(subscribers).toHaveLength(3);
  expect(requests).toHaveLength(2);
}));

test('rebind cancels a pending old bind and shutdown cancels the active child and prompt fact', async () => withFixture(async fixture => {
  const { facts, subscribers, timers, warnings } = fixture;
  fixture.holdFact = 'bind';
  const starting = fixture.emit('session_start');
  await settle();
  expect(subscribers).toHaveLength(0);
  expect(facts.map(fact => fact.command)).toEqual(['bind']);
  fixture.holdFact = undefined;
  fixture.session = 'native-two';
  await fixture.emit('session_switch');
  await starting;
  expect(facts[0].child.killed).toBe(true);
  expect(subscribers).toHaveLength(1);

  fixture.holdFact = 'prompt-submitted';
  const prompting = fixture.emit('before_agent_start', { reason: 'prompt' });
  await settle();
  expect(facts.at(-1)?.command).toBe('prompt-submitted');
  await fixture.emit('session_shutdown');
  await prompting;
  expect(subscribers[0].killed).toBe(true);
  expect(facts.at(-1)?.child.killed).toBe(true);
  expect(timers.size).toBe(0);
  expect(warnings).toEqual([]);
}));
