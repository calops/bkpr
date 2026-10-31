import type { ExtensionAPI, ExtensionContext } from '@oh-my-pi/pi-coding-agent';

/** Bind only Omp's actual main-agent conversation; Omp owns OSC 7501. */
export default function bkpr(pi: ExtensionAPI): void {
  const program = process.env.BKPR_CAPTURE_PROGRAM;
  const managed = process.env.BKPR_CAPTURE_HARNESS === 'omp';
  // Factories are instantiated again for subagents: dedup must not be global.
  let boundSessionId: string | undefined;
  let warnedSessionId: string | null = null;

  const bind = async (ctx: ExtensionContext): Promise<void> => {
    if (ctx.agent.kind !== 'main') return;
    const sessionId = ctx.sessionManager.getSessionId();
    if (sessionId && sessionId === boundSessionId) return;
    try {
      if (!managed || !program) throw new Error('launch-local capture credentials are missing');
      if (!sessionId || Buffer.byteLength(sessionId, 'utf8') > 256 || /[\u0000-\u001f\u007f]/u.test(sessionId)) {
        throw new Error('native session manager returned an invalid conversation ID');
      }
      const result = await pi.exec(program, [
        'capture', 'bind', '--source', 'omp_extension', '--session-id', sessionId,
      ], { timeout: 5000 });
      if (result.killed || result.code !== 0) {
        throw new Error(result.stderr.trim() || 'capture callback failed or timed out');
      }
      boundSessionId = sessionId;
      warnedSessionId = null;
    } catch (error) {
      // A failed bind remains retryable at the next real event, but does not
      // flood the terminal every turn. No success, status or readiness is faked.
      if (warnedSessionId === sessionId) return;
      warnedSessionId = sessionId;
      const message = String(error).slice(0, 512).replace(/[\u0000-\u001f\u007f]/gu, ' ');
      ctx.ui.notify(`Bkpr conversation capture: ${message}`, 'warning');
    }
  };

  pi.on('session_start', async (_event, ctx) => { await bind(ctx); });
  pi.on('session_switch', async (_event, ctx) => { await bind(ctx); });
  pi.on('session_branch', async (_event, ctx) => { await bind(ctx); });
  // "Open elsewhere" can replace the native conversation without emitting
  // session_switch. Inspect the actual session manager again at turn boundaries.
  pi.on('turn_end', async (_event, ctx) => { await bind(ctx); });
}
