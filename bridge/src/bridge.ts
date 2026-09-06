import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import type { AgentHandle, PaseoBackend, RunResult } from "./backend.js";
import { parseJsonObject, parseProposal, proposalJsonSchema, SCHEMA_VERSION, type BridgeRequest } from "./protocol.js";

export interface BridgeLimits {
  maxSessions: number;
  defaultTimeoutMs: number;
  remoteCancelTimeoutMs: number;
  maxProposalBytes: number;
  maxEventBytes: number;
}
export type Emit = (message: Record<string, unknown>) => void;
type State = "new" | "starting" | "open" | "closing" | "closed";
type Observed = { type: "result"; result: RunResult } | { type: "timeout" } | { type: "cancelled" };
interface Session {
  agent: AgentHandle;
  unsubscribers: Array<() => void>;
  remoteBusy: boolean;
  observing: boolean;
  generation: number;
  cancelObserver: (() => void) | null;
  cancelPending: boolean;
  agentStatus: unknown;
}
interface PendingCreate {
  requestId: string;
  state: "creating" | "cleaning" | "cleanup_failed";
  reservationHeld: boolean;
}

export class Bridge {
  readonly #sessions = new Map<string, Session>();
  #state: State = "new";
  #sessionReservations = 0;
  #closePromise: Promise<void> | null = null;
  readonly #resumePending = new Map<string, Promise<Session>>();
  readonly #pendingCreates = new Set<PendingCreate>();
  readonly #cleanupTasks = new Set<Promise<void>>();
  constructor(readonly backend: PaseoBackend, readonly emit: Emit, readonly limits: BridgeLimits) {}

  async start(): Promise<void> {
    if (this.#state !== "new") throw coded("INVALID_STATE", "bridge can only be started once");
    this.#state = "starting";
    try {
      await deadline(this.backend.connect(), this.limits.defaultTimeoutMs, "CONNECT_TIMEOUT");
      if (this.#state !== "starting") {
        await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT").catch(() => {});
        throw coded("BRIDGE_CLOSED", "bridge closed while connecting");
      }
      this.#state = "open";
    } catch (error) {
      await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT").catch(() => {});
      this.#state = "closed";
      throw error;
    }
  }

  async close(): Promise<void> {
    if (this.#state === "closed") return;
    if (this.#closePromise !== null) return this.#closePromise;
    this.#state = "closing";
    this.#closePromise = (async () => {
      if (this.#pendingCreates.size > 0) {
        this.#event("bridge", "create_reconciliation_unresolved", {
          requests: [...this.#pendingCreates].map(({ requestId, state }) => ({ request_id: requestId, state })),
        });
      }
      for (const session of this.#sessions.values()) {
        session.observing = false;
        session.cancelObserver?.();
        this.#dispose(session);
      }
      this.#sessions.clear();
      try {
        await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT");
        await deadline(Promise.allSettled([...this.#cleanupTasks]).then(() => undefined), this.limits.defaultTimeoutMs, "CLEANUP_TIMEOUT").catch(() => {});
      } finally { this.#state = "closed"; }
    })();
    return this.#closePromise;
  }

  async handle(request: BridgeRequest): Promise<void> {
    if (this.#state !== "open") return this.#error(request.request_id, "BRIDGE_NOT_OPEN", `bridge is ${this.#state}`);
    try {
      switch (request.method) {
        case "capabilities": return await this.#capabilities(request.request_id);
        case "start_session": return await this.#startSession(request);
        case "send_prompt": return this.#sendPrompt(request);
        case "cancel": return await this.#cancel(request);
        case "resume_session": return await this.#resume(request);
        case "request_proposal": return await this.#proposal(request);
      }
    } catch (error) { this.#error(request.request_id, errorCode(error), errorMessage(error)); }
  }

  async #capabilities(requestId: string): Promise<void> {
    let providers: unknown;
    try { providers = await deadline(this.backend.listProviders(), this.limits.defaultTimeoutMs, "TIMEOUT"); }
    catch (error) { providers = { error: errorMessage(error) }; }
    this.#ok(requestId, {
      methods: ["capabilities", "start_session", "send_prompt", "cancel", "resume_session", "request_proposal"],
      sdk: { package: "@getpaseo/client", version: "0.7.2" }, streaming: true, resume: true, proposal_output_schema: true,
      remote_cancel: this.backend.supportsRemoteCancel,
      cancel_semantics: this.backend.supportsRemoteCancel ? "paseo_cli_stop" : "stop_bridge_observation_only",
      create_reconciliation: {
        pending: [...this.#pendingCreates].filter(({ state }) => state !== "cleanup_failed").length,
        cleanup_failed: [...this.#pendingCreates].filter(({ state }) => state === "cleanup_failed").length,
      },
      providers,
    });
  }

  #reserveSession(): void {
    if (this.#sessions.size + this.#sessionReservations >= this.limits.maxSessions) throw coded("LIMIT_EXCEEDED", "session limit reached");
    this.#sessionReservations++;
  }

  async #startSession(request: Extract<BridgeRequest, { method: "start_session" }>): Promise<void> {
    this.#reserveSession();
    const record: PendingCreate = { requestId: request.request_id, state: "creating", reservationHeld: true };
    this.#pendingCreates.add(record);
    let outcome: Promise<{ ok: true; agent: AgentHandle } | { ok: false; error: unknown }> | null = null;
    try {
      outcome = this.backend.createAgent({ provider: request.provider, cwd: request.cwd, ...(request.mode_id === undefined ? {} : { modeId: request.mode_id }), ...(request.thinking_option_id === undefined ? {} : { thinkingOptionId: request.thinking_option_id }), ...(request.title === undefined ? {} : { title: request.title }) })
        .then((agent) => ({ ok: true as const, agent }), (error) => ({ ok: false as const, error }));
      let settled;
      try { settled = await deadline(outcome, request.timeout_ms ?? this.limits.defaultTimeoutMs, "TIMEOUT"); }
      catch (error) {
        if (errorCode(error) === "TIMEOUT") void outcome.then((late) => this.#reconcileCreate(record, late));
        else this.#releaseCreate(record);
        throw error;
      }
      if (!settled.ok) { this.#releaseCreate(record); throw settled.error; }
      const agent = settled.agent;
      if (this.#state !== "open") {
        await this.#reconcileCreate(record, settled);
        throw coded("BRIDGE_CLOSED", "bridge closed while creating session");
      }
      const session = this.#attach(agent);
      this.#sessions.set(agent.id, session);
      this.#releaseCreate(record);
      this.#ok(request.request_id, { session_id: agent.id });
      if (request.prompt !== undefined) this.#launchTurn(agent.id, session, request.prompt, request.timeout_ms);
    } catch (error) {
      if (outcome === null) this.#releaseCreate(record);
      throw error;
    }
  }

  async #reconcileCreate(record: PendingCreate, outcome: { ok: true; agent: AgentHandle } | { ok: false; error: unknown }): Promise<void> {
    if (!record.reservationHeld) return;
    if (!outcome.ok) {
      this.#event("bridge", "create_reconciliation_released", { request_id: record.requestId, result: "create_rejected", error: errorMessage(outcome.error).slice(0, 4096) });
      this.#releaseCreate(record);
      return;
    }
    record.state = "cleaning";
    if (await this.#cleanupOwned(outcome.agent, record.requestId)) {
      this.#event("bridge", "create_reconciliation_released", { request_id: record.requestId, result: "late_agent_archived", agent_id: outcome.agent.id });
      this.#releaseCreate(record);
    } else record.state = "cleanup_failed";
  }

  #releaseCreate(record: PendingCreate): void {
    if (!record.reservationHeld) return;
    record.reservationHeld = false;
    this.#sessionReservations--;
    this.#pendingCreates.delete(record);
  }

  #sendPrompt(request: Extract<BridgeRequest, { method: "send_prompt" }>): void {
    const session = this.#get(request.session_id);
    if (session.remoteBusy || session.cancelPending) throw coded("SESSION_BUSY", "Paseo session still has a remote turn or cancellation in progress");
    this.#ok(request.request_id, { accepted: true });
    this.#launchTurn(request.session_id, session, request.prompt, request.timeout_ms);
  }

  #beginRemote(session: Session, prompt: string, timeoutMs: number, outputSchema?: Record<string, unknown>): { generation: number; run: Promise<RunResult> } {
    session.remoteBusy = true;
    session.agentStatus = "running";
    session.observing = true;
    const generation = ++session.generation;
    const run = Promise.resolve().then(() => session.agent.run(prompt, { timeoutMs, ...(outputSchema === undefined ? {} : { outputSchema }) }));
    void run.then((result) => this.#remoteSettled(session, generation, result), () => this.#remoteSettled(session, generation, null));
    return { generation, run };
  }

  #remoteSettled(session: Session, generation: number, result: RunResult | null): void {
    if (generation !== session.generation) return;
    if (result?.agentStatus !== undefined && result.agentStatus !== null) session.agentStatus = result.agentStatus;
    if (isTerminalStatus(session.agentStatus)) session.remoteBusy = false;
    session.observing = false;
    session.cancelObserver = null;
  }

  #launchTurn(id: string, session: Session, prompt: string, timeout?: number): void {
    const timeoutMs = timeout ?? this.limits.defaultTimeoutMs;
    const operation = this.#beginRemote(session, prompt, timeoutMs);
    this.#event(id, "turn_started", {});
    void this.#observe(session, operation.generation, operation.run, timeoutMs).then((observed) => {
      if (this.#state !== "open" || observed.type === "cancelled") return;
      if (observed.type === "timeout") return this.#event(id, "turn_failed", { status: "timeout", error: "bridge operation timed out", remote_agent_may_still_be_running: true });
      const result = observed.result;
      this.#event(id, result.status === "idle" ? "turn_completed" : result.status === "permission" ? "permission_required" : "turn_failed", { status: result.status, error: result.error, last_message: result.lastMessage, remote_agent_may_still_be_running: result.status === "timeout" || result.status === "permission" });
    });
  }

  async #observe(session: Session, generation: number, run: Promise<RunResult>, timeoutMs: number): Promise<Observed> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let cancel!: () => void;
    const cancelled = new Promise<Observed>((resolve) => { cancel = () => resolve({ type: "cancelled" }); });
    session.cancelObserver = cancel;
    const timedOut = new Promise<Observed>((resolve) => { timer = setTimeout(() => resolve({ type: "timeout" }), timeoutMs); });
    const result: Promise<Observed> = run.then((value): Observed => ({ type: "result", result: value })).catch((error): Observed => ({ type: "result", result: { status: "error", error: errorMessage(error), lastMessage: null } }));
    const observed = await Promise.race([result, timedOut, cancelled]);
    if (timer !== undefined) clearTimeout(timer);
    if (generation === session.generation && observed.type !== "result") session.observing = false;
    return observed;
  }

  async #cancel(request: Extract<BridgeRequest, { method: "cancel" }>): Promise<void> {
    const session = this.#get(request.session_id);
    if (session.cancelPending) throw coded("CANCEL_IN_PROGRESS", "cancellation is already in progress");
    const hadRemoteTurn = session.remoteBusy;
    const generation = session.generation;
    session.cancelPending = hadRemoteTurn;
    session.observing = false;
    session.cancelObserver?.();
    let remoteCancelled = false;
    let cancelError: string | null = null;
    if (hadRemoteTurn && this.backend.supportsRemoteCancel) {
      const cancellation = this.backend.cancelAgent(request.session_id, this.limits.remoteCancelTimeoutMs);
      const finalize = (cancelled: boolean) => {
        if (session.generation !== generation) return;
        session.cancelPending = false;
        if (cancelled) { session.remoteBusy = false; session.generation++; session.cancelObserver = null; }
        else if (isTerminalStatus(session.agentStatus)) session.remoteBusy = false;
      };
      try { remoteCancelled = await deadline(cancellation, this.limits.remoteCancelTimeoutMs, "CANCEL_TIMEOUT"); finalize(remoteCancelled); }
      catch (error) {
        cancelError = errorMessage(error);
        void cancellation.then(finalize, () => finalize(false));
      }
    }
    if ((!hadRemoteTurn || !this.backend.supportsRemoteCancel) && session.generation === generation) {
      session.cancelPending = false;
    }
    this.#event(request.session_id, "observation_cancelled", { remote_cancelled: remoteCancelled, remote_agent_may_still_be_running: hadRemoteTurn && !remoteCancelled });
    this.#ok(request.request_id, { cancelled: hadRemoteTurn, remote_cancelled: remoteCancelled, remote_agent_may_still_be_running: hadRemoteTurn && !remoteCancelled, ...(cancelError === null ? {} : { cancel_error: cancelError.slice(0, 4096) }) });
  }

  async #resume(request: Extract<BridgeRequest, { method: "resume_session" }>): Promise<void> {
    if (this.#sessions.has(request.session_id)) return this.#ok(request.request_id, { session_id: request.session_id, resumed: true });
    let pending = this.#resumePending.get(request.session_id);
    if (pending === undefined) {
      this.#reserveSession();
      pending = this.#resumeOne(request.session_id);
      this.#resumePending.set(request.session_id, pending);
      void pending.finally(() => this.#resumePending.delete(request.session_id)).catch(() => {});
    }
    const session = await pending;
    this.#ok(request.request_id, { session_id: session.agent.id, resumed: true });
  }

  async #resumeOne(id: string): Promise<Session> {
    try {
      const agent = this.backend.refAgent(id);
      const refreshed = await deadline(agent.refresh(), this.limits.defaultTimeoutMs, "TIMEOUT");
      if (!refreshed.exists) throw coded("NOT_FOUND", "Paseo session not found");
      if (this.#state !== "open") throw coded("BRIDGE_CLOSED", "bridge closed while resuming session");
      const existing = this.#sessions.get(agent.id);
      if (existing !== undefined) return existing;
      const session = this.#attach(agent);
      session.remoteBusy = refreshed.running;
      session.agentStatus = refreshed.running ? "running" : "idle";
      session.observing = refreshed.running;
      this.#sessions.set(agent.id, session);
      return session;
    } finally { this.#sessionReservations--; }
  }

  async #proposal(request: Extract<BridgeRequest, { method: "request_proposal" }>): Promise<void> {
    const session = this.#get(request.session_id);
    if (session.remoteBusy || session.cancelPending) throw coded("SESSION_BUSY", "Paseo session still has a remote turn or cancellation in progress");
    const timeoutMs = request.timeout_ms ?? this.limits.defaultTimeoutMs;
    const operation = this.#beginRemote(session, proposalPrompt(request), timeoutMs, proposalJsonSchema(request.kind));
    this.#event(request.session_id, "proposal_started", { request_id: request.request_id, proposal_kind: request.kind });
    const observed = await this.#observe(session, operation.generation, operation.run, timeoutMs);
    if (observed.type === "cancelled") throw coded("CANCELLED", "proposal observation was cancelled");
    if (observed.type === "timeout") throw coded("TIMEOUT", "proposal operation timed out; remote agent may still be running");
    const result = observed.result;
    if (result.status !== "idle" || result.lastMessage === null) throw coded(result.status === "timeout" ? "TIMEOUT" : result.status === "permission" ? "PERMISSION_REQUIRED" : "AGENT_ERROR", result.error ?? `agent finished with ${result.status}`);
    let proposal;
    try { proposal = parseProposal(parseJsonObject(result.lastMessage, this.limits.maxProposalBytes), request.kind, request.originating_revision); }
    catch (error) { throw coded("INVALID_PROPOSAL", errorMessage(error)); }
    this.#ok(request.request_id, { proposal });
    this.#event(request.session_id, "proposal_completed", { request_id: request.request_id, proposal_kind: request.kind });
  }

  #attach(agent: AgentHandle): Session {
    const session: Session = { agent, remoteBusy: false, observing: false, generation: 0, cancelObserver: null, cancelPending: false, agentStatus: null, unsubscribers: [] };
    session.unsubscribers.push(agent.subscribeStream((payload) => { if (this.#state === "open" && session.observing) this.#event(agent.id, "stream", { payload }); }));
    session.unsubscribers.push(agent.subscribeUpdate((payload) => {
      const status = statusFromUpdate(payload);
      if (status !== null) session.agentStatus = status;
      if (status === "running" || status === "initializing") session.remoteBusy = true;
      else if (isTerminalStatus(status) && !session.cancelPending) session.remoteBusy = false;
      if (this.#state === "open" && session.observing) this.#event(agent.id, "session_update", { payload });
      if (isTerminalStatus(status)) session.observing = false;
    }));
    return session;
  }
  #cleanupOwned(agent: AgentHandle, requestId: string): Promise<boolean> {
    let task!: Promise<void>;
    let succeeded = false;
    task = deadline(this.backend.cleanupOwnedAgent(agent, this.limits.defaultTimeoutMs), this.limits.defaultTimeoutMs, "CLEANUP_TIMEOUT")
      .then(() => { succeeded = true; })
      .catch((error) => this.#event("bridge", "owned_cleanup_failed", { request_id: requestId, agent_id: agent.id, error: errorMessage(error).slice(0, 4096) }))
      .finally(() => this.#cleanupTasks.delete(task));
    this.#cleanupTasks.add(task);
    return task.then(() => succeeded);
  }
  #dispose(session: Session): void { for (const unsubscribe of session.unsubscribers.splice(0)) unsubscribe(); session.generation++; session.cancelObserver = null; }
  #get(id: string): Session { const session = this.#sessions.get(id); if (!session) throw coded("NOT_FOUND", "unknown session_id"); return session; }
  #ok(requestId: string, result: Record<string, unknown>): void { this.emit({ schema_version: SCHEMA_VERSION, request_id: requestId, ok: true, result }); }
  #error(requestId: string, code: string, message: string): void { this.emit({ schema_version: SCHEMA_VERSION, request_id: requestId, ok: false, error: { code, message: message.slice(0, 4096) } }); }
  #event(sessionId: string, kind: string, detail: Record<string, unknown>): void {
    const message = { schema_version: SCHEMA_VERSION, session_id: sessionId, kind, ...detail };
    if (Buffer.byteLength(JSON.stringify(message), "utf8") <= this.limits.maxEventBytes) this.emit(message);
    else this.emit({ schema_version: SCHEMA_VERSION, session_id: sessionId, kind, payload_omitted: true, error: { code: "EVENT_TOO_LARGE", message: "Paseo event exceeded the bridge byte limit" } });
  }
}

function proposalPrompt(request: Extract<BridgeRequest, { method: "request_proposal" }>): string {
  const python = fileURLToPath(new URL("../../python/.venv/bin/python", import.meta.url));
  const inspection = existsSync(python)
    ? `A local Python interpreter with Polars is available at ${JSON.stringify(python)}. Use it to read the Parquet schema and bounded samples; do not infer contents from binary strings.`
    : "Inspect local datasets with an available Parquet reader; do not infer their contents from binary strings.";
  return [
    `Propose an lvu ${request.kind} definition.`,
    `Instruction: ${request.instruction}`,
    `Inspection manifest: ${request.context.manifest_path}`,
    `Local datasets: ${request.context.dataset_paths.join(", ")}`,
    inspection,
    `Originating data revision: ${request.originating_revision.data}`,
    `Originating definition revision: ${request.originating_revision.definition}`,
    "Inspect local paths as needed. Do not copy bulk dataset contents into the response. Keep both originating revision values unchanged.",
    "Return exactly one JSON object matching the schema below. No Markdown fences, separators, preface or trailing prose. Put all explanation inside the explanation property. Include the kind, definition, explanation and originating_revision envelope; do not return just the expression.",
    "Each enrichment expressions value must be a single Python expression returning pl.Expr. No assignments, semicolon-separated statements, imports, helper variables, lambdas or callbacks. Choose the simplest reliable source from the actual typed columns and sample values; do not assume any particular input field name. Inspect schemas and null/type provenance across Parquet parts first. Do not regex-parse JSON raw to recover a field already present as a column. Use pl.col('raw').str.extract only when the needed value is absent from structured columns; explain that fallback. Do not reference invented columns or add fallback references to _lvu_raw.",
    "For newly created identifiers, generate valid RFC 4122 UUIDs (for example Python uuid.uuid4()). Do not use zero-filled placeholder identifiers.",
    "For view adaptation, optional enrichments is the complete ordered chain of {id, source}. Preserve IDs for unchanged stages. Each source is either name = a single pl.Expr or /regex/flags with named captures. Omit enrichments to retain the reviewed recipe chain; an empty array explicitly clears it. Leave recipe_stage_revisions empty; unresolved references cannot be applied.",
    `JSON schema: ${JSON.stringify(proposalJsonSchema(request.kind))}`,
  ].join("\n");
}

function deadline<T>(promise: Promise<T>, timeoutMs: number, code: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(coded(code, `operation exceeded ${timeoutMs}ms`)), timeoutMs);
    promise.then((value) => { clearTimeout(timer); resolve(value); }, (error) => { clearTimeout(timer); reject(error); });
  });
}
function coded(code: string, message: string): Error { return Object.assign(new Error(message), { code }); }
function errorCode(error: unknown): string { return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : "INTERNAL_ERROR"; }
function errorMessage(error: unknown): string { return error instanceof Error ? error.message : String(error); }
function isTerminalStatus(status: unknown): status is "idle" | "error" | "closed" { return status === "idle" || status === "error" || status === "closed"; }
function statusFromUpdate(update: unknown): unknown {
  if (typeof update !== "object" || update === null || !("kind" in update) || update.kind !== "upsert" || !("agent" in update) || typeof update.agent !== "object" || update.agent === null || !("status" in update.agent)) return null;
  return update.agent.status;
}
