import type { AgentHandle, CreateAgentOptions, PaseoBackend, RunResult } from "../src/backend.js";

export interface Deferred<T> { promise: Promise<T>; resolve(value: T): void; reject(error: unknown): void; }
export function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
export interface DeferredRun extends Deferred<RunResult> { prompt: string; options: { timeoutMs: number; outputSchema?: Record<string, unknown> }; }

export class FakeAgent implements AgentHandle {
  readonly runs: DeferredRun[] = [];
  readonly updateListeners = new Set<(value: unknown) => void>();
  readonly streamListeners = new Set<(value: unknown) => void>();
  archived = false;
  refreshResult: { exists: boolean; running: boolean } | Deferred<{ exists: boolean; running: boolean }> = { exists: true, running: false };
  constructor(readonly id: string) {}
  run(prompt: string, options: { timeoutMs: number; outputSchema?: Record<string, unknown> }): Promise<RunResult> {
    const pending = deferred<RunResult>();
    this.runs.push({ prompt, options, ...pending });
    return pending.promise;
  }
  refresh(): Promise<{ exists: boolean; running: boolean }> { return "promise" in this.refreshResult ? this.refreshResult.promise : Promise.resolve(this.refreshResult); }
  subscribeUpdate(handler: (value: unknown) => void): () => void { this.updateListeners.add(handler); return () => void this.updateListeners.delete(handler); }
  subscribeStream(handler: (value: unknown) => void): () => void { this.streamListeners.add(handler); return () => void this.streamListeners.delete(handler); }
  async archive(): Promise<void> { this.archived = true; }
  update(value: unknown): void { for (const listener of this.updateListeners) listener(value); }
  stream(value: unknown): void { for (const listener of this.streamListeners) listener(value); }
}

export class FakeBackend implements PaseoBackend {
  connected = false;
  closed = false;
  closeCalls = 0;
  connectResult: Deferred<void> | null = null;
  supportsRemoteCancel = true;
  cancelResult: boolean | Deferred<boolean> = true;
  readonly createCalls: CreateAgentOptions[] = [];
  readonly createDeferred: Array<Deferred<AgentHandle>> = [];
  deferCreates = false;
  cleanupResult: void | Error | Deferred<void> = undefined;
  readonly cleanupCalls: string[] = [];
  readonly agents = new Map<string, FakeAgent>();
  providers = [{ provider: "fake", status: "ready", enabled: true }];
  async connect(): Promise<void> { if (this.connectResult !== null) await this.connectResult.promise; this.connected = true; }
  async close(): Promise<void> { this.closeCalls++; this.closed = true; }
  createAgent(options: CreateAgentOptions): Promise<AgentHandle> {
    this.createCalls.push(options);
    const agent = new FakeAgent(`agent-${this.createCalls.length}`);
    this.agents.set(agent.id, agent);
    if (!this.deferCreates) return Promise.resolve(agent);
    const pending = deferred<AgentHandle>();
    this.createDeferred.push(pending);
    return pending.promise;
  }
  refAgent(id: string): AgentHandle { const agent = this.agents.get(id) ?? new FakeAgent(id); this.agents.set(id, agent); return agent; }
  async listProviders(): Promise<Array<{ provider: string; status: string; enabled: boolean }>> { return this.providers; }
  cancelAgent(): Promise<boolean> { return typeof this.cancelResult === "boolean" ? Promise.resolve(this.cancelResult) : this.cancelResult.promise; }
  async cleanupOwnedAgent(agent: AgentHandle): Promise<void> {
    this.cleanupCalls.push(agent.id);
    if (this.cleanupResult instanceof Error) throw this.cleanupResult;
    if (this.cleanupResult !== undefined) await this.cleanupResult.promise;
    await agent.archive();
  }
}
