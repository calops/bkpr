import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import { isAbsolute } from 'node:path';

const MAX_FRAME = 1024 * 1024;
const MAX_RESPONSE = 8 * 1024 * 1024;

/** JSON object exchanged with the launch-local Bkpr capture callback. */
export type Json = Record<string, unknown>;
/** Body-free, generation-specific reference to a native inbox notification. */
export type InboxPointer = {
  notification_id: string;
  generation: number;
  session_id: string;
  message_count: number;
  unread_count: number;
};

/** Launch-private calls to the native capture executable. */
export interface NativeCallbacks {
  /** Absolute native Bkpr executable. */
  program: string;
  /** Scoped child environment, never installed globally. */
  environment: NodeJS.ProcessEnv;
  /** Dispatch one bounded native agent request. */
  request(value: Json, signal?: AbortSignal): Promise<Json>;
  /** Report an exact native binding or prompt submission. */
  fact(command: 'bind' | 'prompt-submitted', source: string, session: string, signal?: AbortSignal): Promise<void>;
  /** Dispatch one typed capture event using its exact binding generation. */
  event(value: Json, signal?: AbortSignal): Promise<Json>;
}

/** Semantic state supplied by an authenticated native integration. */
export type NativeStatus = {
  state: 'idle' | 'working' | 'done' | 'blocked' | 'error' | 'clear';
  kind?: 'permission' | 'question' | 'auth';
};

/** Ordered, generation-bound volatile status publication. */
export interface NativeStatusChannel {
  /** Resolves after enrollment against the current native binding. */
  ready: Promise<void>;
  /** Resolves after publication, not durable storage or message processing. */
  report(status: NativeStatus): Promise<void>;
  /** Cancel this channel and every outstanding report. */
  close(): void;
}

function object(value: unknown): Json {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('expected JSON object');
  return value as Json;
}

/** Validate untrusted notification metadata without retaining message bodies. */
export function parseInboxPointer(value: unknown): InboxPointer {
  const item = object(value);
  if (typeof item.notification_id !== 'string' || !item.notification_id || item.notification_id.length > 512 ||
      /[\u0000-\u001f\u007f]/u.test(item.notification_id) ||
      typeof item.generation !== 'number' || !Number.isSafeInteger(item.generation) || item.generation < 1 ||
      typeof item.session_id !== 'string' || !item.session_id || Buffer.byteLength(item.session_id) > 1024 ||
      /[\u0000-\u001f\u007f]/u.test(item.session_id) ||
      typeof item.message_count !== 'number' || !Number.isSafeInteger(item.message_count) || item.message_count < 1 ||
      typeof item.unread_count !== 'number' || !Number.isSafeInteger(item.unread_count) || item.unread_count < item.message_count) {
    throw new Error('invalid native inbox pointer');
  }
  return { notification_id: item.notification_id, generation: item.generation, session_id: item.session_id,
    message_count: item.message_count, unread_count: item.unread_count };
}

/** Read an owned 0600 launch context, rejecting symlinks, invalid grants, and files larger than 64 KiB. */
export async function createNativeCallbacksFromFile(kind: string, path: string): Promise<NativeCallbacks> {
  if (!isAbsolute(path)) throw new Error('invalid native capture file');
  const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  let config: Json;
  try {
    const stat = await file.stat();
    if (!stat.isFile() || stat.size > 65536 || (stat.mode & 0o777) !== 0o600 ||
        (process.getuid && stat.uid !== process.getuid())) throw new Error('invalid native capture file ownership');
    const buffer = Buffer.alloc(65537);
    let length = 0;
    while (length < buffer.length) {
      const result = await file.read(buffer, length, buffer.length - length, null);
      if (result.bytesRead === 0) break;
      length += result.bytesRead;
    }
    if (length > 65536) throw new Error('native capture file exceeds limit');
    config = object(JSON.parse(buffer.subarray(0, length).toString('utf8')));
  } finally { await file.close(); }
  const grant = object(config.grant);
  const valid = (value: unknown, maximum = 256): value is string =>
    typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= maximum && !/[\u0000-\u001f\u007f]/u.test(value);
  if (grant.kind !== kind || !valid(grant.launch_id) || !valid(grant.pane_id) || !valid(grant.token, 4096) ||
      typeof config.program !== 'string' || !isAbsolute(config.program) ||
      typeof config.socket !== 'string' || !isAbsolute(config.socket)) throw new Error('invalid private native capture setup');
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    if (key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_')) {
      delete environment[key];
      delete process.env[key];
    }
  }
  const callbacks = createNativeCallbacks(kind, { ...environment,
    BKPR_CAPTURE_LAUNCH_ID: grant.launch_id, BKPR_CAPTURE_TOKEN: grant.token, BKPR_CAPTURE_HARNESS: kind,
    BKPR_CAPTURE_PROGRAM: config.program, BKPR_SOCKET: config.socket, BKPR_PANE_ID: grant.pane_id });
  if (!callbacks) throw new Error('native capture setup unavailable');
  return callbacks;
}

/** Detect launch-local activation proof before asynchronous plugin admission work. */
export function nativeActivationPresent(explicitCaptureFile?: unknown): boolean {
  return explicitCaptureFile !== undefined || process.env.BKPR_INTEGRATION_FILE !== undefined ||
    Object.keys(process.env).some(key => key.startsWith('BKPR_CAPTURE_'));
}

/** Select explicit native options, the managed-launch hint, then legacy ephemeral capture. */
export async function optionalNativeCallbacks(kind: string, explicitCaptureFile?: unknown):
Promise<{ native: NativeCallbacks; captureFile?: string } | undefined> {
  const captureFile = explicitCaptureFile !== undefined ? explicitCaptureFile : process.env.BKPR_INTEGRATION_FILE;
  if (captureFile !== undefined) {
    if (typeof captureFile !== 'string' || !isAbsolute(captureFile)) throw new Error('invalid private native capture setup');
    try {
      return { native: await createNativeCallbacksFromFile(kind, captureFile), captureFile };
    } catch {
      throw new Error('invalid private native capture setup');
    }
  }
  const present = Object.keys(process.env).some(key => key.startsWith('BKPR_CAPTURE_'));
  const valid = (value: unknown, maximum = 256): value is string =>
    typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= maximum && !/[\u0000-\u001f\u007f]/u.test(value);
  if (present && (process.env.BKPR_CAPTURE_HARNESS !== kind ||
      !valid(process.env.BKPR_CAPTURE_TOKEN, 4096) || !valid(process.env.BKPR_CAPTURE_LAUNCH_ID) ||
      !valid(process.env.BKPR_PANE_ID) || !process.env.BKPR_CAPTURE_PROGRAM || !isAbsolute(process.env.BKPR_CAPTURE_PROGRAM) ||
      !process.env.BKPR_SOCKET || !isAbsolute(process.env.BKPR_SOCKET))) throw new Error('invalid private native capture setup');
  const native = createNativeCallbacks(kind);
  return native ? { native } : undefined;
}

/** Claim this managed launch's callbacks and remove its capabilities from inherited environments. */
export function createNativeCallbacks(kind: string, providedEnvironment?: NodeJS.ProcessEnv): NativeCallbacks | undefined {
  const selected = providedEnvironment ?? process.env;
  const program = selected.BKPR_CAPTURE_PROGRAM;
  if (selected.BKPR_CAPTURE_HARNESS !== kind || !program) return undefined;
  const environment = { ...selected };
  const privateValues: string[] = [];
  for (const key of Object.keys(environment)) {
    if (key.startsWith('BKPR_CAPTURE_') || key.startsWith('BKPR_AGENT_')) {
      if (environment[key]) privateValues.push(environment[key]!);
      if (!providedEnvironment) delete process.env[key];
    }
  }
  const diagnostic = (value: string): string => {
    for (const secret of privateValues) value = value.split(secret).join('[redacted]');
    return value.replace(/[\u0000-\u001f\u007f-\u009f]/gu, ' ').trim().slice(0, 2048);
  };
  const callback = (args: string[], input: string, signal?: AbortSignal): Promise<Json> => {
    const { promise, resolve, reject } = Promise.withResolvers<Json>();
    if (signal?.aborted) { reject(new Error('cancelled before dispatch')); return promise; }
    const child = spawn(program, args, { env: environment, stdio: ['pipe', 'pipe', 'pipe'] });
    const chunks: Buffer[] = [];
    const errors: Buffer[] = [];
    let bytes = 0;
    let errorBytes = 0;
    let settled = false;
    const finish = (error?: Error, result?: Json) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      if (error) reject(error); else resolve(result!);
    };
    const abort = () => {
      child.kill('SIGKILL');
      finish(new Error('native callback cancelled after dispatch; mutation outcome unknown, do not replay'));
    };
    const timer = setTimeout(abort, 15000);
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    child.stdin!.on('error', () => {
      child.kill('SIGKILL');
      finish(new Error('native callback input interrupted; mutation outcome unknown'));
    });
    child.stdin!.end(input);
    child.stdout!.on('data', (chunk: Buffer) => {
      bytes += chunk.length;
      if (bytes > MAX_RESPONSE) { child.kill('SIGKILL'); finish(new Error('native callback response exceeds limit; outcome unknown')); }
      else if (!settled) chunks.push(chunk);
    });
    child.stderr!.on('data', (chunk: Buffer) => {
      const remaining = 8192 - errorBytes;
      if (remaining > 0) errors.push(chunk.subarray(0, remaining));
      errorBytes += chunk.length;
    });
    child.on('error', () => finish(new Error('native callback process failed; mutation outcome unknown, do not replay')));
    child.on('close', code => {
      if (settled) return;
      if (code !== 0) {
        const detail = diagnostic(Buffer.concat(errors).toString('utf8'));
        finish(new Error(`native callback failed; mutation outcome unknown, do not replay${detail ? `: ${detail}` : ''}`));
        return;
      }
      try {
        const text = Buffer.concat(chunks).toString('utf8');
        if (!text.endsWith('\n') || text.slice(0, -1).includes('\n')) throw new Error('invalid native callback framing');
        finish(undefined, object(JSON.parse(text)));
      } catch { finish(new Error('invalid native callback response; mutation outcome unknown, do not replay')); }
    });
    return promise;
  };
  const request = (request: Json, signal?: AbortSignal): Promise<Json> => {
    const encoded = JSON.stringify(request);
    if (Buffer.byteLength(encoded) > MAX_FRAME) return Promise.reject(new Error('native callback request exceeds limit'));
    return callback(['capture', 'agent', '--stdin'], encoded, signal);
  };
  const fact = (command: 'bind' | 'prompt-submitted', source: string, session: string, signal?: AbortSignal): Promise<void> => {
    const { promise, resolve, reject } = Promise.withResolvers<void>();
    if (signal?.aborted) { reject(new Error('cancelled before dispatch')); return promise; }
    const child = spawn(program, ['capture', command, '--source', source, '--session-id', session],
      { env: environment, stdio: ['ignore', 'ignore', 'pipe'] });
    const errors: Buffer[] = [];
    let errorBytes = 0;
    let settled = false;
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener('abort', abort);
      if (error) reject(error); else resolve();
    };
    const abort = () => {
      child.kill('SIGKILL');
      finish(new Error('native fact callback cancelled after dispatch; mutation outcome unknown, do not replay'));
    };
    const timer = setTimeout(abort, 5000);
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    child.stderr!.on('data', (chunk: Buffer) => {
      const remaining = 8192 - errorBytes;
      if (remaining > 0) errors.push(chunk.subarray(0, remaining));
      errorBytes += chunk.length;
    });
    child.on('error', () => finish(new Error('native fact callback process failed; outcome unknown, do not replay')));
    child.on('close', code => {
      if (code === 0) finish();
      else {
        const detail = diagnostic(Buffer.concat(errors).toString('utf8'));
        finish(new Error(`native fact callback failed; outcome unknown, do not replay${detail ? `: ${detail}` : ''}`));
      }
    });
    return promise;
  };
  const event = (value: Json, signal?: AbortSignal): Promise<Json> => {
    const encoded = JSON.stringify(value);
    if (Buffer.byteLength(encoded) > MAX_FRAME) return Promise.reject(new Error('native capture event exceeds limit'));
    return callback(['capture', 'event', '--stdin'], encoded, signal);
  };
  return { program, environment, request, fact, event };
}

/** Open one bounded status stream for an exact native conversation. */
export function openNativeStatusChannel(native: NativeCallbacks, source: string, session: string): NativeStatusChannel {
  const enrollment = Promise.withResolvers<void>();
  const child = spawn(native.program, ['capture', 'status', '--source', source, '--session-id', session],
    { env: native.environment, stdio: ['pipe', 'pipe', 'pipe'] });
  type Pending = { line: string; resolve(): void; reject(error: Error): void };
  const queued: Pending[] = [];
  let current: Pending | undefined;
  let generation: number | undefined;
  let pending = Buffer.alloc(0);
  let closed = false;
  let deadline: NodeJS.Timeout | undefined;
  const fail = (error: Error): void => {
    if (closed) return;
    closed = true;
    clearTimeout(deadline);
    child.stdin!.destroy();
    child.kill('SIGTERM');
    enrollment.reject(error);
    current?.reject(error);
    current = undefined;
    for (const item of queued.splice(0)) item.reject(error);
  };
  const arm = (): void => {
    clearTimeout(deadline);
    deadline = setTimeout(() => fail(new Error('native status publication timed out; outcome unknown')), 15000);
  };
  const flush = (): void => {
    if (closed || generation === undefined || current || queued.length === 0) return;
    current = queued.shift()!;
    arm();
    child.stdin!.write(current.line, error => {
      if (error) fail(new Error('native status input interrupted; outcome unknown'));
    });
  };
  arm();
  child.stdout!.on('data', (chunk: Buffer) => {
    if (closed) return;
    if (pending.length + chunk.length > 8192) {
      fail(new Error('native status response exceeds limit; outcome unknown'));
      return;
    }
    pending = Buffer.concat([pending, chunk]);
    let newline: number;
    while (!closed && (newline = pending.indexOf(10)) >= 0) {
      const line = pending.subarray(0, newline);
      pending = pending.subarray(newline + 1);
      try {
        const reply = object(JSON.parse(line.toString('utf8')));
        if (!Number.isSafeInteger(reply.generation) || Number(reply.generation) < 1) {
          throw new Error('invalid status generation');
        }
        if (generation === undefined) {
          if (reply.accepted !== undefined) throw new Error('status report preceded enrollment');
          generation = Number(reply.generation);
          clearTimeout(deadline);
          enrollment.resolve();
          flush();
        } else {
          if (!current || reply.accepted !== true || reply.generation !== generation) {
            throw new Error('unexpected status acknowledgement');
          }
          clearTimeout(deadline);
          const completed = current;
          current = undefined;
          completed.resolve();
          flush();
        }
      } catch {
        fail(new Error('invalid native status response; outcome unknown, do not replay'));
      }
    }
  });
  child.stderr!.resume();
  child.stdin!.on('error', () => fail(new Error('native status input interrupted; outcome unknown')));
  child.stdout!.on('error', () => fail(new Error('native status response interrupted; outcome unknown')));
  child.on('error', () => fail(new Error('native status process failed; outcome unknown')));
  child.on('close', () => fail(new Error('native status process exited; outcome unknown')));
  return {
    ready: enrollment.promise,
    report(status) {
      if (closed) return Promise.reject(new Error('native status channel is closed'));
      if (queued.length + Number(current !== undefined) >= 32) {
        return Promise.reject(new Error('native status admission is full; report not dispatched'));
      }
      let line: string;
      try {
        line = JSON.stringify(status) + '\n';
        if (Buffer.byteLength(line) > 4096) throw new Error('status frame exceeds limit');
      } catch {
        return Promise.reject(new Error('invalid native status report; not dispatched'));
      }
      const result = Promise.withResolvers<void>();
      queued.push({ line, resolve: () => result.resolve(), reject: result.reject });
      flush();
      return result.promise;
    },
    close() {
      fail(new Error('native status channel closed; outstanding publication outcome unknown'));
    },
  };
}
