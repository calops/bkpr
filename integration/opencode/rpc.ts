// Public OpenCode 2 plugin RPC; payloads are body-free native identities only.
const id = { type: 'string', minLength: 1, maxLength: 256 } as const;
const nativeID = { type: 'string', minLength: 1, maxLength: 1024 } as const;
const challenge = {
  type: 'object', properties: { id, sessionID: nativeID, purpose: { enum: ['prompt', 'tool', 'wake'] } },
  required: ['id', 'sessionID', 'purpose'], additionalProperties: false,
} as const;
const lifecycle = {
  input: { type: 'object', properties: { owner: id }, required: ['owner'], additionalProperties: false },
  output: { type: 'object', properties: {}, additionalProperties: false },
} as const;
/** Body-free owning-TUI enrollment, selection and recovery RPC contract. */
export const Contract = {
  id: 'bkpr-native',
  methods: {
    activate: {
      input: lifecycle.input,
      output: {
        type: 'object', properties: {
          requiredFingerprint: { anyOf: [{
            type: 'object', properties: { id: nativeID, created: { type: 'integer', minimum: 0 } },
            required: ['id', 'created'], additionalProperties: false,
          }, { type: 'null' }] },
          recovering: { type: 'boolean' },
        },
        required: ['requiredFingerprint', 'recovering'], additionalProperties: false,
      },
    },
    deactivate: lifecycle,
    check_owner: {
      input: lifecycle.input,
      output: { enum: ['active', 'inactive', 'unowned', 'superseded'] },
    },
    pending: {
      input: { type: 'object', properties: {}, additionalProperties: false },
      output: { type: 'array', items: challenge },
    },
    answer: {
      input: {
        type: 'object', properties: {
          id, owner: id, sessionID: { anyOf: [nativeID, { type: 'null' }] }, restored: { type: 'boolean' },
          binding: { anyOf: [{
            type: 'object', properties: { id: nativeID, source: { const: 'opencode_plugin' }, generation: { type: 'integer', minimum: 1 } },
            required: ['id', 'source', 'generation'], additionalProperties: false,
          }, { type: 'null' }] },
        },
        required: ['id', 'owner', 'sessionID', 'restored', 'binding'], additionalProperties: false,
      },
      output: { type: 'object', properties: {}, additionalProperties: false },
    },
  },
  events: {
    challenge: { schema: challenge },
    recovery_failed: { schema: { type: 'object', properties: {}, additionalProperties: false } },
  },
} as const;

type Location = { directory: string };
type PluginClient = {
  event: { subscribe(options: { types: readonly ['plugin.updated']; signal: AbortSignal }): AsyncIterable<{ type: string; location?: Location }> };
  plugin: { list(options: { location: Location; signal: AbortSignal }): Promise<{ data: readonly {
    id?: string; features: { server?: boolean; rpc?: boolean }; state: { status: string; error?: string };
  }[] }> };
};
type Readiness = {
  ready: boolean;
  revision: number;
  waiters: number;
  changed: ReturnType<typeof Promise.withResolvers<void>>;
};

/** Location-scoped server-plugin readiness from native inventory and lifecycle events. */
export class NativePluginReadiness {
  private readonly controller = new AbortController();
  private readonly connected = Promise.withResolvers<void>();
  private readonly locations = new Map<string, Readiness>();
  private failure: Error | undefined;
  private readonly completion = Promise.withResolvers<Error>();
  /** Resolves when the lifecycle stream fails or this readiness scope is closed. */
  readonly ended = this.completion.promise;

  /** Subscribe to the native server plugin lifecycle. */
  constructor(private readonly client: PluginClient) {
    void this.listen();
  }

  private fail(error: unknown): void {
    if (this.failure) return;
    this.failure = error instanceof Error ? error : new Error('native plugin lifecycle unavailable');
    this.completion.resolve(this.failure);
    this.connected.resolve();
    for (const state of this.locations.values()) state.changed.resolve();
    this.controller.abort(this.failure);
  }

  private async listen(): Promise<void> {
    try {
      for await (const event of this.client.event.subscribe({ types: ['plugin.updated'], signal: this.controller.signal })) {
        if (event.type === 'server.connected') this.connected.resolve();
        if (event.type !== 'plugin.updated' && event.type !== 'server.connected') continue;
        for (const [directory, state] of this.locations) {
          if (event.location && event.location.directory !== directory) continue;
          state.ready = false;
          state.revision += 1;
          state.changed.resolve();
          state.changed = Promise.withResolvers<void>();
        }
      }
      this.fail(new Error('native plugin lifecycle stream closed'));
    } catch (error) {
      this.fail(error);
    }
  }

  /** Wait for this location's server registration, failing on setup, stream or deadline errors. */
  async wait(location: Location): Promise<void> {
    if (this.failure) throw this.failure;
    const directory = location.directory;
    if (typeof directory !== 'string' || Buffer.byteLength(directory) > 4096) throw new Error('invalid native plugin location');
    let state = this.locations.get(directory);
    if (state) {
      this.locations.delete(directory);
      this.locations.set(directory, state);
      if (state.ready) return;
    } else {
      if (this.locations.size >= 64) {
        for (const [key, candidate] of this.locations) {
          if (candidate.waiters !== 0) continue;
          this.locations.delete(key);
          break;
        }
        if (this.locations.size >= 64) throw new Error('native plugin readiness capacity exhausted');
      }
      state = { ready: false, revision: 0, waiters: 0, changed: Promise.withResolvers<void>() };
      this.locations.set(directory, state);
    }
    state.waiters += 1;
    const deadline = new AbortController();
    const timer = setTimeout(() => deadline.abort(new Error('native plugin readiness deadline exceeded')), 15000);
    const signal = AbortSignal.any([this.controller.signal, deadline.signal]);
    const stopped = Promise.withResolvers<never>();
    const abort = (): void => stopped.reject(signal.reason);
    signal.addEventListener('abort', abort, { once: true });
    if (signal.aborted) abort();
    const bounded = <T>(promise: Promise<T>): Promise<T> => Promise.race([promise, stopped.promise]);
    try {
      await bounded(this.connected.promise);
      signal.throwIfAborted();
      for (;;) {
        signal.throwIfAborted();
        if (this.failure) throw this.failure;
        const revision = state.revision;
        const changed = state.changed.promise;
        const inventory = await bounded(this.client.plugin.list({ location, signal }));
        signal.throwIfAborted();
        if (this.failure) throw this.failure;
        if (state.revision !== revision) continue;
        const plugin = inventory.data.find(item => item.id === Contract.id);
        if (plugin?.state.status === 'failed') throw new Error(`native Bkpr server plugin failed: ${plugin.state.error}`);
        if (plugin?.state.status === 'active') {
          if (!plugin.features.server || !plugin.features.rpc) throw new Error('native Bkpr server plugin has no RPC registration');
          state.ready = true;
          return;
        }
        await bounded(changed);
      }
    } finally {
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
      state.waiters -= 1;
    }
  }

  /** Cancel pending readiness and release the native event subscription. */
  close(): void {
    this.fail(new Error('native plugin readiness closed'));
    this.locations.clear();
  }
}
