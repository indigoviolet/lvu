import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { Bridge } from "../src/bridge.js";
import { OwnedSessionLedger } from "../src/owned_sessions.js";
import { requestSchema } from "../src/protocol.js";
import { deferred, FakeBackend } from "./fake-backend.js";

const limits = { maxSessions: 2, defaultTimeoutMs: 200, remoteCancelTimeoutMs: 500, maxProposalBytes: 2048, maxEventBytes: 2048 };
const base = { schema_version: 1 as const };
const response = (output: Array<Record<string, unknown>>, id: string) => output.find((message) => message.request_id === id && typeof message.ok === "boolean");
const tick = () => new Promise<void>((resolve) => setImmediate(resolve));
async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadlineAt = Date.now() + 2000;
  while (!predicate()) {
    if (Date.now() >= deadlineAt) throw new Error("timed out waiting for multiwindow lease handoff");
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}
// Startup order establishes no owner: either window may transiently hold the
// fresh lease, so the first managed request retries (bounded) until its
// window wins with a live session, exactly like production busy retry.
// Failed attempts leave no ledger state: entry throws before any mutation.
async function startOwnedUntilOk(
  bridge: Bridge,
  output: Array<Record<string, unknown>>,
  baseId: string,
  body: Record<string, unknown>,
): Promise<string> {
  for (let attempt = 0; ; attempt++) {
    const requestId = `${baseId}-try-${attempt}`;
    await bridge.handle(requestSchema.parse({ ...base, request_id: requestId, ...body }));
    const res = response(output, requestId) as { ok?: unknown } | undefined;
    if (res?.ok === true) return requestId;
    if (attempt >= 20) throw new Error(`managed start never acquired for ${baseId}; last: ${JSON.stringify(res)}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}
// Ownership release nulls the in-memory flag before the lock-file unlink
// completes, so absence of the file (not just the flag) is the no-stale-lock
// invariant a second window's acquisition actually observes.
async function waitForLockGone(root: string): Promise<void> {
  const deadlineAt = Date.now() + 2000;
  for (;;) {
    const content = await readFile(join(root, "bridge.lock"), "utf8").catch((error: unknown) => error);
    if (typeof content !== "string") {
      if ((content as { code?: unknown }).code === "ENOENT") return;
      throw content;
    }
    if (Date.now() >= deadlineAt) throw new Error(`timed out waiting for bridge.lock removal, still holds: ${content}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe("owned multiwindow standby", () => {
  it("finishes startup recovery before creating a new ephemeral agent", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-recovery-order-"));
    const gate = deferred<void>();
    const ledger = new OwnedSessionLedger(root);
    const recover = ledger.recoveryBatch.bind(ledger);
    ledger.recoveryBatch = async (limit) => { await gate.promise; return recover(limit); };
    const output: Array<Record<string, unknown>> = [];
    const backend = new FakeBackend();
    const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger);
    try {
      await bridge.start();
      const pending = bridge.handle(requestSchema.parse({ ...base, request_id: "new", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      await tick(); await tick();
      expect(backend.createCalls).toHaveLength(0);
      gate.resolve();
      await pending;
      expect(response(output, "new")).toMatchObject({ ok: true });
      expect(backend.cleanupCalls).toEqual([]);
      expect([...backend.agents.values()][0]!.archived).toBe(false);
    } finally {
      gate.resolve();
      await bridge.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("releases a startup acquisition that wins only after timeout and close", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-late-start-"));
    const gate = deferred<void>();
    const ledger = new OwnedSessionLedger(root);
    const acquire = ledger.acquireLease.bind(ledger);
    let acquired = false;
    ledger.acquireLease = async () => { await gate.promise; await acquire(); acquired = true; };
    const bridge = new Bridge(new FakeBackend(), () => {}, { ...limits, defaultTimeoutMs: 40 }, ledger);
    try {
      await bridge.start();
      await bridge.close(); // Must not wait indefinitely for the gated I/O.
      gate.resolve();
      await waitUntil(() => acquired);
      await waitForLockGone(root);
      expect(ledger.ownsLease).toBe(false);
    } finally {
      gate.resolve();
      await bridge.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("keeps ownership through a timed-out workspace operation and prevents late writes after close", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-workspace-timeout-"));
    const gate = deferred<void>();
    const ledger = new OwnedSessionLedger(root);
    const backend = new FakeBackend();
    const placement = backend.ensureWorkspace.bind(backend);
    let entered = false;
    backend.ensureWorkspace = async (path, id) => { entered = true; await gate.promise; return placement(path, id); };
    const output: Array<Record<string, unknown>> = [];
    const bridge = new Bridge(backend, (message) => output.push(message), limits, ledger);
    try {
      await bridge.start();
      await bridge.handle(requestSchema.parse({ ...base, request_id: "blocked", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", timeout_ms: 40 }));
      expect(entered).toBe(true);
      expect(response(output, "blocked")).toMatchObject({ error: { code: "TIMEOUT" } });
      expect(ledger.ownsLease).toBe(true);
      await bridge.close();
      expect(ledger.ownsLease).toBe(true);
      gate.resolve();
      await waitForLockGone(root);
      expect(await ledger.readWorkspace()).toBeNull();
      expect(backend.createCalls).toHaveLength(0);
    } finally {
      gate.resolve();
      await bridge.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("defers recovery and per-request busy without removing owner state, then serializes after release", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    const ownerOutput: Array<Record<string, unknown>> = [];
    const standbyOutput: Array<Record<string, unknown>> = [];
    const ownerBackend = new FakeBackend();
    const standbyBackend = new FakeBackend();
    const ownerBridge = new Bridge(ownerBackend, (message) => ownerOutput.push(message), limits, new OwnedSessionLedger(root));
    const standbyBridge = new Bridge(standbyBackend, (message) => standbyOutput.push(message), limits, new OwnedSessionLedger(root));
    try {
      await ownerBridge.start();
      await ownerBridge.handle(requestSchema.parse({ ...base, request_id: "auto", method: "start_session", provider: "fake/model", cwd: "/ignored", purpose: "auto_setup", title: "lvu automatic log setup" }));
      expect(response(ownerOutput, "auto")).toMatchObject({ ok: true });

      // A crashed ephemeral create left behind by the owner: standby must not
      // touch it while it holds no lease.
      const ownerLedger = new OwnedSessionLedger(root);
      await ownerLedger.initialize();
      const crashedIds = ownerLedger.identifiers();
      await ownerLedger.createPending({ ...crashedIds, protocolRequestId: "crashed-create", purpose: "ask", lifecycle: "ephemeral" });

      await standbyBridge.start();
      expect(standbyOutput.find((message) => message.kind === "owned_create_unresolved")).toBeUndefined();
      expect(standbyBackend.cleanupCalls).toEqual([]);

      // Managed creation is per-request busy with the exact lock path; the
      // bridge stays open and the owner's lock file is never removed.
      const lockBefore = await readFile(join(root, "bridge.lock"), "utf8");
      await standbyBridge.handle(requestSchema.parse({ ...base, request_id: "busy", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(standbyOutput, "busy")).toMatchObject({ error: { code: "OWNED_ROOT_BUSY" } });
      expect(String((response(standbyOutput, "busy") as { error: { message: string } }).error.message)).toContain(JSON.stringify(join(root, "bridge.lock")));
      expect(await readFile(join(root, "bridge.lock"), "utf8")).toBe(lockBefore);
      // No pending marker was created for the refused request.
      await expect(ownerLedger.read(crashedIds.ownershipId)).resolves.toMatchObject({ state: "pending_create" });

      // Legacy unowned resume stays available without the lease.
      standbyBackend.refAgent("legacy-unowned");
      await standbyBridge.handle(requestSchema.parse({ ...base, request_id: "legacy", method: "resume_session", session_id: "legacy-unowned" }));
      expect(response(standbyOutput, "legacy")).toMatchObject({ ok: true });

      // Managed resume of the owner's resumable conversation is busy, not a
      // second evaluator or a destructive takeover.
      await standbyBridge.handle(requestSchema.parse({ ...base, request_id: "resume-busy", method: "resume_session", session_id: "agent-1", purpose: "auto_setup" }));
      expect(response(standbyOutput, "resume-busy")).toMatchObject({ error: { code: "OWNED_ROOT_BUSY" } });

      // Owner exits normally: its resumable auto_setup conversation is
      // retained (not archived), and the lease is released.
      await ownerBridge.close();
      expect(ownerBackend.cleanupCalls).toEqual([]);
      expect(await ownerLedger.readByAgent("agent-1")).toMatchObject({ purpose: "auto_setup", lifecycle: "resumable", state: "active" });

      // The next managed request on the standby acquires the lease and runs.
      await standbyBridge.handle(requestSchema.parse({ ...base, request_id: "retry", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(standbyOutput, "retry")).toMatchObject({ ok: true });
      expect(await ownerLedger.readByAgent("agent-1")).toMatchObject({ state: "active" });
      await standbyBridge.close();
      await expect(readFile(join(root, "bridge.lock"), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await ownerBridge.close().catch(() => {});
      await standbyBridge.close().catch(() => {});
      await rm(root, { recursive: true, force: true });
    }
  });

  it("hands the lease to a second open window after the first analysis settles, then back", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    const output1: Array<Record<string, unknown>> = [];
    const output2: Array<Record<string, unknown>> = [];
    const backend1 = new FakeBackend();
    const backend2 = new FakeBackend();
    const ledger1 = new OwnedSessionLedger(root);
    const ledger2 = new OwnedSessionLedger(root);
    const bridge1 = new Bridge(backend1, (message) => output1.push(message), limits, ledger1);
    const bridge2 = new Bridge(backend2, (message) => output2.push(message), limits, ledger2);
    const revision = { data: "watermark-7", definition: "view-3" };
    const validAutoSetup = () => JSON.stringify({ kind: "auto_setup", definition: { schema_version: 1, enrichments: [], pinned_columns: [], color_rules: [], grouping: null }, explanation: "No useful enrichment found", originating_revision: revision });
    try {
      await bridge1.start();
      await bridge2.start();
      // Startup order establishes no priority: window 1 retries until it wins
      // with a live session, and only then is exclusion deterministic.
      await startOwnedUntilOk(bridge1, output1, "first-auto", { method: "start_session", provider: "fake/model", cwd: "/ignored", purpose: "auto_setup", title: "lvu automatic log setup" });
      expect(ledger1.ownsLease).toBe(true);
      await bridge2.handle(requestSchema.parse({ ...base, request_id: "second-busy", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(output2, "second-busy")).toMatchObject({ error: { code: "OWNED_ROOT_BUSY" } });
      // The first analysis settles and detaches its observer, keeping the
      // resumable Paseo conversation and ledger entry intact.
      const proposal = bridge1.handle(requestSchema.parse({ ...base, request_id: "first-prop", method: "request_proposal", session_id: "agent-1", kind: "auto_setup", instruction: "set up this log", originating_revision: revision, context: { manifest_path: "/tmp/i/manifest.json", dataset_paths: ["/tmp/i/sample.parquet"] } }));
      await tick();
      backend1.agents.get("agent-1")!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: validAutoSetup(), agentStatus: "idle" });
      await proposal;
      expect(response(output1, "first-prop")).toMatchObject({ ok: true });
      // The now-idle first window lets go without closing: no stale lock left
      // behind, nothing deleted, conversation retained.
      await waitUntil(() => !ledger1.ownsLease);
      await waitForLockGone(root);
      expect(await ledger1.readByAgent("agent-1")).toMatchObject({ purpose: "auto_setup", lifecycle: "resumable", state: "active" });
      // The second window runs Analyze again while the first stays open.
      await bridge2.handle(requestSchema.parse({ ...base, request_id: "second-ask", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", prompt: "help" }));
      expect(response(output2, "second-ask")).toMatchObject({ ok: true });
      const session2 = (response(output2, "second-ask") as { result: { session_id: string } }).result.session_id;
      await waitUntil(() => (backend2.agents.get(session2)?.runs.length ?? 0) > 0);
      backend2.agents.get(session2)!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" });
      await waitUntil(() => backend2.cleanupCalls.length === 1);
      await waitUntil(() => !ledger2.ownsLease);
      await waitForLockGone(root);
      // And the first window reacquires on its next owned request, reloading
      // (not trusting) the workspace placement across the other's tenure.
      const placementsBefore = backend1.ensureWorkspaceCalls.length;
      await bridge1.handle(requestSchema.parse({ ...base, request_id: "first-again", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(output1, "first-again")).toMatchObject({ ok: true });
      expect(ledger1.ownsLease).toBe(true);
      expect(backend1.ensureWorkspaceCalls.length).toBeGreaterThan(placementsBefore);
      await bridge1.close();
      await bridge2.close();
      await expect(readFile(join(root, "bridge.lock"), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await bridge1.close().catch(() => {});
      await bridge2.close().catch(() => {});
      await rm(root, { recursive: true, force: true });
    }
  });

  it("lets either of two idle open windows acquire without the other closing", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    const outputA: Array<Record<string, unknown>> = [];
    const outputB: Array<Record<string, unknown>> = [];
    const backendA = new FakeBackend();
    const backendB = new FakeBackend();
    const ledgerA = new OwnedSessionLedger(root);
    const ledgerB = new OwnedSessionLedger(root);
    const bridgeA = new Bridge(backendA, (message) => outputA.push(message), limits, ledgerA);
    const bridgeB = new Bridge(backendB, (message) => outputB.push(message), limits, ledgerB);
    try {
      await bridgeA.start();
      await bridgeB.start();
      // Automatic work absent in both windows: once startup recovery settles,
      // neither squats the lease.
      await waitUntil(() => !ledgerA.ownsLease && !ledgerB.ownsLease);
      await waitForLockGone(root);
      // A acquires on demand while B stays open but idle.
      await bridgeA.handle(requestSchema.parse({ ...base, request_id: "a-one", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask", prompt: "help" }));
      expect(response(outputA, "a-one")).toMatchObject({ ok: true });
      expect(ledgerA.ownsLease).toBe(true);
      // B is busy only while A's session is live, never because A started first.
      await bridgeB.handle(requestSchema.parse({ ...base, request_id: "b-busy", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(outputB, "b-busy")).toMatchObject({ error: { code: "OWNED_ROOT_BUSY" } });
      // A's ephemeral settles and archives; B acquires with nobody closing.
      const sessionA = (response(outputA, "a-one") as { result: { session_id: string } }).result.session_id;
      await waitUntil(() => (backendA.agents.get(sessionA)?.runs.length ?? 0) > 0);
      backendA.agents.get(sessionA)!.runs[0]!.resolve({ status: "idle", error: null, lastMessage: "done", agentStatus: "idle" });
      await waitUntil(() => backendA.cleanupCalls.length === 1);
      await waitUntil(() => !ledgerA.ownsLease);
      await waitForLockGone(root);
      await bridgeB.handle(requestSchema.parse({ ...base, request_id: "b-two", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      expect(response(outputB, "b-two")).toMatchObject({ ok: true });
      expect(ledgerB.ownsLease).toBe(true);
      await bridgeA.close();
      await bridgeB.close();
      await expect(readFile(join(root, "bridge.lock"), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await bridgeA.close().catch(() => {});
      await bridgeB.close().catch(() => {});
      await rm(root, { recursive: true, force: true });
    }
  });

  it("shares one lazy acquisition across concurrent managed creates", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    const output: Array<Record<string, unknown>> = [];
    const backend = new FakeBackend();
    const bridge = new Bridge(backend, (message) => output.push(message), limits, new OwnedSessionLedger(root));
    const holder = new OwnedSessionLedger(root);
    try {
      await holder.initialize();
      await holder.acquireLease();
      await bridge.start();
      await holder.releaseLease();
      await Promise.all([
        bridge.handle(requestSchema.parse({ ...base, request_id: "one", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" })),
        bridge.handle(requestSchema.parse({ ...base, request_id: "two", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "source_assistance" })),
      ]);
      expect(response(output, "one")).toMatchObject({ ok: true });
      expect(response(output, "two")).toMatchObject({ ok: true });
      expect(backend.createCalls).toHaveLength(2);
      // One shared workspace placement for the single lazy acquisition.
      expect(backend.ensureWorkspaceCalls).toHaveLength(1);
      await bridge.close();
    } finally {
      await bridge.close().catch(() => {});
      await holder.releaseLease().catch(() => {});
      await rm(root, { recursive: true, force: true });
    }
  });

  it("releases a lease won while closing instead of leaking a stale lock", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    const output: Array<Record<string, unknown>> = [];
    const backend = new FakeBackend();
    const standbyLedger = new OwnedSessionLedger(root);
    const bridge = new Bridge(backend, (message) => output.push(message), limits, standbyLedger);
    const holder = new OwnedSessionLedger(root);
    try {
      await holder.initialize();
      await holder.acquireLease();
      await bridge.start();
      const gate = deferred<void>();
      const originalAcquire = standbyLedger.acquireLease.bind(standbyLedger);
      let acquired = false;
      standbyLedger.acquireLease = () => gate.promise.then(() => originalAcquire()).then(() => { acquired = true; });
      const pending = bridge.handle(requestSchema.parse({ ...base, request_id: "racing", method: "start_session", provider: "fake", cwd: "/ignored", purpose: "ask" }));
      await new Promise((resolve) => setImmediate(resolve));
      await bridge.close();
      await holder.releaseLease();
      gate.resolve();
      await pending;
      expect(response(output, "racing")).toMatchObject({ error: { code: "BRIDGE_CLOSED" } });
      await waitUntil(() => acquired);
      await waitForLockGone(root);
    } finally {
      await bridge.close().catch(() => {});
      await holder.releaseLease().catch(() => {});
      await rm(root, { recursive: true, force: true });
    }
  });

  it("tracks exclusive lease ownership without unlinking foreign locks", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-owned-multi-"));
    try {
      const first = new OwnedSessionLedger(root);
      const second = new OwnedSessionLedger(root);
      await first.initialize();
      await second.initialize();
      expect(first.ownsLease).toBe(false);
      await first.acquireLease();
      expect(first.ownsLease).toBe(true);
      expect(second.ownsLease).toBe(false);
      await second.releaseLease();
      expect(await readFile(join(root, "bridge.lock"), "utf8")).not.toBe("");
      await first.releaseLease();
      expect(first.ownsLease).toBe(false);
      await expect(second.acquireLease()).resolves.toBeUndefined();
      expect(second.ownsLease).toBe(true);
      await second.releaseLease();
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});
