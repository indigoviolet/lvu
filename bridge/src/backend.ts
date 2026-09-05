import {
  createPaseoClient,
  type PaseoAgentHandle,
  type PaseoAgentStream,
  type PaseoAgentUpdate,
  type PaseoClient,
} from "@getpaseo/client";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export interface RunResult {
  status: "idle" | "error" | "permission" | "timeout";
  error: string | null;
  lastMessage: string | null;
  agentStatus?: AgentStatus | null;
}
export type AgentStatus = "initializing" | "running" | "idle" | "error" | "closed";

export interface AgentHandle {
  readonly id: string;
  run(text: string, options: { timeoutMs: number; outputSchema?: Record<string, unknown> }): Promise<RunResult>;
  refresh(): Promise<{ exists: boolean; running: boolean }>;
  subscribeUpdate(handler: (update: unknown) => void): () => void;
  subscribeStream(handler: (event: unknown) => void): () => void;
  archive(): Promise<void>;
}

export interface CreateAgentOptions {
  provider: string;
  cwd: string;
  modeId?: string;
  thinkingOptionId?: string;
  title?: string;
}

export interface PaseoBackend {
  connect(): Promise<void>;
  close(): Promise<void>;
  createAgent(options: CreateAgentOptions): Promise<AgentHandle>;
  refAgent(id: string): AgentHandle;
  listProviders(): Promise<Array<{ provider: string; status: string; enabled: boolean }>>;
  cancelAgent(id: string, timeoutMs: number): Promise<boolean>;
  cleanupOwnedAgent(agent: AgentHandle, timeoutMs: number): Promise<void>;
  readonly supportsRemoteCancel: boolean;
}

function wrapAgent(agent: PaseoAgentHandle): AgentHandle {
  return {
    id: agent.id,
    run: async (text, options) => {
      const result = await agent.run(text, options);
      return { ...result, agentStatus: result.final?.status ?? null };
    },
    refresh: async () => {
      const result = await agent.refresh();
      return { exists: result !== null, running: result?.agent.status === "running" };
    },
    subscribeUpdate: (handler) => agent.subscribe((update: PaseoAgentUpdate) => handler(update)),
    subscribeStream: (handler) => agent.timeline.subscribe((event: PaseoAgentStream) => handler(event)),
    archive: async () => { await agent.archive(); },
  };
}

export class PaseoCli {
  #available = false;
  constructor(readonly executable: string, readonly host: string) {}
  get available(): boolean { return this.#available; }
  async probe(timeoutMs: number): Promise<void> {
    await execFileAsync(this.executable, ["--version"], { timeout: timeoutMs, maxBuffer: 16 * 1024, encoding: "utf8", windowsHide: true });
    this.#available = true;
  }
  async cancel(id: string, timeoutMs: number): Promise<boolean> {
    if (!this.#available) return false;
    const { stdout } = await execFileAsync(this.executable, ["stop", id, "--host", this.host, "--json"], { timeout: timeoutMs, maxBuffer: 64 * 1024, encoding: "utf8", windowsHide: true });
    const result = JSON.parse(stdout) as { stoppedCount?: unknown };
    if (typeof result.stoppedCount !== "number") throw new Error("Paseo stop returned an invalid response");
    return result.stoppedCount > 0;
  }
  async archive(id: string, timeoutMs: number): Promise<void> {
    if (!this.#available) throw new Error("Paseo CLI is unavailable for late agent cleanup");
    const { stdout } = await execFileAsync(this.executable, ["archive", id, "--force", "--host", this.host, "--json"], { timeout: timeoutMs, maxBuffer: 64 * 1024, encoding: "utf8", windowsHide: true });
    const result = JSON.parse(stdout) as { status?: unknown };
    if (result.status !== "archived") throw new Error("Paseo archive returned an invalid response");
  }
}

export class SdkBackend implements PaseoBackend {
  readonly #client: PaseoClient;
  readonly #cli: PaseoCli | null;

  constructor(options: { url: string; password?: string; connectTimeoutMs: number; cliPath?: string | null; cliHost?: string | null }) {
    const cliPath = options.cliPath === undefined ? "paseo" : options.cliPath;
    const cliHost = options.cliHost === undefined ? inferCliHost(options.url) : options.cliHost;
    this.#cli = cliPath === null || cliHost === null ? null : new PaseoCli(cliPath, cliHost);
    this.#client = createPaseoClient({
      url: options.url,
      clientId: "lvu-paseo-bridge",
      appVersion: "0.1.0",
      connectTimeoutMs: options.connectTimeoutMs,
      reconnect: { enabled: false },
      ...(options.password === undefined ? {} : { password: options.password }),
    });
  }
  get supportsRemoteCancel(): boolean { return this.#cli?.available ?? false; }

  async connect(): Promise<void> {
    await this.#client.connect();
    await this.#cli?.probe(2_000).catch(() => {});
  }
  close(): Promise<void> { return this.#client.close(); }
  async createAgent(options: CreateAgentOptions): Promise<AgentHandle> {
    const agent = await this.#client.agents.create({
      config: { provider: options.provider, ...(options.modeId === undefined ? {} : { modeId: options.modeId }), ...(options.thinkingOptionId === undefined ? {} : { thinkingOptionId: options.thinkingOptionId }) },
      cwd: options.cwd,
      ...(options.title === undefined ? {} : { title: options.title }),
    });
    return wrapAgent(agent);
  }
  refAgent(id: string): AgentHandle { return wrapAgent(this.#client.agents.ref(id)); }
  async listProviders(): Promise<Array<{ provider: string; status: string; enabled: boolean }>> {
    const snapshot = await this.#client.providers.waitForReady({ timeoutMs: 10_000 });
    return snapshot.entries.map(({ provider, status, enabled }) => ({ provider, status, enabled }));
  }
  async cancelAgent(id: string, timeoutMs: number): Promise<boolean> {
    return await this.#cli?.cancel(id, timeoutMs) ?? false;
  }
  async cleanupOwnedAgent(agent: AgentHandle, timeoutMs: number): Promise<void> {
    if (this.#cli?.available) return this.#cli.archive(agent.id, timeoutMs);
    await agent.archive();
  }
}

function inferCliHost(url: string): string | null {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "ws:" || parsed.pathname !== "/ws") return null;
    return parsed.host;
  } catch { return null; }
}
