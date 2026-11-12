import { expect, mock, test } from 'bun:test';
import type { Hooks } from '@opencode-ai/plugin';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setImmediate as nextTurn } from 'node:timers/promises';

type Json = Record<string, unknown>;
type Child = EventEmitter & { stdin: PassThrough; stdout: PassThrough; stderr: PassThrough; killed: boolean; kill(): boolean };
type Slot = (context: unknown, props: { session_id: string }) => null;

// Run in its own Bun invocation: process mocking is local to this native SDK fixture.
test('selected root epochs, typed status, and ambiguous native asynchronous dispatch govern capture', async () => {
  const children: { args: string[]; child: Child }[] = [];
  const requests: Json[] = [];
  const statuses: { session: string; value: Json }[] = [];
  const prompts: Json[] = [];
  const warnings: string[] = [];
  let holdBind = true;
  let holdValidate: (() => void) | undefined;
  let interceptValidate = false;
  let failOutcome = false;
  let holdPromptFact = false;
  const spawn = (_program: string, args: string[]) => {
    const child: Child = Object.assign(new EventEmitter(), {
      stdin: new PassThrough(), stdout: new PassThrough(), stderr: new PassThrough(), killed: false,
      kill() { this.killed = true; queueMicrotask(() => child.emit('close', 0)); return true; },
    });
    children.push({ args, child });
    if (args[1] === 'agent-subscribe') {
      queueMicrotask(() => child.stdout.write('{"receiver_id":"receiver-' + children.length + '"}\n'));
    } else if (args[1] === 'status') {
      queueMicrotask(() => child.stdout.write('{"generation":1}\n'));
      let pending = '';
      child.stdin.on('data', chunk => {
        pending += chunk.toString();
        let newline: number;
        while ((newline = pending.indexOf('\n')) >= 0) {
          const line = pending.slice(0, newline);
          pending = pending.slice(newline + 1);
          statuses.push({ session: args[args.indexOf('--session-id') + 1], value: JSON.parse(line) });
          child.stdout.write('{"accepted":true,"generation":1}\n');
        }
      });
    } else if (args[1] === 'agent') {
      let input = '';
      child.stdin.on('data', chunk => { input += chunk.toString(); });
      child.stdin.on('end', () => {
        const request = JSON.parse(input);
        requests.push(request);
        const reply = () => {
          child.stdout.write(failOutcome && request.method === 'wake_outcome' ? 'not-json\n' :
            JSON.stringify(request.method === 'wake_validate' ? { current: true } : {}) + '\n');
          child.emit('close', 0);
        };
        if (interceptValidate && request.method === 'wake_validate') holdValidate = reply;
        else reply();
      });
    } else if (!(holdBind && args[1] === 'bind') && !(holdPromptFact && args[1] === 'prompt-submitted')) {
      queueMicrotask(() => child.emit('close', args[1] === 'prompt-submitted' && args.at(-1) === 'child' ? 1 : 0));
    }
    return child;
  };
  mock.module('node:child_process', () => ({ spawn }));
  // The adapter and real shared helper must resolve spawn only after this process-boundary mock is installed.
  const { default: plugin } = await import('./index');
  const { default: serverPlugin } = await import('./server');
  const directory = await mkdtemp(join(tmpdir(), 'bkpr-opencode-'));
  const captureFile = join(directory, 'capture.json');
  await writeFile(captureFile, JSON.stringify({ grant: {
    launch_id: 'launch', token: 'PRIVATE_CAPTURE_TOKEN', pane_id: 'pane', kind: 'opencode',
  }, program: '/private/bkpr', socket: '/private/socket' }), { mode: 0o600 });
  const handlers = new Map<string, (event: { properties: Json }) => void>();
  let slots: Record<string, Slot> = {};
  let dispose: (() => void) | undefined;
  const serverDisposers: (() => Promise<void>)[] = [];
  const serverWarnings: string[] = [];
  const serverInput = { client: { tui: { showToast: async ({ body }: { body: { message: string } }) => {
    serverWarnings.push(body.message);
  } } } } as unknown as Parameters<typeof serverPlugin.server>[0];
  let route: { name: string; params?: { sessionID: string } } = { name: 'home' };
  const sessions: Record<string, { id: string; parentID?: string }> = {
    'root-a': { id: 'root-a' }, 'root-b': { id: 'root-b' }, child: { id: 'child', parentID: 'root-a' },
  };
  let nativeStatus = { type: 'idle' };
  let permissions: { sessionID: string }[] = [];
  let questions: { sessionID: string }[] = [];
  let promptBehavior: 'void' | 'throw' = 'void';
  const api = {
    route: { get current() { return route; } },
    state: { session: {
      get: (id: string) => sessions[id], status: () => nativeStatus,
      permission: () => permissions, question: () => questions,
    } },
    ui: { toast: (input: { message: string }) => { warnings.push(input.message); } },
    event: { on(type: string, handler: (event: { properties: Json }) => void) {
      handlers.set(type, handler); return () => { handlers.delete(type); };
    } },
    slots: { register(input: { slots: Record<string, Slot> }) { slots = input.slots; return 'bkpr'; } },
    lifecycle: { onDispose(callback: () => void) { dispose = callback; } },
    client: { session: { async promptAsync(input: { sessionID: string; parts: { type: 'text'; text: string }[] }) {
      prompts.push(input);
      if (promptBehavior === 'throw') throw new Error('PRIVATE_CAPTURE_TOKEN provider failure');
      return { response: new Response(null, { status: 204 }), data: undefined };
    } } },
  };
  const emit = (type: string, properties: Json) => handlers.get(type)?.({ properties });
  const settle = async () => { await nextTurn(); await nextTurn(); };
  const observe = (id: string) => {
    route = { name: 'session', params: { sessionID: id } };
    expect(slots.app({}, { session_id: id })).toBeNull();
    expect(slots.session_prompt_right({}, { session_id: id })).toBeNull();
  };
  const receiver = () => children.findLast(item => item.args[1] === 'agent-subscribe')!.child;
  const notify = (id: string, session = 'root-a') => receiver().stdout.write(JSON.stringify({ notification: {
    notification_id: id, generation: 1, session_id: session, message_count: 1, unread_count: 2,
    body: 'PRIVATE_PEER_BODY',
  } }) + '\n');
  try {
    await plugin.tui(api as unknown as Parameters<typeof plugin.tui>[0], { captureFile });
    expect(handlers.has('session.next.prompt.admitted')).toBe(false);
    expect(handlers.has('message.updated')).toBe(false);
    observe('child');
    await settle();
    expect(children).toHaveLength(0);
    permissions = [{ sessionID: 'root-a' }];
    route = { name: 'session', params: { sessionID: 'root-a' } };
    // A blocked selected root has no session_prompt_right subtree: app observation alone must enroll it.
    expect(slots.app({}, { session_id: '' })).toBeNull();
    await settle();
    expect(children.filter(item => item.args[1] === 'bind')).toHaveLength(1);
    expect(children.filter(item => item.args[1] === 'prompt-submitted')).toHaveLength(0);
    holdBind = false;
    children.find(item => item.args[1] === 'bind')!.child.emit('close', 0);
    await settle();
    expect(children.filter(item => item.args[1] === 'prompt-submitted')).toHaveLength(0);
    const serverHooks: Hooks = await serverPlugin.server(serverInput, { captureFile });
    serverDisposers.push(serverHooks.dispose!);
    const untouched = { message: { private: 'PRIVATE_USER_PROMPT' }, parts: [{ text: 'PRIVATE_USER_PROMPT' }] };
    await serverHooks['chat.message']!({ sessionID: 'child' }, untouched as never);
    await settle();
    await serverHooks['chat.message']!({ sessionID: 'root-a' }, untouched as never);
    await settle();
    expect(children.filter(item => item.args[1] === 'prompt-submitted' && item.args.at(-1) === 'root-a')).toHaveLength(1);
    expect(untouched).toEqual({ message: { private: 'PRIVATE_USER_PROMPT' }, parts: [{ text: 'PRIVATE_USER_PROMPT' }] });
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'blocked', kind: 'permission' });
    permissions = [];
    emit('permission.replied', { sessionID: 'root-a', requestID: 'initial-permission' });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'idle' });

    emit('session.status', { sessionID: 'child', status: { type: 'busy' } });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'idle' });
    nativeStatus = { type: 'busy' };
    emit('session.status', { sessionID: 'root-a', status: nativeStatus });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'working' });
    permissions = [{ sessionID: 'root-a' }];
    emit('permission.asked', { sessionID: 'root-a', id: 'permission' });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'blocked', kind: 'permission' });
    permissions = [];
    questions = [{ sessionID: 'root-a' }];
    emit('question.asked', { sessionID: 'root-a', id: 'question' });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'blocked', kind: 'question' });
    questions = [];
    nativeStatus = { type: 'idle' };
    emit('session.status', { sessionID: 'root-a', status: nativeStatus });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'done' });
    emit('session.error', { sessionID: 'root-a', error: { name: 'ProviderAuthError' } });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'blocked', kind: 'auth' });
    emit('session.error', { sessionID: 'root-a', error: { name: 'UnknownError' } });
    await settle();
    expect(statuses.at(-1)?.value).toMatchObject({ state: 'error' });

    notify('offered'); notify('offered');
    await settle();
    expect(prompts).toHaveLength(1);
    expect(prompts[0]).toMatchObject({ sessionID: 'root-a', parts: [{ type: 'text' }] });
    expect(requests.find(item => item.notification_id === 'offered' && item.method === 'wake_outcome'))
      .toMatchObject({ outcome: { state: 'unknown' } });
    expect(requests.some(item => (item.outcome as Json | undefined)?.state === 'accepted')).toBe(false);
    promptBehavior = 'throw';
    notify('ambiguous'); notify('ambiguous');
    await settle();
    expect(prompts).toHaveLength(2);
    expect(requests.find(item => item.notification_id === 'ambiguous' && item.method === 'wake_outcome'))
      .toMatchObject({ outcome: { state: 'unknown' } });

    interceptValidate = true;
    const oldReceiver = receiver();
    const oldStatus = children.findLast(item => item.args[1] === 'status')!.child;
    notify('retired-during-validation');
    await settle();
    expect(holdValidate).toBeDefined();
    observe('root-b');
    holdValidate!();
    await settle();
    expect(oldReceiver.killed).toBe(true);
    expect(oldStatus.killed).toBe(true);
    expect(prompts).toHaveLength(2);
    const statusCount = statuses.length;
    emit('session.error', { sessionID: 'root-a', error: { name: 'UnknownError' } });
    await settle();
    expect(statuses).toHaveLength(statusCount);
    observe('root-a');
    await settle();
    expect(children.filter(item => item.args[1] === 'bind')).toHaveLength(3);
    const latestReceiver = receiver();
    route = { name: 'home' };
    slots.app({}, { session_id: '' });
    await settle();
    expect(latestReceiver.killed).toBe(true);
    observe('root-b');
    await settle();
    const deletedReceiver = receiver();
    delete sessions['root-b'];
    emit('session.deleted', { sessionID: 'root-b' });
    await settle();
    expect(deletedReceiver.killed).toBe(true);
    const beforeMissingRoot = children.length;
    slots.session_prompt_right({}, { session_id: 'root-b' });
    await settle();
    expect(children).toHaveLength(beforeMissingRoot);
    observe('root-a');
    await settle();
    interceptValidate = false;
    failOutcome = true;
    promptBehavior = 'void';
    const stoppedReceiver = receiver();
    const beforeOutcomeFailure = prompts.length;
    notify('unrecorded-outcome'); notify('must-not-dispatch');
    await settle();
    expect(stoppedReceiver.killed).toBe(true);
    expect(prompts).toHaveLength(beforeOutcomeFailure + 1);
    expect(warnings).toEqual(['Native wake outcome could not be recorded; receiver stopped to prevent ambiguous replay.']);
    route = { name: 'home' };
    slots.app({}, { session_id: '' });
    holdBind = true;
    observe('root-a');
    await settle();
    const pendingBind = children.findLast(item => item.args[1] === 'bind')!;
    const pendingBindCount = children.filter(item => item.args[1] === 'bind').length;
    sessions['root-b'] = { id: 'root-b' };
    observe('root-b');
    await settle();
    expect(pendingBind.child.killed).toBe(false);
    expect(children.filter(item => item.args[1] === 'bind')).toHaveLength(pendingBindCount);
    holdBind = false;
    pendingBind.child.emit('close', 0);
    await settle();
    expect(children.filter(item => item.args[1] === 'bind')).toHaveLength(pendingBindCount + 1);
    expect(children.findLast(item => item.args[1] === 'bind')!.args.at(-1)).toBe('root-b');
    const disposedReceiver = receiver();
    dispose!();
    await settle();
    expect(disposedReceiver.killed).toBe(true);
    expect(handlers.size).toBe(0);
    await plugin.tui(api as unknown as Parameters<typeof plugin.tui>[0], { captureFile });
    holdBind = true;
    observe('root-a');
    await settle();
    const unknownBind = children.findLast(item => item.args[1] === 'bind')!;
    unknownBind.child.emit('close', 1);
    await settle();
    const afterUnknownBind = children.filter(item => item.args[1] === 'bind').length;
    observe('root-b');
    await settle();
    expect(children.filter(item => item.args[1] === 'bind')).toHaveLength(afterUnknownBind);
    expect(warnings.at(-1)).toBe('Native binding outcome is unknown; further conversation binding is disabled for this launch.');
    expect(JSON.stringify({ requests, prompts, warnings })).not.toContain('PRIVATE_CAPTURE_TOKEN');
    expect(JSON.stringify({ requests, prompts, warnings })).not.toContain('PRIVATE_PEER_BODY');
    expect(JSON.stringify({ requests, prompts, warnings })).not.toContain('PRIVATE_USER_PROMPT');
    expect(warnings).toHaveLength(2);
    const boundedHooks: Hooks = await serverPlugin.server(serverInput, { captureFile });
    serverDisposers.push(boundedHooks.dispose!);
    holdPromptFact = true;
    const beforeBurst = children.filter(item => item.args[1] === 'prompt-submitted').length;
    for (let index = 0; index < 40; index += 1) {
      await boundedHooks['chat.message']!({ sessionID: 'root-a' }, {} as never);
    }
    await settle();
    expect(children.filter(item => item.args[1] === 'prompt-submitted')).toHaveLength(beforeBurst + 32);
    const pendingFacts = children.filter(item => item.args[1] === 'prompt-submitted').slice(-32);
    await boundedHooks.dispose!();
    expect(pendingFacts.every(item => item.child.killed)).toBe(true);
  } finally {
    dispose?.();
    for (const close of serverDisposers) await close();
    expect(handlers.size).toBe(0);
    await rm(directory, { recursive: true, force: true });
    mock.restore();
  }
});
