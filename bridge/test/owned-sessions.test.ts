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
      await first.acquireLease();
      const failure = await second.acquireLease().catch((error: unknown) => error);
      expect(String(failure)).toContain("OWNED_ROOT_BUSY");
      expect(String(failure)).toContain("busy or contains a stale bridge.lock");
      expect(String(failure)).toContain(JSON.stringify(join(root, "bridge.lock")));
      expect((failure as { code?: unknown }).code).toBe("OWNED_ROOT_BUSY");
      await first.releaseLease(); await expect(second.acquireLease()).resolves.toBeUndefined(); await second.releaseLease();
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("quotes a spaced root exactly so the host recovers the full path", async () => {
    const base = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    const root = join(base, "My Logs", "capture", "assistance");
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      const { writeFile } = await import("node:fs/promises");
      const lock = join(root, "bridge.lock");
      await writeFile(lock, `${JSON.stringify({ pid: 1, nonce: "spaced-stale" })}\n`, { flag: "wx" });
      const failure = await ledger.acquireLease().catch((error: unknown) => error);
      expect((failure as { code?: unknown }).code).toBe("OWNED_ROOT_BUSY");
      // JSON-quoted exact bytes: a space-splitting walk must not truncate this.
      expect(String(failure)).toContain(JSON.stringify(lock));
      expect(JSON.parse(String(failure).slice(String(failure).indexOf("at ") + 3).split(";")[0]!)).toBe(lock);
      expect(await readFile(lock, "utf8")).toContain("spaced-stale");
    } finally { await rm(base, { recursive: true, force: true }); }
  });

  it("refuses a possibly stale lock without removing it", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      // A stale but well-formed lock from a dead bridge: EEXIST alone cannot
      // prove it is stale, so acquisition refuses and leaves the file intact.
      const { writeFile } = await import("node:fs/promises");
      await writeFile(join(root, "bridge.lock"), `${JSON.stringify({ pid: 1, nonce: "stale-nonce-that-never-matches" })}\n`, { flag: "wx" });
      await expect(ledger.acquireLease()).rejects.toMatchObject({ code: "OWNED_ROOT_BUSY" });
      expect(await readFile(join(root, "bridge.lock"), "utf8")).toContain("stale-nonce-that-never-matches");
      // A non-owner release must not unlink a lock it does not own.
      await ledger.releaseLease();
      expect(await readFile(join(root, "bridge.lock"), "utf8")).toContain("stale-nonce-that-never-matches");
    } finally { await rm(root, { recursive: true, force: true }); }
  });

  it("never silently removes a malformed lock file", async () => {
    const root = await mkdtemp(join(tmpdir(), "lvu-ledger-"));
    try {
      const ledger = new OwnedSessionLedger(root); await ledger.initialize();
      const { writeFile } = await import("node:fs/promises");
      await writeFile(join(root, "bridge.lock"), "not-json\n", { flag: "wx" });
      await expect(ledger.acquireLease()).rejects.toThrow("OWNED_ROOT_BUSY");
      expect(await readFile(join(root, "bridge.lock"), "utf8")).toBe("not-json\n");
      await ledger.releaseLease();
      expect(await readFile(join(root, "bridge.lock"), "utf8")).toBe("not-json\n");
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
