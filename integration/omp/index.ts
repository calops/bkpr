import type { ExtensionAPI, ExtensionContext } from '@oh-my-pi/pi-coding-agent';
import { ImageProtocol, isImageProtocolForced, setTerminalImageProtocol } from '@oh-my-pi/pi-tui';
import { spawn, type ChildProcess } from 'node:child_process';
import { setTimeout as delayRetry } from 'node:timers/promises';
import { formatChannelWake, optionalNativeCallbacks, parseInboxPointer, type InboxPointer, type Json } from '../shared/native.ts';

const MAX_FRAME = 1024 * 1024;
const RECEIVER_RETRY_MS = [250, 500, 1000, 2000, 4000] as const;

type Conversation = {
  session: string;
  lifetime: AbortController;
  bound: boolean;
  retries: number;
  retrying: boolean;
  lastSeenGeneration: number;
};

type Receiver = {
  child: ChildProcess;
  lifetime: AbortController;
  closed: Promise<void>;
};

function object(value: unknown): Json {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('expected JSON object');
  return value as Json;
}

/** Native binding and step-boundary asides; shared tools live in MCP. */
export default async function bkpr(pi: ExtensionAPI): Promise<void> {
  const activation = await optionalNativeCallbacks('omp');
  if (!activation) return;
  const { program, environment: launchEnvironment, request, fact } = activation.native;
  if (!isImageProtocolForced()) setTerminalImageProtocol(ImageProtocol.Kitty);
  let conversation: Conversation | undefined;
  let warnedSessionId: string | undefined;
  let receiver: Receiver | undefined;
  let disposed = false;
  let promptObserved = false;

  const current = (selection: Conversation, ctx: ExtensionContext): boolean =>
    conversation === selection && !selection.lifetime.signal.aborted && ctx.sessionManager.getSessionId() === selection.session;

  const stop = (): void => {
    conversation?.lifetime.abort();
    if (receiver && !receiver.lifetime.signal.aborted) {
      receiver.lifetime.abort();
      receiver.child.kill('SIGKILL');
    }
  };

  const notice = async (pointer: InboxPointer, selection: Conversation, id: string, ctx: ExtensionContext, signal: AbortSignal): Promise<void> => {
    if (signal.aborted || !current(selection, ctx)) return;
    const claim = { receiver_id: id, notification_id: pointer.notification_id, generation: pointer.generation };
    const valid = await request({ method: 'wake_validate', ...claim }, signal);
    if (valid.current !== true || signal.aborted || !current(selection, ctx)) return;
    let outcome: Json;
    try {
      pi.sendMessage({
        customType: 'bkpr-message', attribution: 'agent',
        content: formatChannelWake(pointer.channel_wake),
        display: true, details: { id: pointer.notification_id, generation: pointer.generation },
      }, { deliverAs: 'aside', triggerTurn: true });
      // Omp's void API offers no receipt from the asynchronous native queue.
      outcome = { state: 'unknown', reason: 'Omp aside offered; native queue acceptance is unobservable through sendMessage' };
    } catch {
      outcome = { state: 'unknown', reason: 'Omp aside dispatch threw after native acceptance became ambiguous; no replay' };
    }
    await request({ method: 'wake_outcome', ...claim, outcome }, signal);
  };
  const retryReceiver = (selection: Conversation, ctx: ExtensionContext, reason: string): void => {
    if (!current(selection, ctx)) return;
    const delay = RECEIVER_RETRY_MS[selection.retries];
    if (delay === undefined) {
      ctx.ui.notify('Bkpr receiver recovery exhausted; submit a prompt, switch native conversations, or relaunch Bkpr', 'warning');
      return;
    }
    if (selection.retries++ === 0) {
      ctx.ui.notify(`Bkpr receiver stopped (${reason}); reconnecting without replaying native wakes`, 'warning');
    }
    selection.retrying = true;
    void delayRetry(delay, undefined, { signal: selection.lifetime.signal, ref: false }).then(() => {
      selection.retrying = false;
      startReceiver(selection, ctx);
    }, () => {
      selection.retrying = false;
    });
  };

  const startReceiver = (selection: Conversation, ctx: ExtensionContext): void => {
    if (!selection.bound || !current(selection, ctx) || receiver) return;
    let child: ChildProcess;
    try {
      child = spawn(program, ['capture', 'agent-subscribe', selection.session], { env: launchEnvironment, stdio: ['ignore', 'pipe', 'pipe'] });
    } catch {
      retryReceiver(selection, ctx, 'subscriber could not start');
      return;
    }
    const lifetime = new AbortController();
    const { promise: closed, resolve } = Promise.withResolvers<void>();
    receiver = { child, lifetime, closed };
    const { signal } = lifetime;
    let receiverId: string | undefined;
    let delivery = Promise.resolve();
    let pending = Buffer.alloc(0);
    let reason = 'subscriber exited';
    const fail = (failure: string): void => {
      reason = failure;
      lifetime.abort();
      child.kill('SIGKILL');
    };
    child.stderr!.resume(); // Keep pipe drainage independent of user-visible diagnostics.
    child.stdout!.on('data', (chunk: Buffer) => {
      if (signal.aborted) return;
      if (!current(selection, ctx)) { void bind(ctx); return; }
      pending = Buffer.concat([pending, chunk]);
      if (pending.length > MAX_FRAME) { fail('notification frame exceeds limit'); return; }
      let newline: number;
      while ((newline = pending.indexOf(10)) >= 0) {
        const line = pending.subarray(0, newline);
        pending = pending.subarray(newline + 1);
        try {
          const event = object(JSON.parse(line.toString('utf8')));
          if (!receiverId && typeof event.receiver_id === 'string' && event.receiver_id) { receiverId = event.receiver_id; continue; }
          if (!receiverId || !event.notification) throw new Error('notification without receiver handshake');
          const id = receiverId;
          const message = parseInboxPointer(event.notification);
          if (message.session_id !== selection.session || message.generation <= selection.lastSeenGeneration) continue;
          // Native generations increase across receiver registrations, even when the receiver ID is reused.
          selection.lastSeenGeneration = message.generation;
          delivery = delivery.then(() => notice(message, selection, id, ctx, signal)).catch(error => {
            if (!signal.aborted && current(selection, ctx)) ctx.ui.notify(`Bkpr native notification: ${String(error).slice(0, 512)}`, 'warning');
          });
        } catch { fail('notification framing failed'); return; }
      }
    });
    child.on('error', () => {
      if (signal.aborted) return;
      reason = 'subscriber process failed';
      lifetime.abort();
    });
    child.once('exit', () => lifetime.abort());
    child.once('close', () => {
      lifetime.abort();
      receiver = undefined;
      resolve();
      retryReceiver(selection, ctx, reason);
    });
  };

  const bindActual = async (selection: Conversation, ctx: ExtensionContext): Promise<void> => {
    if (selection.bound || !current(selection, ctx)) return;
    // A killed subscriber still owns the child slot until its pipes close.
    await receiver?.closed;
    if (!current(selection, ctx)) return;
    const { session, lifetime } = selection;
    try {
      if (!session || Buffer.byteLength(session) > 256 || /[\u0000-\u001f\u007f]/u.test(session)) throw new Error('invalid native conversation ID');
      await fact('bind', 'omp_extension', session, lifetime.signal);
      if (!current(selection, ctx)) return;
      selection.bound = true;
      warnedSessionId = undefined;
      startReceiver(selection, ctx);
    } catch (error) {
      if (!current(selection, ctx) || warnedSessionId === session) return;
      warnedSessionId = session;
      ctx.ui.notify(`Bkpr native conversation: ${String(error).slice(0, 512)}`, 'warning');
    }
  };
  let binding = Promise.resolve();
  const bind = async (ctx: ExtensionContext): Promise<Conversation | undefined> => {
    if (ctx.agent.kind !== 'main' || disposed) return;
    const session = ctx.sessionManager.getSessionId();
    if (conversation?.session !== session) {
      stop();
      conversation = { session, lifetime: new AbortController(), bound: false, retrying: false, retries: 0, lastSeenGeneration: 0 };
    }
    const selection = conversation;
    if (!selection.bound) {
      binding = binding.then(() => bindActual(selection, ctx));
      await binding;
    }
    return selection.bound && current(selection, ctx) ? selection : undefined;
  };
  for (const event of ['session_start', 'session_switch', 'session_branch', 'turn_end'] as const) {
    pi.on(event, async (_event, ctx) => { await bind(ctx); });
  }
  pi.on('before_agent_start', async (event, ctx) => {
    // Native prompt preparation, not terminal input or semantic OSC status.
    // Unsupported/older hosts without this reason remain unknown.
    if (ctx.agent.kind !== 'main' || disposed || event.reason !== 'prompt') return;
    const selection = await bind(ctx);
    if (!selection || !current(selection, ctx)) return;
    // User input can rearm an exhausted subscriber, but automatic turns cannot.
    if (!receiver && !selection.retrying && selection.retries === RECEIVER_RETRY_MS.length) {
      selection.retries = 0;
      startReceiver(selection, ctx);
    }
    if (promptObserved) return;
    try {
      await fact('prompt-submitted', 'omp_extension', selection.session, selection.lifetime.signal);
      if (current(selection, ctx)) promptObserved = true;
    } catch (error) {
      if (current(selection, ctx)) ctx.ui.notify(`Bkpr prompt observation: ${String(error).slice(0, 512)}`, 'warning');
    }
  });
  pi.on('session_shutdown', async (_event, ctx) => {
    if (ctx.agent.kind !== 'main') return;
    disposed = true;
    stop();
    await Promise.all([binding, receiver?.closed]);
  });
}
