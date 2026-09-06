import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { OwnedSessionLedger } from "../src/owned_sessions.js";
import { deferred } from "./fake-backend.js";

describe("owned session ledger", () => {
  it("requires an absolute owned root", () => {
    expect(() => new OwnedSessionLedger("relative/path")).toThrow("absolute path");
  });

  it("records pending ownership before association and marks oversized activity truncated", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      const ids = ledger.identifiers();
      const record = await ledger.createPending({ ...ids, protocolRequestId: "request", purpose: "ask", lifecycle: "ephemeral" });
      const pending = await ledger.read(record.ownershipId);
      expect(pending).toMatchObject({ state: "pending_create" });
      expect(pending).not.toHaveProperty("agentId");
      ledger.recordActivity(record, "oversized", "x".repeat(4 * 1024 * 1024));
      await ledger.flush(record);
      expect(await ledger.read(record.ownershipId)).toMatchObject({ activityTruncated: true, activityLost: false });
      expect((await ledger.read(record.ownershipId))!.activityBytes).toBeLessThan(1024);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("serializes delayed activity and terminal mutations against canonical state", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-")); const gate = deferred<void>();
    try {
      const ledger = new OwnedSessionLedger(root, { beforeActivityWrite: () => gate.promise }); await ledger.initialize();
      const ids = ledger.identifiers(); const stale = await ledger.createPending({ ...ids, protocolRequestId: "request", purpose: "ask", lifecycle: "ephemeral" });
      const active = await ledger.update(stale, { state: "active", workspaceId: "workspace", agentId: "agent" });
      ledger.recordActivity(stale, "stream", { delta: "persist me first" });
      let terminalSettled = false;
      const terminal = ledger.update(active, { state: "pending_cleanup" }).then((record) => ledger.update(record, { state: "archived", archivedAt: "2026-09-06T00:00:00Z" })).then(() => { terminalSettled = true; });
      await Promise.resolve(); expect(terminalSettled).toBe(false);
      gate.resolve(); await terminal; await ledger.flush(stale);
      expect(await ledger.read(stale.ownershipId)).toMatchObject({ state: "archived", archivedAt: "2026-09-06T00:00:00Z", activityTruncated: false });
      expect((await ledger.read(stale.ownershipId))!.activityBytes).toBeGreaterThan(0);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("rotates bounded recovery batches instead of starving older pending records", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      for (let index = 0; index < 3; index++) { const ids = ledger.identifiers(); await ledger.createPending({ ...ids, protocolRequestId: `request-${index}`, purpose: "ask", lifecycle: "ephemeral" }); }
      const seen = new Set<string>();
      for (let index = 0; index < 3; index++) for (const record of await ledger.recoveryBatch(1)) seen.add(record.ownershipId);
      expect(seen.size).toBe(3);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("bounds queued activity while a write is blocked and persists one truncation", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-")); const gate = deferred<void>();
    try {
      const ledger = new OwnedSessionLedger(root, { beforeActivityWrite: () => gate.promise }); await ledger.initialize();
      const ids = ledger.identifiers(); const record = await ledger.createPending({ ...ids, protocolRequestId: "flood", purpose: "ask", lifecycle: "ephemeral" });
      for (let index = 0; index < 1_000; index++) ledger.recordActivity(record, "stream", { index, delta: "x".repeat(1024) });
      gate.resolve(); await ledger.flush(record);
      const persisted = await ledger.read(record.ownershipId); expect(persisted).toMatchObject({ activityTruncated: true, activityLost: false });
      const lines = (await readFile(ledger.activityPath(record), "utf8")).trim().split("\n");
      expect(lines.length).toBeLessThanOrEqual(65); expect(lines.filter((line) => line.includes("activity_truncated"))).toHaveLength(1);
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("holds one exclusive owned-root lease", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const first = new OwnedSessionLedger(root); const second = new OwnedSessionLedger(root); await first.initialize(); await second.initialize();
      await first.acquireLease(); await expect(second.acquireLease()).rejects.toThrow("busy or contains a stale bridge.lock");
      await first.releaseLease(); await expect(second.acquireLease()).resolves.toBeUndefined(); await second.releaseLease();
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("retains activity failure across flush attempts and marks it durably", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root, { beforeActivityWrite: async () => { throw new Error("disk refused activity"); } }); await ledger.initialize();
      const ids = ledger.identifiers(); const record = await ledger.createPending({ ...ids, protocolRequestId: "lost", purpose: "ask", lifecycle: "ephemeral" });
      ledger.recordActivity(record, "stream", { delta: "must persist" });
      await expect(ledger.flush(record)).rejects.toThrow("disk refused activity");
      await expect(ledger.flush(record)).rejects.toThrow("disk refused activity");
      expect(await ledger.read(record.ownershipId)).toMatchObject({ activityLost: true });
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("releases in-memory admission state for many durably settled sessions", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      for (let index = 0; index < 20; index++) {
        const ids = ledger.identifiers(); const record = await ledger.createPending({ ...ids, protocolRequestId: `settled-${index}`, purpose: "ask", lifecycle: "ephemeral" });
        ledger.recordActivity(record, "run_settled", { index }); await ledger.flush(record);
        await ledger.update(record, { state: "archived", archivedAt: "2026-09-06T00:00:00Z" }); await ledger.releaseMemory(record);
      }
      expect(ledger.residentActivityRecords).toBe(0);
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});
