import type { PluginModule } from '@opencode-ai/plugin';
import { createNativeCallbacksFromFile, type NativeCallbacks } from '../shared/native.ts';

/** Observe this launch's first native submission before persistence, not durable admission. */
export default {
  id: 'bkpr',
  server: async (input, options) => {
    if (options?.captureFile === undefined) return {};
    let warned = false;
    const warn = (message: string): void => {
      if (warned) return;
      warned = true;
      void input.client.tui.showToast({ body: { title: 'Bkpr capture', message, variant: 'warning' } }).catch(() => {});
    };
    let native: NativeCallbacks;
    try {
      if (typeof options.captureFile !== 'string') throw new Error('invalid capture file');
      native = await createNativeCallbacksFromFile('opencode', options.captureFile);
    } catch {
      warn('Private prompt capture setup is invalid or unavailable; prompt observation is disabled.');
      return {};
    }
    const lifetime = new AbortController();
    const pending = new Set<Promise<void>>();
    let disposed = false;
    let observed = false;
    return {
      'chat.message': async ({ sessionID }) => {
        if (disposed || observed) return;
        if (!sessionID || Buffer.byteLength(sessionID) > 256 || /[\u0000-\u001f\u007f]/u.test(sessionID)) {
          warn('Native prompt observation has an invalid session identity.');
          return;
        }
        if (pending.size >= 32) {
          warn('Native prompt observation capacity exceeded; that submission remains unknown.');
          return;
        }
        const observation: Promise<void> = Promise.resolve().then(async () => {
          if (disposed || observed) return;
          await native.fact('prompt-submitted', 'opencode_plugin', sessionID, lifetime.signal);
          observed = true;
        }).catch(() => {
          if (!disposed) warn('Native prompt observation could not be recorded; no observation replay.');
        }).finally(() => { pending.delete(observation); });
        pending.add(observation);
      },
      dispose: async () => {
        disposed = true;
        lifetime.abort();
        await Promise.allSettled(pending);
        pending.clear();
      },
    };
  },
} satisfies PluginModule;
