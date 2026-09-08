import { describe, expect, it, vi } from "vitest";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bridge } from "../src/bridge.js";
import { OwnedSessionLedger } from "../src/owned_sessions.js";
import { requestSchema } from "../src/protocol.js";
import type { AgentSnapshot } from "../src/backend.js";
import { deferred, FakeBackend, snapshot } from "./fake-backend.js";

const limits = { maxSessions: 2, defaultTimeoutMs: 50, remoteCancelTimeoutMs: 20, maxProposalBytes: 2048, maxEventBytes: 2048 };
const base = { schema_version: 1 as const };
const revision = { data: "watermark-7", definition: "view-3" };
const response = (output: Array<Record<string, unknown>>, id: string) => output.find((message) => message.request_id === id && typeof message.ok === "boolean");
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));
async function waitUntil(predicate: () => boolean): Promise<void> { for (let index = 0; index < 100 && !predicate(); index++) await new Promise((resolve) => setTimeout(resolve, 1)); }
function harness(customLimits = limits) { const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), customLimits); return { backend, output, bridge }; }
async function startOne(h: ReturnType<typeof harness>, extras: Record<string, unknown> = {}) { await h.bridge.start(); await h.bridge.handle(requestSchema.parse({ ...base, request_id: "start", method: "start_session", provider: "fake/model", cwd: "/tmp/investigation", ...extras })); }
function proposalRequest(id = "proposal") { return requestSchema.parse({ ...base, request_id: id, method: "request_proposal", session_id: "agent-1", kind: "filter", instruction: "errors only", originating_revision: revision, context: { manifest_path: "/tmp/i/manifest.json", dataset_paths: ["/tmp/i/sample.parquet"] } }); }
function validFilter() { return JSON.stringify({ kind: "filter", definition: { schema_version: 1, expression: "pl.col('level') == 'error'" }, explanation: "Select errors", originating_revision: revision }); }

describe("Bridge lifecycle", () => {
  it("cannot reopen when connect resolves after close", async () => {
    const h = harness(); const connecting = deferred<void>(); h.backend.connectResult = connecting;
    const start = h.bridge.start(); const close = h.bridge.close(); connecting.resolve();
    await expect(start).rejects.toMatchObject({ code: "BRIDGE_CLOSED" }); await close;
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "late", method: "capabilities" }));
    expect(response(h.output, "late")).toMatchObject({ error: { code: "BRIDGE_NOT_OPEN" } });
    expect(h.backend.closeCalls).toBeGreaterThanOrEqual(1);
  });

  it("gates requests before connection and after close", async () => {
    const h = harness();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "early", method: "capabilities" }));
    await h.bridge.start();
    await h.bridge.close();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "late", method: "capabilities" }));
    expect(response(h.output, "early")).toMatchObject({ error: { code: "BRIDGE_NOT_OPEN" } });
    expect(response(h.output, "late")).toMatchObject({ error: { code: "BRIDGE_NOT_OPEN" } });
  });

  it("reserves capacity across concurrent creates", async () => {
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; await h.bridge.start();
    const first = h.bridge.handle(requestSchema.parse({ ...base, request_id: "one", method: "start_session", provider: "fake", cwd: "/tmp" }));
    const second = h.bridge.handle(requestSchema.parse({ ...base, request_id: "two", method: "start_session", provider: "fake", cwd: "/tmp" }));
    await second;
    expect(response(h.output, "two")).toMatchObject({ error: { code: "LIMIT_EXCEEDED" } });
    h.backend.createDeferred[0]!.resolve(h.backend.agents.get("agent-1")!); await first;
    expect(h.backend.createCalls).toHaveLength(1);
  });

  it("names the daemon, the provider and the authenticated alternatives when a session cannot start", async () => {
    // The user-visible report was `local agent service: bridge is not running`
    // for every one of these; each has a different remedy.
    const unreachable = harness();
    unreachable.backend.createError = new Error("Daemon client closed");
    unreachable.backend.providersError = new Error("Daemon client closed");
    await unreachable.bridge.start();
    await unreachable.bridge.handle(requestSchema.parse({ ...base, request_id: "d", method: "start_session", provider: "codex/model", cwd: "/tmp" }));
    expect(response(unreachable.output, "d")).toMatchObject({ error: { code: "DAEMON_UNREACHABLE" } });
    expect(String((response(unreachable.output, "d") as { error: { message: string } }).error.message)).toContain("cannot reach the Paseo daemon");

    const unknown = harness();
    unknown.backend.createError = new Error("provider rejected");
    unknown.backend.providers = [{ provider: "claude", status: "ready", enabled: true }];
    await unknown.bridge.start();
    await unknown.bridge.handle(requestSchema.parse({ ...base, request_id: "u", method: "start_session", provider: "codex/model", cwd: "/tmp" }));
    expect(response(unknown.output, "u")).toMatchObject({ error: { code: "PROVIDER_UNKNOWN", message: expect.stringContaining("claude") } });

    const unauthenticated = harness();
    unauthenticated.backend.createError = new Error("provider rejected");
    unauthenticated.backend.providers = [{ provider: "codex", status: "unauthenticated", enabled: true }];
    await unauthenticated.bridge.start();
    await unauthenticated.bridge.handle(requestSchema.parse({ ...base, request_id: "a", method: "start_session", provider: "codex/model", cwd: "/tmp" }));
    expect(response(unauthenticated.output, "a")).toMatchObject({ error: { code: "PROVIDER_UNAVAILABLE", message: expect.stringContaining("no other provider is authenticated") } });

    // A healthy provider leaves the original create failure intact.
    const other = harness();
    other.backend.createError = new Error("workspace is read-only");
    await other.bridge.start();
    await other.bridge.handle(requestSchema.parse({ ...base, request_id: "o", method: "start_session", provider: "fake/model", cwd: "/tmp" }));
    expect(response(other.output, "o")).toMatchObject({ error: { message: "workspace is read-only" } });
  });

  it("reports capabilities providers with a classified code when the daemon is gone", async () => {
    const h = harness();
    h.backend.providersError = new Error("Daemon client closed");
    await h.bridge.start();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "cap", method: "capabilities" }));
    expect(response(h.output, "cap")).toMatchObject({ result: { providers: { code: "DAEMON_UNREACHABLE" } } });
  });

  it("reserves capacity across concurrent resumes", async () => {
    const h = harness({ ...limits, maxSessions: 1 }); await h.bridge.start();
    h.backend.refAgent("resume-a"); h.backend.refAgent("resume-b"); const a = h.backend.agents.get("resume-a")!; const b = h.backend.agents.get("resume-b")!;
    const da = deferred<AgentSnapshot>(); const db = deferred<AgentSnapshot>(); a.refreshResult = da; b.refreshResult = db;
    const first = h.bridge.handle(requestSchema.parse({ ...base, request_id: "ra", method: "resume_session", session_id: "resume-a" }));
    const second = h.bridge.handle(requestSchema.parse({ ...base, request_id: "rb", method: "resume_session", session_id: "resume-b" }));
    await second; expect(response(h.output, "rb")).toMatchObject({ error: { code: "LIMIT_EXCEEDED" } });
    da.resolve(snapshot()); await first; db.resolve(snapshot());
  });

  it("coalesces concurrent resumes of the same session", async () => {
    const h = harness({ ...limits, maxSessions: 1 }); await h.bridge.start(); h.backend.refAgent("same");
    const agent = h.backend.agents.get("same")!; const refreshing = deferred<AgentSnapshot>(); agent.refreshResult = refreshing;
    const first = h.bridge.handle(requestSchema.parse({ ...base, request_id: "one", method: "resume_session", session_id: "same" }));
    const second = h.bridge.handle(requestSchema.parse({ ...base, request_id: "two", method: "resume_session", session_id: "same" }));
    refreshing.resolve(snapshot()); await Promise.all([first, second]);
    expect(agent.streamListeners.size).toBe(1); expect(agent.updateListeners.size).toBe(1);
    expect(response(h.output, "one")).toMatchObject({ ok: true }); expect(response(h.output, "two")).toMatchObject({ ok: true });
  });

  it("does not attach a create that finishes after close", async () => {
    const h = harness(); h.backend.deferCreates = true; await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "create", method: "start_session", provider: "fake", cwd: "/tmp" }));
    const closing = h.bridge.close();
    const agent = h.backend.agents.get("agent-1")!; h.backend.createDeferred[0]!.resolve(agent);
    await Promise.all([creating, closing]);
    expect(agent.streamListeners.size).toBe(0); expect(agent.updateListeners.size).toBe(0);
    expect(response(h.output, "create")).toMatchObject({ error: { code: "BRIDGE_CLOSED" } });
    expect(agent.archived).toBe(true);
  });

  it("archives a bridge-owned create that settles after its deadline", async () => {
    vi.useFakeTimers();
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "create", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 10 }));
    await vi.advanceTimersByTimeAsync(11); await creating;
    const agent = h.backend.agents.get("agent-1")!; h.backend.createDeferred[0]!.resolve(agent); await vi.runAllTimersAsync();
    expect(agent.archived).toBe(true); expect(response(h.output, "create")).toMatchObject({ error: { code: "TIMEOUT" } });
    h.backend.deferCreates = false;
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "replacement", method: "start_session", provider: "fake", cwd: "/tmp" }));
    expect(response(h.output, "replacement")).toMatchObject({ ok: true });
    vi.useRealTimers();
  });

  it("retains capacity while a timed-out create never settles", async () => {
    vi.useFakeTimers();
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "stuck", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 5 }));
    await vi.advanceTimersByTimeAsync(6); await creating;
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "blocked", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 5 }));
    expect(response(h.output, "stuck")).toMatchObject({ error: { code: "TIMEOUT" } });
    expect(response(h.output, "blocked")).toMatchObject({ error: { code: "LIMIT_EXCEEDED" } });
    expect(h.backend.createCalls).toHaveLength(1);
    vi.useRealTimers();
  });

  it("releases timed-out create capacity after a late rejection", async () => {
    vi.useFakeTimers();
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "rejected", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 5 }));
    await vi.advanceTimersByTimeAsync(6); await creating;
    h.backend.createDeferred[0]!.reject(new Error("late create failure")); await vi.runAllTimersAsync();
    h.backend.deferCreates = false;
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "replacement", method: "start_session", provider: "fake", cwd: "/tmp" }));
    expect(response(h.output, "replacement")).toMatchObject({ ok: true });
    expect(h.output).toContainEqual(expect.objectContaining({ kind: "create_reconciliation_released", result: "create_rejected" }));
    vi.useRealTimers();
  });

  it("retains capacity and reports a failed late-owned cleanup", async () => {
    vi.useFakeTimers();
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; h.backend.cleanupResult = new Error("archive unavailable"); await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "cleanup", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 5 }));
    await vi.advanceTimersByTimeAsync(6); await creating;
    h.backend.createDeferred[0]!.resolve(h.backend.agents.get("agent-1")!); await vi.runAllTimersAsync();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "blocked", method: "start_session", provider: "fake", cwd: "/tmp" }));
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "capabilities", method: "capabilities" }));
    expect(response(h.output, "blocked")).toMatchObject({ error: { code: "LIMIT_EXCEEDED" } });
    expect(response(h.output, "capabilities")).toMatchObject({ result: { create_reconciliation: { pending: 0, cleanup_failed: 1 } } });
    expect(h.output).toContainEqual(expect.objectContaining({ kind: "owned_cleanup_failed", request_id: "cleanup", error: "archive unavailable" }));
    expect(h.backend.cleanupCalls).toEqual(["agent-1"]);
    vi.useRealTimers();
  });

  it("closes within its deadline with an unresolved create and reports it", async () => {
    vi.useFakeTimers();
    const h = harness({ ...limits, maxSessions: 1 }); h.backend.deferCreates = true; await h.bridge.start();
    const creating = h.bridge.handle(requestSchema.parse({ ...base, request_id: "stuck", method: "start_session", provider: "fake", cwd: "/tmp", timeout_ms: 5 }));
    await vi.advanceTimersByTimeAsync(6); await creating;
    await h.bridge.close();
    expect(h.output).toContainEqual(expect.objectContaining({ kind: "create_reconciliation_unresolved", requests: [{ request_id: "stuck", state: "creating" }] }));
    vi.useRealTimers();
  });

  it("does not attach a refresh that finishes after close", async () => {
    const h = harness(); await h.bridge.start(); h.backend.refAgent("resume-late");
    const agent = h.backend.agents.get("resume-late")!; const refreshing = deferred<AgentSnapshot>(); agent.refreshResult = refreshing;
    const resume = h.bridge.handle(requestSchema.parse({ ...base, request_id: "resume", method: "resume_session", session_id: "resume-late" }));
    const closing = h.bridge.close(); refreshing.resolve(snapshot());
    await Promise.all([resume, closing]);
    expect(agent.streamListeners.size).toBe(0); expect(agent.updateListeners.size).toBe(0);
    expect(response(h.output, "resume")).toMatchObject({ error: { code: "BRIDGE_CLOSED" } });
  });

  it("preserves remote-busy state when resuming a running agent", async () => {
    const h = harness(); await h.bridge.start(); h.backend.refAgent("running");
    h.backend.agents.get("running")!.refreshResult = snapshot({ running: true, status: "running" });
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "resume", method: "resume_session", session_id: "running" }));
    agentUpdate(h.backend.agents.get("running")!, "idle");
    expect(h.output.find((message) => message.kind === "session_update")).toBeDefined();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "send", method: "send_prompt", session_id: "running", prompt: "new" }));
    expect(response(h.output, "send")).toMatchObject({ ok: true });
  });
});

describe("turn observation", () => {
  it("passes exact model settings and streams only during an observed turn", async () => {
    const h = harness(); await startOne(h, { mode_id: "full-access", thinking_option_id: "medium", prompt: "inspect" });
    expect(h.backend.createCalls[0]).toMatchObject({ provider: "fake/model", modeId: "full-access", thinkingOptionId: "medium" });
    const agent = h.backend.agents.get("agent-1")!; agent.stream({ delta: "hello" }); agent.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" }); await tick();
    agent.stream({ delta: "late" });
    expect(h.output.filter((message) => message.kind === "stream")).toHaveLength(1);
  });

  it("cancels a pending proposal promptly, fences events, and blocks new sends until remote stop", async () => {
    const h = harness(); await startOne(h); h.backend.cancelResult = deferred<boolean>();
    const proposal = h.bridge.handle(proposalRequest()); await tick();
    const cancel = h.bridge.handle(requestSchema.parse({ ...base, request_id: "cancel", method: "cancel", session_id: "agent-1" }));
    await proposal;
    expect(response(h.output, "proposal")).toMatchObject({ error: { code: "CANCELLED" } });
    h.backend.agents.get("agent-1")!.stream({ delta: "after cancel" }); agentUpdate(h.backend.agents.get("agent-1")!, "running");
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "send", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "send")).toMatchObject({ error: { code: "SESSION_BUSY" } });
    (h.backend.cancelResult as ReturnType<typeof deferred<boolean>>).resolve(true); await cancel;
    expect(response(h.output, "cancel")).toMatchObject({ result: { remote_cancelled: true, remote_agent_may_still_be_running: false } });
    expect(h.output.filter((message) => message.kind === "stream")).toHaveLength(0);
    expect(h.output.filter((message) => message.kind === "session_update")).toHaveLength(0);
  });

  it("keeps a session busy when remote cancel is unavailable", async () => {
    const h = harness(); h.backend.supportsRemoteCancel = false; await startOne(h, { prompt: "slow" });
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "cancel", method: "cancel", session_id: "agent-1" }));
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "send", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "cancel")).toMatchObject({ result: { remote_cancelled: false, remote_agent_may_still_be_running: true } });
    expect(response(h.output, "send")).toMatchObject({ error: { code: "SESSION_BUSY" } });
  });

  it.each(["timeout", "permission"] as const)("keeps resolved SDK %s results busy while the authoritative status is running", async (status) => {
    const h = harness(); await startOne(h, { prompt: "slow" }); const agent = h.backend.agents.get("agent-1")!;
    agent.runs[0]!.resolve({ status, error: null, lastMessage: null, agentStatus: "running" }); await tick();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "send", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "send")).toMatchObject({ error: { code: "SESSION_BUSY" } });
    agentUpdate(agent, "idle");
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "after-idle", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "after-idle")).toMatchObject({ ok: true });
  });

  it("keeps a new generation blocked until a timed-out cancellation command settles", async () => {
    vi.useFakeTimers();
    const h = harness(); await startOne(h, { prompt: "slow" }); const agent = h.backend.agents.get("agent-1")!;
    const cancellation = deferred<boolean>(); h.backend.cancelResult = cancellation;
    const cancel = h.bridge.handle(requestSchema.parse({ ...base, request_id: "cancel", method: "cancel", session_id: "agent-1" }));
    await vi.advanceTimersByTimeAsync(21); await cancel; agentUpdate(agent, "idle");
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "blocked", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "blocked")).toMatchObject({ error: { code: "SESSION_BUSY" } });
    cancellation.resolve(false); await Promise.resolve(); await Promise.resolve();
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "allowed", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "allowed")).toMatchObject({ ok: true });
    vi.useRealTimers();
  });

  it("bounds proposal lifetime even when the SDK promise never resolves", async () => {
    vi.useFakeTimers();
    const h = harness(); await startOne(h);
    const proposal = h.bridge.handle(proposalRequest("timeout"));
    await vi.advanceTimersByTimeAsync(51); await proposal;
    expect(response(h.output, "timeout")).toMatchObject({ error: { code: "TIMEOUT" } });
    await h.bridge.handle(requestSchema.parse({ ...base, request_id: "send", method: "send_prompt", session_id: "agent-1", prompt: "new" }));
    expect(response(h.output, "send")).toMatchObject({ error: { code: "SESSION_BUSY" } });
    vi.useRealTimers();
  });

  it("validates a returned proposal and concrete supplied schema", async () => {
    const h = harness(); await startOne(h); const pending = h.bridge.handle(proposalRequest()); await tick();
    const run = h.backend.agents.get("agent-1")!.runs[0]!;
    expect(run.options.outputSchema).toMatchObject({ properties: { definition: { required: ["schema_version", "expression"] } } });
    expect(run.options.outputSchema).toMatchObject({ properties: { originating_revision: { properties: { data: { const: revision.data }, definition: { const: revision.definition } } } } });
    const inlineSchema = JSON.parse(run.prompt.split("JSON schema: ")[1]!);
    expect(inlineSchema).toEqual(run.options.outputSchema);
    expect(run.prompt).toContain("do not return only the inner definition");
    expect(run.prompt).toContain("Parquet");
    expect(run.prompt).toContain("128 rows per source and 512 total");
    expect(run.prompt).toContain("actual rows/sources inspected");
    expect(run.prompt).toContain("Do not regex-parse JSON raw");
    expect(run.prompt).toContain("do not assume any particular input field name");
    run.resolve({ status: "idle", error: null, lastMessage: validFilter(), agentStatus: "idle" }); await pending;
    expect(response(h.output, "proposal")).toMatchObject({ ok: true, result: { proposal: { kind: "filter" } } });
  });

  it("rejects a stale proposal even though the outgoing schema is revision-bound", async () => {
    const h = harness(); await startOne(h); const pending = h.bridge.handle(proposalRequest("stale")); await tick();
    const stale = JSON.parse(validFilter()) as { originating_revision: { definition: string } };
    stale.originating_revision.definition = "view-2";
    h.backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: JSON.stringify(stale), agentStatus: "idle" });
    await pending;
    expect(response(h.output, "stale")).toMatchObject({ error: { code: "INVALID_PROPOSAL", message: "proposal revision does not match the requested revision" } });
  });

  it("surfaces rejected SDK work and removes subscriptions on shutdown", async () => {
    const h = harness(); await startOne(h, { prompt: "go" }); const agent = h.backend.agents.get("agent-1")!;
    agent.runs[0]!.reject(new Error("socket disconnected")); await tick();
    expect(h.output.find((message) => message.kind === "turn_failed")).toMatchObject({ error: "socket disconnected" });
    await h.bridge.close(); expect(agent.streamListeners.size).toBe(0); expect(agent.updateListeners.size).toBe(0);
  });
});

describe("managed assistance lifecycle", () => {
  it("does not archive an initial idle subscription update before the owned request", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); backend.immediateUpdateOnCreate = { kind: "upsert", agent: { status: "idle" } };
      const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), limits, new OwnedSessionLedger(root)); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(backend.cleanupCalls).toEqual([]);
      const proposal = bridge.handle(proposalRequest()); await tick();
      expect(backend.agents.get("agent-1")!.runs).toHaveLength(1);
      backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: validFilter(), agentStatus: "idle" }); await proposal; await bridge.close();
      expect(response(output, "proposal")).toMatchObject({ ok: true }); expect(backend.cleanupCalls).toEqual(["agent-1"]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("waits for the full run result after an early idle update and logs it before archive", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = []; const ledger = new OwnedSessionLedger(root);
      const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      const proposal = bridge.handle(proposalRequest()); await tick(); const agent = backend.agents.get("agent-1")!;
      agent.update({ kind: "upsert", agent: { status: "idle" } }); await tick();
      expect(backend.cleanupCalls).toEqual([]);
      await bridge.handle(requestSchema.parse({ ...base, request_id: "too-early", method: "send_prompt", session_id: "agent-1", prompt: "again" }));
      expect(response(output, "too-early")).toMatchObject({ error: { code: "SESSION_BUSY" } });
      agent.runs[0]!.resolve({ status: "idle", error: null, lastMessage: validFilter(), agentStatus: "idle" }); await proposal; await bridge.close();
      const record = await ledger.readByAgent("agent-1"); const activity = await readFile(ledger.activityPath(record!), "utf8");
      expect(activity.indexOf("run_settled")).toBeGreaterThanOrEqual(0); expect(activity.indexOf("cleanup_requested")).toBeGreaterThan(activity.indexOf("run_settled"));
      expect(backend.cleanupCalls).toEqual(["agent-1"]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("creates ephemeral Ask work in the stable workspace and archives only after persisted terminal activity", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = [];
      const ledger = new OwnedSessionLedger(root); const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger);
      await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake/model", cwd: "/ignored", purpose: "ask" }));
      expect(backend.ensureWorkspaceCalls).toEqual([{ root }]);
      expect(backend.createCalls[0]).toMatchObject({ cwd: root, workspaceId: "workspace-1", labels: { "lvu.lifecycle": "ephemeral", "lvu.purpose": "ask" } });
      const proposal = bridge.handle(proposalRequest()); await tick();
      backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: validFilter(), agentStatus: "idle" });
      await proposal;
      await bridge.close();
      expect(backend.cleanupCalls).toEqual(["agent-1"]);
      expect(output).toContainEqual(expect.objectContaining({ kind: "session_archived", activity_path: expect.stringContaining("/activity/") }));
      const record = await ledger.readByAgent("agent-1");
      expect(record).toMatchObject({ purpose: "ask", lifecycle: "ephemeral", state: "archived", workspaceId: "workspace-1" });
      expect((await readFile(ledger.activityPath(record!), "utf8"))).toContain("run_settled");
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("persists and reuses the verified workspace id without creating a second placement", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const ledger = new OwnedSessionLedger(root);
      const first = new FakeBackend(); const bridge1 = new Bridge(first, () => {}, limits, ledger); await bridge1.start();
      await bridge1.handle(requestSchema.parse({ ...base, request_id: "one", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "investigation" })); await bridge1.close();
      const second = new FakeBackend(); const bridge2 = new Bridge(second, () => {}, limits, new OwnedSessionLedger(root)); await bridge2.start();
      await bridge2.handle(requestSchema.parse({ ...base, request_id: "two", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "investigation" }));
      expect(second.ensureWorkspaceCalls).toEqual([{ root, storedId: "workspace-1" }]);
      await bridge2.close();
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("does not archive an ephemeral session after observation-only cancellation", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); backend.supportsRemoteCancel = false;
      const ledger = new OwnedSessionLedger(root); const bridge = new Bridge(backend, () => {}, limits, ledger); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", prompt: "slow" }));
      await bridge.handle(requestSchema.parse({ ...base, request_id: "cancel", method: "cancel", session_id: "agent-1" }));
      await tick(); expect(backend.cleanupCalls).toEqual([]);
      await bridge.close();
      expect(backend.cleanupCalls).toEqual([]);
      expect(await ledger.readByAgent("agent-1")).toMatchObject({ state: "pending_cleanup", lastError: expect.stringContaining("before remote terminal") });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("keeps an owned pending record and emits its activity path when archive fails", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); backend.cleanupResult = new Error("archive refused");
      const output: Array<Record<string, unknown>> = []; const ledger = new OwnedSessionLedger(root);
      const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "source_assistance", prompt: "help" }));
      backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" });
      await bridge.close();
      expect(output).toContainEqual(expect.objectContaining({ kind: "archive_failed", error: "archive refused", activity_path: expect.stringContaining("/activity/") }));
      expect(await ledger.readByAgent("agent-1")).toMatchObject({ state: "archive_failed" });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("rejects another run while managed archive is pending", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); const archive = deferred<void>(); backend.cleanupResult = archive;
      const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), limits, new OwnedSessionLedger(root)); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", prompt: "help" }));
      backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" });
      for (let index = 0; index < 32 && backend.cleanupCalls.length === 0; index++) await tick();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "again", method: "send_prompt", session_id: "agent-1", prompt: "again" }));
      expect(response(output, "again")).toMatchObject({ error: { code: "SESSION_CLOSING" } });
      archive.resolve(); await bridge.close();
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("never archives after owned activity persistence is lost", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = [];
      const ledger = new OwnedSessionLedger(root, { beforeActivityWrite: async () => { throw new Error("activity disk failed"); } });
      const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", prompt: "help" }));
      backend.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" });
      await bridge.close();
      expect(backend.cleanupCalls).toEqual([]);
      expect(output).toContainEqual(expect.objectContaining({ kind: "archive_failed", error: "activity disk failed" }));
      expect(await ledger.readByAgent("agent-1")).toMatchObject({ state: "archive_failed", activityLost: true });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("reconciles only a terminal exact ledger-owned agent after restart", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      await ledger.writeWorkspace({ version: 1, workspaceId: "workspace-1", projectId: "project-1", directory: root });
      const ids = ledger.identifiers(); let record = await ledger.createPending({ ...ids, protocolRequestId: "old", purpose: "ask", lifecycle: "ephemeral" });
      record = await ledger.update(record, { state: "pending_cleanup", workspaceId: "workspace-1", agentId: "owned-agent" });
      const backend = new FakeBackend(); backend.refAgent("owned-agent");
      backend.agents.get("owned-agent")!.refreshResult = snapshot({ workspaceId: "workspace-1", status: "idle" });
      const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), { ...limits, remoteCancelTimeoutMs: 500 }, new OwnedSessionLedger(root));
      await bridge.start();
      for (let index = 0; index < 32 && backend.cleanupCalls.length === 0; index++) await tick();
      await bridge.close();
      expect(backend.cleanupCalls).toEqual(["owned-agent"]);
      expect(output).toContainEqual(expect.objectContaining({ kind: "session_archived", session_id: "owned-agent" }));
      expect(await ledger.read(record.ownershipId)).toMatchObject({ state: "archived" });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("reports an ambiguous durable pending create without discovering or archiving an agent", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      await ledger.writeWorkspace({ version: 1, workspaceId: "workspace-1", projectId: "project-1", directory: root });
      const ids = ledger.identifiers(); await ledger.createPending({ ...ids, protocolRequestId: "lost-create", purpose: "ask", lifecycle: "ephemeral" });
      const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), { ...limits, remoteCancelTimeoutMs: 500 }, new OwnedSessionLedger(root));
      await bridge.start();
      await bridge.close();
      expect(output).toContainEqual(expect.objectContaining({ kind: "owned_create_unresolved", request_id: "lost-create" }));
      expect(backend.agents.size).toBe(0); expect(backend.cleanupCalls).toEqual([]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("retains a recovered running ephemeral agent until a terminal update then archives it", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      await ledger.writeWorkspace({ version: 1, workspaceId: "workspace-1", projectId: "project-1", directory: root });
      const ids = ledger.identifiers(); let record = await ledger.createPending({ ...ids, protocolRequestId: "active", purpose: "ask", lifecycle: "ephemeral" });
      record = await ledger.update(record, { state: "active", workspaceId: "workspace-1", agentId: "running-owned" });
      const backend = new FakeBackend(); backend.refAgent("running-owned"); const agent = backend.agents.get("running-owned")!;
      agent.refreshResult = snapshot({ workspaceId: "workspace-1", status: "running", running: true });
      const bridge = new Bridge(backend, () => {}, { ...limits, remoteCancelTimeoutMs: 500 }, new OwnedSessionLedger(root)); await bridge.start();
      await waitUntil(() => agent.updateListeners.size === 1); expect(backend.cleanupCalls).toEqual([]);
      agent.update({ kind: "upsert", agent: { status: "idle" } });
      await bridge.close();
      expect(backend.cleanupCalls).toEqual(["running-owned"]);
      expect(await ledger.read(record.ownershipId)).toMatchObject({ state: "archived" });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("becomes ready while bounded recovery refresh remains unresolved", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      await ledger.writeWorkspace({ version: 1, workspaceId: "workspace-1", projectId: "project-1", directory: root });
      const ids = ledger.identifiers(); let record = await ledger.createPending({ ...ids, protocolRequestId: "stalled", purpose: "ask", lifecycle: "ephemeral" });
      record = await ledger.update(record, { state: "active", workspaceId: "workspace-1", agentId: "stalled-agent" });
      const backend = new FakeBackend(); backend.refAgent("stalled-agent"); backend.agents.get("stalled-agent")!.refreshResult = deferred<AgentSnapshot>();
      const output: Array<Record<string, unknown>> = []; const bridge = new Bridge(backend, (message) => output.push(message), { ...limits, remoteCancelTimeoutMs: 10 }, new OwnedSessionLedger(root));
      await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ready", method: "capabilities" }));
      expect(response(output, "ready")).toMatchObject({ ok: true });
      await bridge.close();
      expect(await ledger.read(record.ownershipId)).toMatchObject({ state: "active" });
      expect(backend.cleanupCalls).toEqual([]);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("refuses managed ephemeral resume while retaining legacy resume", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-"));
    try {
      const backend = new FakeBackend(); const output: Array<Record<string, unknown>> = []; const ledger = new OwnedSessionLedger(root);
      const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger); await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      await bridge.close();
      const resumed = new Bridge(backend, (message) => output.push(message), limits, new OwnedSessionLedger(root)); await resumed.start();
      await resumed.handle(requestSchema.parse({ ...base, request_id: "resume", method: "resume_session", session_id: "agent-1", purpose: "ask" }));
      expect(response(output, "resume")).toMatchObject({ error: { code: "NOT_RESUMABLE" } });
      await resumed.handle(requestSchema.parse({ ...base, request_id: "legacy", method: "resume_session", session_id: "unowned" }));
      expect(response(output, "legacy")).toMatchObject({ ok: true });
      await resumed.close();
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});

function agentUpdate(agent: { update(value: unknown): void }, status: string): void { agent.update({ kind: "upsert", agent: { status } }); }
