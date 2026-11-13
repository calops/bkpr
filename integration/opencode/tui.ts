import { Plugin } from '@opencode/plugin/tui';
import { createEffect, createRoot } from 'solid-js';
import { randomUUID } from 'node:crypto';
import { optionalNativeCallbacks } from '../shared/native.ts';
import { Contract, NativePluginReadiness } from './rpc.ts';
import { NativeSession, object, requiredRequest, requiredSession, sessionID, validateRecovery, type RequiredSession } from './session.ts';

/** Native UI remains in charge of its editor, draft, navigation and OSC output. */
export default Plugin.define({
  id: 'bkpr-native',
  async setup(ctx) {
    const activation = await optionalNativeCallbacks('opencode', ctx.options.captureFile);
    if (!activation) return;
    const { native } = activation;
    const requested = requiredRequest(ctx.options.requiredSession);
    let required: RequiredSession | undefined;
    let restored = requested === undefined;
    let closed = false;
    const warn = (message: string): void => ctx.ui.toast.show({ title: 'Bkpr', message, variant: 'warning' });
    const currentRoot = (): string | null => {
      if (closed || !restored) return null;
      const route = ctx.ui.router.current();
      if (route.type !== 'session' || !ctx.data.session.get(route.sessionID)) return null;
      const root = ctx.data.session.root(route.sessionID);
      const info = ctx.data.session.get(root);
      return info && !info.parentID ? sessionID(root) : null;
    };
    const currentLocation = () => {
      const root = currentRoot();
      return (root ? ctx.data.session.get(root)?.location : undefined) ?? ctx.location ?? ctx.data.location.default();
    };
    const owner = randomUUID();
    const readiness = new NativePluginReadiness(ctx.client);
    const activationLocation = currentLocation();
    let activationStarted = false;
    const enrollment = async () => {
      const location = currentLocation();
      await readiness.wait(location);
      return ctx.client.rpc(Contract).check_owner({ owner }, { location });
    };
    const ownsEnrollment = async (): Promise<boolean> => !closed && await enrollment() === 'active';
    const superseded = async (): Promise<boolean> => {
      try { return !closed && await enrollment() === 'superseded'; } catch { return false; }
    };
    const integration = new NativeSession(native, currentRoot,
      (input, signal) => ctx.client.session.prompt(input, { signal }), warn,
      ownsEnrollment);
    let stop: (() => void) | undefined;
    let dispose: (() => void) | undefined;
    let closing: Promise<void> | undefined;
    let ready = false;
    const deactivate = (): Promise<void> => {
      if (closing) return closing;
      closed = true;
      stop?.();
      dispose?.();
      closing = (async () => {
        try {
          // Withdraw capture even if native cleanup cannot be confirmed.
          const results = await Promise.allSettled([integration.close(), activationStarted
            ? ctx.client.rpc(Contract).deactivate({ owner }, { location: activationLocation, signal: AbortSignal.timeout(15000) })
            : Promise.resolve()]);
          const failed = results.find(result => result.status === 'rejected');
          if (failed?.status === 'rejected') throw failed.reason;
        } finally {
          readiness.close();
        }
      })();
      return closing;
    };
    const failRecovery = async (reason = 'exact native OpenCode recovery target is missing or changed'): Promise<void> => {
      try { await deactivate(); } catch {
        process.stderr.write('bkpr: native recovery cleanup was not confirmed; no callback replay\n');
      } finally {
        process.stderr.write(`bkpr: ${reason}\n`);
        process.kill(process.pid, 'SIGTERM');
      }
    };
    void readiness.ended.then(async () => {
      if (closed) return;
      if (requested) {
        await failRecovery('native service connection lost during recovered launch');
        return;
      }
      try { await deactivate(); } catch {
        process.stderr.write('bkpr: native service capture cleanup was not confirmed; no callback replay\n');
      }
    });
    const answered = new Set<string>();
    const answer = async (input: unknown, location: { directory: string }): Promise<void> => {
      const challenge = object(input);
      const id = sessionID(challenge.id);
      const session = sessionID(challenge.sessionID);
      if (challenge.purpose !== 'prompt' && challenge.purpose !== 'tool' && challenge.purpose !== 'wake') throw new Error('invalid native selection challenge');
      if (answered.has(id) || closed || !ready) return;
      answered.add(id);
      if (answered.size > 128) answered.delete(answered.values().next().value!);
      try {
        await readiness.wait(location);
        if (closed) return;
        if (challenge.purpose === 'prompt' && currentRoot() !== session) {
          // Background native sessions keep their ordinary prompt/model/tool
          // behavior. They neither select a capture root nor claim a user fact.
          await ctx.client.rpc(Contract).answer({ id, owner, sessionID: null, restored, binding: null }, { location });
          return;
        }
        if (challenge.purpose === 'prompt') await integration.prompt(session);
        const binding = await integration.barrier();
        if (closed) return;
        await ctx.client.rpc(Contract).answer({ id, owner, sessionID: binding?.id ?? null, restored, binding }, { location });
      } catch {
        if (!closed) warn('Native prompt binding could not be confirmed; no prompt fact replay');
      }
    };
    stop = ctx.data.listen(event => {
      if (closed) return;
      if (event.details.type === 'rpc.bkpr-native.recovery_failed') { void failRecovery(); return; }
      if (event.details.type !== 'rpc.bkpr-native.challenge') return;
      const details = event.details as typeof event.details & { data: unknown; location: { directory: string } };
      void answer(details.data, details.location);
    });
    try {
      await readiness.wait(activationLocation);
      activationStarted = true;
      const enrollment = object(await ctx.client.rpc(Contract).activate({ owner }, { location: activationLocation }));
      if (requested && enrollment.recovering !== false) {
        required = requiredSession(enrollment.requiredFingerprint);
        if (!required || required.id !== requested.id) throw new Error('native recovery enrollment identity mismatch');
      } else {
        restored = true;
      }
      await integration.enroll();
    } catch (error) {
      if (await superseded()) { await deactivate(); return deactivate; }
      if (requested) { if (!closed) await failRecovery(); else await deactivate(); return deactivate; }
      await deactivate();
      throw error;
    }
    if (required) {
      const fingerprint = required;
      const validateRequired = async (): Promise<void> => {
        if (closed) throw new Error('native recovery preparation cancelled');
        const info = await ctx.client.session.get({ sessionID: fingerprint.id });
        if (closed) throw new Error('native recovery preparation cancelled');
        validateRecovery(info, fingerprint);
      };
      try {
        await validateRequired();
        await ctx.data.session.sync(required.id);
        await validateRequired();
        ctx.ui.router.navigate({ type: 'session', sessionID: required.id });
        // Navigation starts another asynchronous native family load. A fresh
        // snapshot is not route readiness: the server validates the original
        // identity again around the first native prompt preparation, including a
        // possible home-created fallback.
        await validateRequired();
        restored = true;
        await integration.select(required.id);
        await validateRequired();
        if (currentRoot() !== required.id) throw new Error('exact native recovery route changed');
      } catch {
        if (await superseded()) { await deactivate(); return deactivate; }
        if (!closed) await failRecovery();
        else await deactivate();
        return deactivate;
      }
    }
    ready = true;
    dispose = createRoot(cleanup => {
    createEffect(() => {
      const root = currentRoot();
      const location = currentLocation();
      void integration.select(root).then(async () => {
        if (closed) return;
        // A --prompt admission may have reached the server while this native
        // TUI plugin was loading. Recover its body-free barrier, never history.
        await readiness.wait(location);
        if (closed) return;
        const challenges = await ctx.client.rpc(Contract).pending({}, { location });
        if (!Array.isArray(challenges)) throw new Error('invalid native pending barriers');
        for (const challenge of challenges) await answer(challenge, location);
      }).catch(() => { if (!closed) warn('Native selection binding unavailable; Bkpr context remains unavailable'); });
    });
      return cleanup;
    });
    return deactivate;
  },
});
