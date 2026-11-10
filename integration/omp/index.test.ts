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
  const spawn = (_program: string, args: string[]) => {
    commands.push(args);
    const child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(),
      kill() { queueMicrotask(() => child.emit('close', 0)); return true; },
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
        child.stdout.write(JSON.stringify(request.method === 'wake_validate' ? { current: true } : {}) + '\n');
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
    .filter(([key]) => key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_')));
  process.env.BKPR_CAPTURE_HARNESS = 'omp';
  process.env.BKPR_CAPTURE_PROGRAM = '/private/fixture/bkpr';
  process.env.BKPR_CAPTURE_TOKEN = 'private-fixture-capability';
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
    extension(host);
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
  } finally {
    await handlers.get('session_shutdown')?.({}, context);
    for (const key of Object.keys(process.env)) {
      if (key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_')) delete process.env[key];
    }
    Object.assign(process.env, previous);
    mock.restore();
  }
});
