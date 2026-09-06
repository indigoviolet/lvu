import { randomUUID } from "node:crypto";
import { mkdir, open, opendir, rename, rm } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { z } from "zod";

export type SessionPurpose = "ask" | "source_assistance" | "investigation";
export type SessionLifecycle = "ephemeral" | "resumable";

const id = z.string().regex(/^[A-Za-z0-9_-]{1,256}$/);
const timestamp = z.string().max(64);
const workspaceSchema = z.object({ version: z.literal(1), workspaceId: id, projectId: id.nullable(), directory: z.string().min(1).max(16_384) }).strict();
const leaseSchema = z.object({ pid: z.number().int().positive(), nonce: id }).strict();
const sessionSchema = z.object({
  version: z.literal(1), ownershipId: id, requestId: id, protocolRequestId: z.string().min(1).max(128),
  purpose: z.enum(["ask", "source_assistance", "investigation"]), lifecycle: z.enum(["ephemeral", "resumable"]),
  state: z.enum(["pending_create", "active", "pending_cleanup", "archived", "archive_failed", "create_failed"]),
  workspaceId: id.optional(), agentId: id.optional(), createdAt: timestamp, updatedAt: timestamp,
  lastUserMessageAt: timestamp.nullable().optional(), archivedAt: timestamp.nullable().optional(),
  activityBytes: z.number().int().min(0).max(4 * 1024 * 1024), activityTruncated: z.boolean(), lastError: z.string().max(4096).optional(),
  activityLost: z.boolean(),
}).strict();

export type OwnedWorkspaceRecord = z.infer<typeof workspaceSchema>;
export type OwnedSessionRecord = z.infer<typeof sessionSchema>;
const MAX_ACTIVITY_BYTES = 4 * 1024 * 1024;
const MAX_METADATA_BYTES = 64 * 1024;
const MAX_PENDING_RECORDS = 1024;
const MAX_QUEUED_ACTIVITY_EVENTS = 64;

export class OwnedSessionLedger {
  readonly root: string;
  readonly #queues = new Map<string, Promise<unknown>>();
  readonly #failures = new Map<string, unknown>();
  readonly #activityAdmission = new Map<string, { bytes: number; count: number; truncationQueued: boolean }>();
  #workspaceQueue: Promise<unknown> = Promise.resolve();
  #pendingQueue: Promise<unknown> = Promise.resolve();
  #lease: { handle: Awaited<ReturnType<typeof open>>; nonce: string } | null = null;

  constructor(root: string, readonly hooks: { beforeActivityWrite?: () => Promise<void> } = {}) { if (!isAbsolute(root)) throw new Error("LVU_PASEO_OWNED_ROOT must be an absolute path"); this.root = root; }

  async initialize(): Promise<void> {
    for (const directory of [this.root, this.#sessionsDir(), this.#activityDir(), this.#agentsDir(), this.#pendingDir()]) await mkdir(directory, { recursive: true, mode: 0o700 });
    await syncDirectory(this.root); await syncDirectory(dirname(this.root));
  }

  identifiers(): { ownershipId: string; requestId: string } { return { ownershipId: randomUUID(), requestId: randomUUID() }; }
  async readWorkspace(): Promise<OwnedWorkspaceRecord | null> { return this.#readValidated(join(this.root, "workspace.json"), workspaceSchema); }
  writeWorkspace(record: OwnedWorkspaceRecord): Promise<void> {
    const next = this.#workspaceQueue.then(() => this.#atomicJson(join(this.root, "workspace.json"), workspaceSchema.parse(record)));
    this.#workspaceQueue = next.catch(() => {}); return next;
  }

  createPending(input: Pick<OwnedSessionRecord, "ownershipId" | "requestId" | "protocolRequestId" | "purpose" | "lifecycle">): Promise<OwnedSessionRecord> {
    return this.#enqueue(input.ownershipId, async () => {
      const now = new Date().toISOString();
      const record = sessionSchema.parse({ version: 1, ...input, state: "pending_create", createdAt: now, updatedAt: now, activityBytes: 0, activityTruncated: false, activityLost: false });
      await this.#atomicJson(this.#recordPath(record.ownershipId), record);
      await this.#pendingMutation(() => this.#addPendingMarker(record.ownershipId));
      return record;
    });
  }

  async readByAgent(agentId: string): Promise<OwnedSessionRecord | null> {
    const index = await this.#readValidated(join(this.#agentsDir(), `${safeId(agentId)}.json`), z.object({ ownershipId: id }).strict());
    return index === null ? null : this.read(index.ownershipId);
  }
  read(ownershipId: string): Promise<OwnedSessionRecord | null> { return this.#afterQueued(ownershipId, () => this.#readValidated(this.#recordPath(ownershipId), sessionSchema)); }

  update(record: OwnedSessionRecord, patch: Partial<OwnedSessionRecord>): Promise<OwnedSessionRecord> {
    return this.#enqueue(record.ownershipId, async () => {
      const current = await this.#requireRecord(record.ownershipId);
      const next = sessionSchema.parse({ ...current, ...patch, version: 1, ownershipId: current.ownershipId, updatedAt: new Date().toISOString() });
      await this.#atomicJson(this.#recordPath(next.ownershipId), next);
      if (next.agentId !== undefined) await this.#atomicJson(join(this.#agentsDir(), `${safeId(next.agentId)}.json`), { ownershipId: next.ownershipId });
      if (next.state === "archived" || next.state === "create_failed" || (next.lifecycle === "resumable" && next.state === "active")) await this.#pendingMutation(() => this.#removePendingMarker(next.ownershipId));
      return next;
    });
  }

  recordActivity(record: OwnedSessionRecord, kind: string, payload: unknown): void {
    const admission = this.#activityAdmission.get(record.ownershipId) ?? { bytes: 0, count: 0, truncationQueued: false };
    this.#activityAdmission.set(record.ownershipId, admission);
    if (admission.truncationQueued) return;
    let line: string;
    try { line = `${JSON.stringify({ at: new Date().toISOString(), kind, payload })}\n`; }
    catch { line = `${JSON.stringify({ at: new Date().toISOString(), kind, payload_omitted: true, error: "activity was not JSON serializable" })}\n`; }
    let bytes = Buffer.byteLength(line);
    if (bytes > MAX_ACTIVITY_BYTES || admission.count >= MAX_QUEUED_ACTIVITY_EVENTS || admission.bytes + bytes > MAX_ACTIVITY_BYTES) {
      admission.truncationQueued = true;
      line = `${JSON.stringify({ at: new Date().toISOString(), kind: "activity_truncated", omitted: true, reason: "bounded activity queue exceeded" })}\n`;
      bytes = Buffer.byteLength(line);
    } else { admission.bytes += bytes; admission.count++; }
    const reservedBytes = bytes;
    const isTruncation = admission.truncationQueued;
    const queued = this.#enqueue(record.ownershipId, async () => {
      try {
        const current = await this.#requireRecord(record.ownershipId);
        if (current.activityTruncated || current.activityLost) return current;
        const handle = await open(join(this.#activityDir(), `${safeId(current.ownershipId)}.jsonl`), "a", 0o600);
        const observedBytes = (await handle.stat()).size;
        const admittedBytes = Math.max(current.activityBytes, observedBytes);
        if (admittedBytes > MAX_ACTIVITY_BYTES || admittedBytes + reservedBytes > MAX_ACTIVITY_BYTES) {
          await handle.close();
          const truncated = sessionSchema.parse({ ...current, activityBytes: Math.min(admittedBytes, MAX_ACTIVITY_BYTES), activityTruncated: true, updatedAt: new Date().toISOString() });
          await this.#atomicJson(this.#recordPath(current.ownershipId), truncated); return truncated;
        }
        try { await this.hooks.beforeActivityWrite?.(); await handle.writeFile(line); await handle.sync(); } finally { await handle.close(); }
        await syncDirectory(this.#activityDir());
        const next = sessionSchema.parse({ ...current, activityBytes: admittedBytes + reservedBytes, activityTruncated: isTruncation, updatedAt: new Date().toISOString() });
        await this.#atomicJson(this.#recordPath(current.ownershipId), next); return next;
      } catch (error) {
        const current = await this.#requireRecord(record.ownershipId).catch(() => null);
        if (current !== null) await this.#atomicJson(this.#recordPath(current.ownershipId), sessionSchema.parse({ ...current, activityLost: true, lastError: "activity persistence failed", updatedAt: new Date().toISOString() })).catch(() => {});
        throw error;
      }
    }).finally(() => {
      if (!isTruncation) { admission.bytes -= reservedBytes; admission.count--; }
    });
    void queued.catch(() => {});
  }

  async flush(record: OwnedSessionRecord): Promise<void> {
    await (this.#queues.get(record.ownershipId) ?? Promise.resolve()).catch(() => {});
    const failure = this.#failures.get(record.ownershipId);
    if (failure !== undefined) throw failure;
  }
  async releaseMemory(record: OwnedSessionRecord): Promise<void> {
    await this.flush(record);
    this.#queues.delete(record.ownershipId);
    this.#activityAdmission.delete(record.ownershipId);
    this.#failures.delete(record.ownershipId);
  }
  get residentActivityRecords(): number { return this.#activityAdmission.size; }
  activityPath(record: OwnedSessionRecord): string { return join(this.#activityDir(), `${safeId(record.ownershipId)}.jsonl`); }

  async recoveryBatch(limit: number): Promise<OwnedSessionRecord[]> {
    await this.#pendingQueue.catch(() => {});
    const names: string[] = []; const directory = await opendir(this.#pendingDir());
    try { for await (const entry of directory) { if (!entry.isFile() || !entry.name.endsWith(".pending")) continue; if (names.length >= MAX_PENDING_RECORDS) throw new Error("owned pending-session limit exceeded"); names.push(entry.name.slice(0, -8)); } }
    finally { await directory.close().catch(() => {}); }
    names.sort(); if (names.length === 0) return [];
    const cursorPath = join(this.root, "recovery-cursor.json");
    const cursor = await this.#readValidated(cursorPath, z.object({ ownershipId: id }).strict());
    const found = cursor === null ? -1 : names.findIndex((name) => name > cursor.ownershipId);
    const first = found < 0 ? 0 : found;
    const ordered = [...names.slice(first), ...names.slice(0, first)].slice(0, Math.max(1, Math.min(limit, names.length)));
    await this.#atomicJson(cursorPath, { ownershipId: ordered.at(-1)! });
    const records: OwnedSessionRecord[] = [];
    for (const ownershipId of ordered) { const record = await this.read(ownershipId); if (record !== null) records.push(record); }
    return records;
  }

  async acquireLease(): Promise<void> {
    if (this.#lease !== null) return;
    const path = join(this.root, "bridge.lock"); const nonce = randomUUID();
    let handle: Awaited<ReturnType<typeof open>>;
    try { handle = await open(path, "wx", 0o600); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === "EEXIST") throw new Error("owned assistance root is busy or contains a stale bridge.lock; automatic stale-lock removal is intentionally refused");
      throw error;
    }
    try { await handle.writeFile(`${JSON.stringify({ pid: process.pid, nonce })}\n`); await handle.sync(); await syncDirectory(this.root); }
    catch (error) { await handle.close().catch(() => {}); await rm(path, { force: true }).catch(() => {}); throw error; }
    this.#lease = { handle, nonce };
  }
  async releaseLease(): Promise<void> {
    if (this.#lease === null) return;
    const lease = this.#lease; this.#lease = null; await lease.handle.close();
    const path = join(this.root, "bridge.lock"); const current = await this.#readValidated(path, leaseSchema).catch(() => null);
    if (current?.nonce === lease.nonce) { await rm(path, { force: true }); await syncDirectory(this.root); }
  }

  async #addPendingMarker(ownershipId: string): Promise<void> {
    let count = 0; const directory = await opendir(this.#pendingDir());
    try { for await (const entry of directory) if (entry.isFile() && entry.name.endsWith(".pending") && ++count >= MAX_PENDING_RECORDS) throw new Error("owned pending-session limit exceeded"); }
    finally { await directory.close().catch(() => {}); }
    await this.#atomicJson(join(this.#pendingDir(), `${safeId(ownershipId)}.pending`), { ownershipId });
  }
  async #removePendingMarker(ownershipId: string): Promise<void> { await rm(join(this.#pendingDir(), `${safeId(ownershipId)}.pending`), { force: true }); await syncDirectory(this.#pendingDir()); }
  #pendingMutation<T>(operation: () => Promise<T>): Promise<T> { const next = this.#pendingQueue.catch(() => {}).then(operation); this.#pendingQueue = next.catch(() => {}); return next; }

  #enqueue<T>(ownershipId: string, operation: () => Promise<T>): Promise<T> {
    safeId(ownershipId); const previous = this.#queues.get(ownershipId) ?? Promise.resolve();
    const operationResult = previous.catch(() => {}).then(operation);
    const next = operationResult.catch((error) => { this.#failures.set(ownershipId, error); throw error; });
    this.#queues.set(ownershipId, next);
    void next.finally(() => { if (this.#queues.get(ownershipId) === next) this.#queues.delete(ownershipId); }).catch(() => {}); return next;
  }
  async #afterQueued<T>(ownershipId: string, operation: () => Promise<T>): Promise<T> { await (this.#queues.get(ownershipId) ?? Promise.resolve()).catch(() => {}); return operation(); }
  async #requireRecord(ownershipId: string): Promise<OwnedSessionRecord> { const record = await this.#readValidated(this.#recordPath(ownershipId), sessionSchema); if (record === null) throw new Error("owned session record is missing"); return record; }

  async #readValidated<T>(path: string, schema: z.ZodType<T>): Promise<T | null> {
    let handle; try { handle = await open(path, "r"); } catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return null; throw error; }
    try { const stat = await handle.stat(); if (stat.size > MAX_METADATA_BYTES) throw new Error(`owned metadata exceeds ${MAX_METADATA_BYTES} bytes`); return schema.parse(JSON.parse(await handle.readFile("utf8"))); }
    finally { await handle.close(); }
  }
  async #atomicJson(path: string, value: unknown): Promise<void> {
    const encoded = `${JSON.stringify(value)}\n`; if (Buffer.byteLength(encoded) > MAX_METADATA_BYTES) throw new Error(`owned metadata exceeds ${MAX_METADATA_BYTES} bytes`);
    const temporary = `${path}.${randomUUID()}.tmp`; const handle = await open(temporary, "wx", 0o600);
    try { await handle.writeFile(encoded); await handle.sync(); } finally { await handle.close(); }
    await rename(temporary, path); await syncDirectory(dirname(path));
  }
  #recordPath(ownershipId: string): string { return join(this.#sessionsDir(), `${safeId(ownershipId)}.json`); }
  #sessionsDir(): string { return join(this.root, "sessions"); }
  #activityDir(): string { return join(this.root, "activity"); }
  #agentsDir(): string { return join(this.root, "agents"); }
  #pendingDir(): string { return join(this.root, "pending"); }
}

async function syncDirectory(path: string): Promise<void> { const handle = await open(path, "r"); try { await handle.sync(); } finally { await handle.close(); } }
function safeId(value: string): string { return id.parse(value); }
