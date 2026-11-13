import { Plugin } from '@opencode/plugin';
import { randomUUID } from 'node:crypto';
import { Contract } from './rpc.ts';
import { createNativeCallbacksFromFile } from '../shared/native.ts';
import { conversation, object, recoveryIdentity, requiredRequest, sessionID, validateRecovery,
  type Conversation, type RequiredSession, type RequiredSessionRequest } from './session.ts';

type Lifecycle = {
  owner?: string;
  inactive: boolean;
  failed?: true;
  recoveryConfirmed: boolean;
  fingerprint?: RequiredSession;
  lookup?: Promise<RequiredSession>;
  deactivate?: Set<() => void>;
};
// Native RPC/plugin contexts are Location-scoped and local source loads can
// re-evaluate this module. This private server's enrollment must instead stay
// launch-wide, including locations first opened after /plugins deactivation.
const lifecycleKey = Symbol.for('bkpr.native.opencode.lifecycle');
const runtime = globalThis as typeof globalThis & { [lifecycleKey]?: Map<string, Lifecycle> };
const lifecycles = runtime[lifecycleKey] ??= new Map<string, Lifecycle>();
const MAX_LIFECYCLES = 64;
const MAX_CAPTURE_ID_BYTES = 4096;

function acquireLifecycle(captureFile: unknown): Lifecycle {
  if (typeof captureFile !== 'string' || Buffer.byteLength(captureFile) > MAX_CAPTURE_ID_BYTES) {
    throw new Error('invalid private native capture identity');
  }
  const existing = lifecycles.get(captureFile);
  if (existing) {
    lifecycles.delete(captureFile);
    lifecycles.set(captureFile, existing);
    return existing;
  }
  if (lifecycles.size >= MAX_LIFECYCLES) {
    for (const [identity, candidate] of lifecycles) {
      if (candidate.deactivate?.size) continue;
      lifecycles.delete(identity);
      break;
    }
    if (lifecycles.size >= MAX_LIFECYCLES) throw new Error('private native lifecycle capacity exhausted');
  }
  // Admission precedes allocation; no live enrollment is silently evicted.
  const lifecycle: Lifecycle = { inactive: false, recoveryConfirmed: false };
  lifecycles.set(captureFile, lifecycle);
  return lifecycle;
}

/** Private standalone server part: await the actual TUI before prompt admission. */
export default Plugin.define({
  id: 'bkpr-native',
  async setup(ctx) {
    type Answer = { selected: string | null; restored: boolean; binding: Conversation | null; inactive?: true; owner?: string };
    type Pending = { sessionID: string; purpose: 'prompt' | 'tool' | 'wake'; resolve(answer: Answer): void; reject(error: Error): void; timer: NodeJS.Timeout };
    // This is a pre-admission identity/selection fence, not evidence that the
    // native route finished loading or that a prompt was durably admitted.
    const captureFile = ctx.options.captureFile;
    let lifecycle: Lifecycle;
    try { lifecycle = acquireLifecycle(captureFile); } catch (error) {
      process.stderr.write('bkpr: private native lifecycle enrollment is unavailable\n');
      process.kill(process.pid, 'SIGTERM');
      throw error;
    }
    const pending = new Map<string, Pending>();
    const lifetime = new AbortController();
    let closed = false;
    let initialized = false;
    let required: RequiredSession | undefined;
    let requested: RequiredSessionRequest | undefined;
    const deactivatePending = (): void => {
      for (const item of pending.values()) {
        clearTimeout(item.timer);
        if (item.purpose === 'prompt') item.resolve({ selected: null, restored: false, binding: null, inactive: true });
        else item.reject(new Error('Bkpr native TUI integration is inactive'));
      }
      pending.clear();
    };
    (lifecycle.deactivate ??= new Set()).add(deactivatePending);
    let toolContext: { dispose(): Promise<void> } | undefined;
    const registration = await ctx.rpc.register(Contract, {
      activate: async input => {
        if (closed || lifecycle.failed) throw new Error('native integration is closed');
        const selectedOwner = sessionID(object(input).owner);
        lifecycle.owner = selectedOwner;
        lifecycle.inactive = false;
        await validateInitialRecovery();
        if (closed || lifecycle.owner !== selectedOwner) throw new Error('native TUI enrollment was superseded');
        return { requiredFingerprint: lifecycle.fingerprint ?? null, recovering: requested !== undefined && !lifecycle.recoveryConfirmed };
      },
      deactivate: async input => {
        if (sessionID(object(input).owner) !== lifecycle.owner) return {};
        lifecycle.owner = undefined;
        lifecycle.inactive = true;
        lifecycle.deactivate?.forEach(deactivate => deactivate());
        return {};
      },
      check_owner: async input => {
        const owner = sessionID(object(input).owner);
        if (!lifecycle.owner) return 'unowned';
        if (lifecycle.owner !== owner) return 'superseded';
        return closed || lifecycle.inactive || lifecycle.failed ? 'inactive' : 'active';
      },
      pending: async () => [...pending].map(([id, item]) => ({ id, sessionID: item.sessionID, purpose: item.purpose })),
      answer: async input => {
        if (!lifecycle.owner || lifecycle.inactive) throw new Error('Bkpr native TUI integration is not active');
        const value = object(input);
        const owner = sessionID(value.owner);
        if (owner !== lifecycle.owner) throw new Error('native TUI answer enrollment was superseded');
        const item = pending.get(sessionID(value.id));
        if (!item || typeof value.restored !== 'boolean') throw new Error('expired or invalid native prompt barrier');
        const selected = value.sessionID === null ? null : sessionID(value.sessionID);
        const binding = value.binding === null ? null : conversation(value.binding);
        if ((binding?.id ?? null) !== selected) throw new Error('native barrier binding differs from selected root');
        pending.delete(String(value.id));
        clearTimeout(item.timer);
        item.resolve({ selected, restored: value.restored, binding, owner });
        return {};
      },
    });
    const barrier = async (session: string, purpose: 'prompt' | 'tool' | 'wake', signal = lifetime.signal): Promise<Answer> => {
      if (closed || !initialized || pending.size >= 8 || signal.aborted) throw new Error('Bkpr native selection barrier unavailable');
      if (lifecycle.inactive) {
        if (purpose !== 'prompt') throw new Error('Bkpr native TUI integration is inactive');
        return { selected: null, restored: false, binding: null, inactive: true };
      }
      const id = randomUUID();
      const answer = Promise.withResolvers<Answer>();
      const cancel = (): void => { answer.reject(new Error('native selection admission cancelled before dispatch')); };
      const timer = setTimeout(() => answer.reject(new Error('Bkpr native selection binding timed out')), 15000);
      pending.set(id, { sessionID: session, purpose, ...answer, timer });
      signal.addEventListener('abort', cancel, { once: true });
      try {
        const [, reply] = await Promise.all([
          registration.events.emit('challenge', { id, sessionID: session, purpose }), answer.promise,
        ]);
        if (closed || signal.aborted) throw new Error('native selection admission cancelled before dispatch');
        if (required && !reply.restored && !reply.inactive) throw new Error('exact native recovery has not completed');
        return reply;
      } finally {
        signal.removeEventListener('abort', cancel);
        clearTimeout(timer);
        pending.delete(id);
      }
    };
    const validateInitialRecovery = async (signal = lifetime.signal): Promise<void> => {
      if (lifecycle.failed) throw new Error('native recovery has already failed');
      if (!requested || lifecycle.recoveryConfirmed) return;
      try {
        if (!lifecycle.fingerprint) {
          // A single public lookup in this same alive native service owns the
          // launch's canonical fingerprint, shared by every Location context.
          const request = requested;
          lifecycle.lookup ??= (async () => recoveryIdentity(await ctx.session.get({ sessionID: request.id }), request))();
          const lookup = lifecycle.lookup;
          const fingerprint = await lookup;
          if (closed || signal.aborted) throw new Error('native recovery validation cancelled before dispatch');
          lifecycle.fingerprint ??= fingerprint;
          if (lifecycle.lookup === lookup) lifecycle.lookup = undefined;
        }
        required = lifecycle.fingerprint;
        if (required.id !== requested.id) throw new Error('native recovery target differs across locations');
        if (closed || signal.aborted) throw new Error('native recovery validation cancelled before dispatch');
        const info = await ctx.session.get({ sessionID: required.id });
        // The public server getter is not cancellable. Discard its result on
        // teardown/deadline instead of publishing facts after it resolves.
        if (closed || signal.aborted) throw new Error('native recovery validation cancelled before dispatch');
        validateRecovery(info, required);
      } catch (error) {
        if (!closed && !signal.aborted) {
          closed = true;
          lifecycle.failed = true;
          lifetime.abort();
          process.stderr.write('bkpr: exact native OpenCode recovery target is missing, changed or unavailable\n');
          // Keep the transport alive until the owning TUI withdraws capture
          // and exits; stopping this server can lose the volatile failure event.
          await registration.events.emit('recovery_failed', {});
        }
        throw error;
      }
    };
    // Register the admission fence before any recoverable setup/check. Native
    // SessionPrompt.prepare propagates hook rejection before admission/wake.
    const prompt = await ctx.session.hook('prompt', async event => {
      if (closed || !initialized || lifecycle.failed) throw new Error('Bkpr native prompt barrier unavailable');
      await validateInitialRecovery();
      if (closed || lifetime.signal.aborted || lifecycle.failed) throw new Error('native prompt preparation cancelled');
      // Queued wakes cross the same recovery fence without observing a user
      // prompt; unlike ordinary background work they must still own selection.
      const wake = event.metadata?.['bkpr.nativeWake'] === true;
      const reply = await barrier(sessionID(event.sessionID), wake ? 'wake' : 'prompt');
      if (wake && reply.selected !== event.sessionID) throw new Error('native queued wake no longer owns the selected root');
      // Validate the original target even for a freshly created home fallback,
      // after the owning TUI answered, not only when event.sessionID is A.
      await validateInitialRecovery();
      if (closed || lifetime.signal.aborted || lifecycle.failed) throw new Error('native prompt preparation cancelled');
      if (wake && (lifecycle.inactive || reply.owner !== lifecycle.owner)) throw new Error('native wake TUI enrollment was superseded');
      lifecycle.recoveryConfirmed = true;
    });
    try {
      requested = requiredRequest(ctx.options.requiredSession);
      if (typeof ctx.options.captureFile !== 'string') throw new Error('missing private native capture file');
      const native = await createNativeCallbacksFromFile('opencode', ctx.options.captureFile);
      toolContext = await ctx.tool.hook('execute.before', async event => {
        // Native MCP effectiveName prefixes the normalized server namespace
        // with "_". Public ToolDomain has no get(id), so avoid constructing
        // its full catalog for ordinary bash/read/native tool execution.
        if (!event.tool.startsWith('bkpr_')) return;
        const controller = new AbortController();
        const signal = AbortSignal.any([controller.signal, lifetime.signal]);
        const timer = setTimeout(() => controller.abort(), 15000);
        let scoped = false;
        let fenced = false;
        let enrolled: string | undefined;
        const check = (): void => {
          if (closed || signal.aborted || lifecycle.failed) throw new Error('native tool context cancelled before dispatch');
          if (scoped && lifecycle.inactive) throw new Error('Bkpr native TUI integration is inactive');
          if (fenced && (!enrolled || enrolled !== lifecycle.owner)) throw new Error('native TUI tool enrollment was superseded');
        };
        try {
          check();
          const tools = await ctx.tool.list();
          check();
          if (tools.find(tool => tool.id === event.tool)?.options?.namespace !== 'bkpr') return;
          scoped = true;
          check();
          await validateInitialRecovery(signal);
          check();
          const reply = await barrier(sessionID(event.sessionID), 'tool', signal);
          enrolled = reply.owner;
          fenced = true;
          check();
          if (!reply.binding) throw new Error('Bkpr tool context has no selected native root');
          const identity = await native.request({ method: 'whoami' }, signal);
          check();
          const current = conversation(identity.conversation);
          if (current.id !== reply.binding.id || current.generation !== reply.binding.generation) {
            throw new Error('Bkpr tool context selection binding was superseded');
          }
          let root = sessionID(event.sessionID);
          const seen = new Set<string>();
          let bytes = 0;
          for (;;) {
            check();
            if (seen.has(root)) throw new Error('native session ancestry contains a cycle');
            bytes += Buffer.byteLength(root);
            if (bytes > 64 * 1024) throw new Error('native session ancestry exceeds metadata budget');
            seen.add(root);
            const info = await ctx.session.get({ sessionID: root });
            check();
            if (!info.parentID) break;
            root = sessionID(info.parentID);
          }
          if (root !== current.id) throw new Error('Bkpr tool context is not the current native root');
          await validateInitialRecovery(signal);
          check();
          if (event.sessionID !== root) {
            await native.event({ kind: 'native_context', conversation: current, context_id: event.sessionID }, signal);
            check();
          }
        } finally {
          clearTimeout(timer);
          controller.abort();
        }
      });
      initialized = true;
    } catch {
      closed = true;
      lifetime.abort();
      process.stderr.write('bkpr: private native OpenCode integration is unavailable; stopping private server\n');
      process.kill(process.pid, 'SIGTERM');
    }
    return async () => {
      closed = true;
      lifetime.abort();
      for (const item of pending.values()) { clearTimeout(item.timer); item.reject(new Error('native plugin closed')); }
      pending.clear();
      lifecycle.deactivate?.delete(deactivatePending);
      if (lifecycle.deactivate?.size === 0) {
        // Keep only this bounded launch-private phase/fingerprint tombstone
        // until private process exit; forgetting it would reset disabled or
        // completed-recovery semantics when the routing cache closes a Location.
        lifecycle.deactivate = undefined;
        lifecycle.lookup = undefined;
      }
      await prompt.dispose();
      await toolContext?.dispose();
      await registration.dispose();
    };
  },
});
