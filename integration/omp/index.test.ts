import { expect, mock, test } from 'bun:test';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { setImmediate as nextTurn } from 'node:timers/promises';

type FixtureContext = {
  agent: { kind: string };
  sessionManager: { getSessionId(): string };
  ui: { notify(message: string): void };
};
type Handler = (event: Record<string, unknown>, context: FixtureContext) => Promise<unknown>;

test('native root prompt facts and batched wake pointers remain body-free and launch-scoped', async () => {
  const commands: string[][] = [];
  const requests: Record<string, unknown>[] = [];
  const messages: { content: string; details: unknown }[] = [];
  const warnings: string[] = [];
  let subscriber: PassThrough | undefined;
  let callbackResponse: string | undefined;
  let holdCallback = false;
  let callbackKills = 0;
  const spawn = (_program: string, args: string[]) => {
    commands.push(args);
    const child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(),
      kill() { callbackKills += 1; queueMicrotask(() => child.emit('close', 0)); return true; },
    });
    if (args[1] === 'agent-subscribe') {
      subscriber = child.stdout;
      queueMicrotask(() => child.stdout.write('{"receiver_id":"receiver"}\n'));
    } else if (args[1] === 'agent') {
      let input = '';
      child.stdin.on('data', chunk => { input += chunk.toString(); });
      child.stdin.on('end', () => {
        const request = JSON.parse(input);
        requests.push(request);
        if (holdCallback) return;
        child.stdout.write(callbackResponse ?? JSON.stringify(request.method === 'wake_validate' ? { current: true } : {}) + '\n');
        child.emit('close', 0);
      });
    } else {
      queueMicrotask(() => child.emit('close', 0));
    }
    return child;
  };
  mock.module('node:child_process', () => ({ spawn }));
  mock.module('@oh-my-pi/pi-tui', () => ({
    ImageProtocol: { Kitty: '\x1b_G' },
    isImageProtocolForced: () => false,
    setTerminalImageProtocol() {},
  }));
  // Import after mocking: this fixture intentionally exercises the module's native subprocess boundary.
  const { default: extension } = await import('./index');
  const previous = Object.fromEntries(Object.entries(process.env)
    .filter(([key]) => key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_') || key === 'BKPR_SOCKET' || key === 'BKPR_PANE_ID' || key === 'BKPR_INTEGRATION_FILE'));
  delete process.env.BKPR_INTEGRATION_FILE;
  process.env.BKPR_CAPTURE_HARNESS = 'omp';
  process.env.BKPR_CAPTURE_PROGRAM = '/private/fixture/bkpr';
  process.env.BKPR_CAPTURE_TOKEN = 'private-fixture-capability';
  process.env.BKPR_CAPTURE_LAUNCH_ID = 'private-fixture-launch';
  process.env.BKPR_PANE_ID = 'private-fixture-pane';
  process.env.BKPR_SOCKET = '/private/fixture/socket';
  const handlers = new Map<string, Handler>();
  let session = 'native-one';
  const context = {
    agent: { kind: 'main' }, sessionManager: { getSessionId: () => session },
    ui: { notify(message: string) { warnings.push(message); } },
  };
  const api = {
    on(event: string, handler: Handler) { handlers.set(event, handler); },
    sendMessage(message: { content: string; details: unknown }) { messages.push(message); },
  };
  try {
    // Deliberately partial fake host: only the native surfaces this integration consumes.
    const host = api as unknown as Parameters<typeof extension>[0];
    await extension(host);
    await handlers.get('session_start')!({}, context);
    await handlers.get('before_agent_start')!({ reason: 'revive', prompt: 'PRIVATE_PROMPT' }, context);
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
    await handlers.get('before_agent_start')!({ reason: 'prompt', prompt: 'PRIVATE_PROMPT' }, { ...context, agent: { kind: 'subagent' } });
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(0);
    for (let index = 0; index < 2; index += 1) {
      await handlers.get('before_agent_start')!({ reason: 'prompt', prompt: 'PRIVATE_PROMPT' }, context);
    }
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(1);
    subscriber!.write(JSON.stringify({ notification: {
      notification_id: 'notice', generation: 1, session_id: 'native-one',
      message_count: 2, unread_count: 3, text: 'PRIVATE_PEER_BODY',
    } }) + '\n');
    await nextTurn();
    await nextTurn();
    expect(messages).toHaveLength(1);
    expect(messages[0].content).not.toContain('PRIVATE_PEER_BODY');
    expect(requests.find(request => request.method === 'wake_outcome')).toMatchObject({
      notification_id: 'notice', outcome: { state: 'unknown' },
    });
    session = 'native-two';
    await handlers.get('session_switch')!({}, context);
    await handlers.get('before_agent_start')!({ reason: 'prompt', prompt: 'PRIVATE_PROMPT' }, context);
    expect(commands.filter(args => args[1] === 'prompt-submitted')).toHaveLength(1);
    expect(JSON.stringify({ commands, requests, messages })).not.toContain('PRIVATE_PROMPT');
    expect(warnings).toEqual([]);

    // Import after mocking to keep the shared helper on the same native subprocess boundary.
    const { createNativeCallbacks, parseInboxPointer } = await import('../shared/native.ts');
    const validPointer = { notification_id: 'notice', generation: 1, session_id: 'native-two', message_count: 2, unread_count: 3 };
    expect(parseInboxPointer({ ...validPointer, text: 'PRIVATE_PEER_BODY' })).toEqual(validPointer);
    for (const invalid of [
      null, [], { ...validPointer, notification_id: '\x1bsecret' },
      { ...validPointer, notification_id: 'x'.repeat(513) },
      { ...validPointer, generation: Number.MAX_SAFE_INTEGER + 1 },
      { ...validPointer, session_id: '' },
      { ...validPointer, session_id: '\n' }, { ...validPointer, message_count: 0 },
      { ...validPointer, unread_count: 1 },
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
      callbackResponse = response;
      await expect(native.request({})).rejects.toThrow('invalid native callback response');
    }
    callbackResponse = 'x'.repeat(8 * 1024 * 1024 + 1);
    const killsBeforeLimit = callbackKills;
    await expect(native.request({})).rejects.toThrow('response exceeds limit');
    expect(callbackKills).toBe(killsBeforeLimit + 1);
    holdCallback = true;
    const dispatched = new AbortController();
    const pending = native.request({}, dispatched.signal);
    const killsBeforeAbort = callbackKills;
    dispatched.abort();
    await expect(pending).rejects.toThrow('mutation outcome unknown, do not replay');
    expect(callbackKills).toBe(killsBeforeAbort + 1);
  } finally {
    await handlers.get('session_shutdown')?.({}, context);
    for (const key of Object.keys(process.env)) {
      if (key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_') || key === 'BKPR_SOCKET' || key === 'BKPR_PANE_ID' || key === 'BKPR_INTEGRATION_FILE') delete process.env[key];
    }
    Object.assign(process.env, previous);
    mock.restore();
  }
});
