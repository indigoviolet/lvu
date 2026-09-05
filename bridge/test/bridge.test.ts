import { describe, expect, it, vi } from "vitest";
import { Bridge } from "../src/bridge.js";
import { requestSchema } from "../src/protocol.js";
import { deferred, FakeBackend } from "./fake-backend.js";

const limits = { maxSessions: 2, defaultTimeoutMs: 50, remoteCancelTimeoutMs: 20, maxProposalBytes: 2048, maxEventBytes: 2048 };
const base = { schema_version: 1 as const };
const revision = { data: "watermark-7", definition: "view-3" };
const response = (output: Array<Record<string, unknown>>, id: string) => output.find((message) => message.request_id === id && typeof message.ok === "boolean");
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));
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

  it("reserves capacity across concurrent resumes", async () => {
    const h = harness({ ...limits, maxSessions: 1 }); await h.bridge.start();
    h.backend.refAgent("resume-a"); h.backend.refAgent("resume-b"); const a = h.backend.agents.get("resume-a")!; const b = h.backend.agents.get("resume-b")!;
    const da = deferred<{ exists: boolean; running: boolean }>(); const db = deferred<{ exists: boolean; running: boolean }>(); a.refreshResult = da; b.refreshResult = db;
    const first = h.bridge.handle(requestSchema.parse({ ...base, request_id: "ra", method: "resume_session", session_id: "resume-a" }));
    const second = h.bridge.handle(requestSchema.parse({ ...base, request_id: "rb", method: "resume_session", session_id: "resume-b" }));
    await second; expect(response(h.output, "rb")).toMatchObject({ error: { code: "LIMIT_EXCEEDED" } });
    da.resolve({ exists: true, running: false }); await first; db.resolve({ exists: true, running: false });
  });

  it("coalesces concurrent resumes of the same session", async () => {
    const h = harness({ ...limits, maxSessions: 1 }); await h.bridge.start(); h.backend.refAgent("same");
    const agent = h.backend.agents.get("same")!; const refreshing = deferred<{ exists: boolean; running: boolean }>(); agent.refreshResult = refreshing;
    const first = h.bridge.handle(requestSchema.parse({ ...base, request_id: "one", method: "resume_session", session_id: "same" }));
    const second = h.bridge.handle(requestSchema.parse({ ...base, request_id: "two", method: "resume_session", session_id: "same" }));
    refreshing.resolve({ exists: true, running: false }); await Promise.all([first, second]);
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
    const agent = h.backend.agents.get("resume-late")!; const refreshing = deferred<{ exists: boolean; running: boolean }>(); agent.refreshResult = refreshing;
    const resume = h.bridge.handle(requestSchema.parse({ ...base, request_id: "resume", method: "resume_session", session_id: "resume-late" }));
    const closing = h.bridge.close(); refreshing.resolve({ exists: true, running: false });
    await Promise.all([resume, closing]);
    expect(agent.streamListeners.size).toBe(0); expect(agent.updateListeners.size).toBe(0);
    expect(response(h.output, "resume")).toMatchObject({ error: { code: "BRIDGE_CLOSED" } });
  });

  it("preserves remote-busy state when resuming a running agent", async () => {
    const h = harness(); await h.bridge.start(); h.backend.refAgent("running");
    h.backend.agents.get("running")!.refreshResult = { exists: true, running: true };
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
    const inlineSchema = JSON.parse(run.prompt.split("JSON schema: ")[1]!);
    expect(inlineSchema).toEqual(run.options.outputSchema);
    expect(run.prompt).toContain("do not return just the expression");
    expect(run.prompt).toContain("Parquet");
    run.resolve({ status: "idle", error: null, lastMessage: validFilter(), agentStatus: "idle" }); await pending;
    expect(response(h.output, "proposal")).toMatchObject({ ok: true, result: { proposal: { kind: "filter" } } });
  });

  it("surfaces rejected SDK work and removes subscriptions on shutdown", async () => {
    const h = harness(); await startOne(h, { prompt: "go" }); const agent = h.backend.agents.get("agent-1")!;
    agent.runs[0]!.reject(new Error("socket disconnected")); await tick();
    expect(h.output.find((message) => message.kind === "turn_failed")).toMatchObject({ error: "socket disconnected" });
    await h.bridge.close(); expect(agent.streamListeners.size).toBe(0); expect(agent.updateListeners.size).toBe(0);
  });
});

function agentUpdate(agent: { update(value: unknown): void }, status: string): void { agent.update({ kind: "upsert", agent: { status } }); }
