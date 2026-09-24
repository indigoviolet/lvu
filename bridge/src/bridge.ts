import { proposalPrompt } from "./proposal_prompt.js";
import type { AgentHandle, PaseoBackend, RunResult } from "./backend.js";
import { OwnedSessionLedger, type OwnedSessionRecord, type SessionLifecycle, type SessionPurpose } from "./owned_sessions.js";
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
  owned: OwnedSessionRecord | null;
  cleanupScheduled: boolean;
  terminalCleanupOnUpdate: boolean;
  turnSettlementPending: boolean;
}
interface PendingCreate {
  requestId: string;
  state: "creating" | "cleaning" | "cleanup_failed";
  reservationHeld: boolean;
  owned: OwnedSessionRecord | null;
}

export class Bridge {
  readonly #sessions = new Map<string, Session>();
  #state: State = "new";
  #sessionReservations = 0;
  #closePromise: Promise<void> | null = null;
  readonly #resumePending = new Map<string, Promise<Session>>();
  readonly #pendingCreates = new Set<PendingCreate>();
  readonly #cleanupTasks = new Set<Promise<void>>();
  #workspacePromise: Promise<{ id: string; projectId: string | null; directory: string }> | null = null;
  #workspacePending = false;
  // Lease pins span the setup window between acquiring the exclusive lock and
  // inserting the new work into a tracked set, so a concurrent idle-release
  // cannot drop the lease mid-setup. All lease transitions run under
  // #leaseMutex. Startup order establishes no lasting priority: once recovery
  // settles with nothing live attached, an idle bridge releases even if it
  // never did managed work, so either of two idle windows can acquire next.
  // #leaseAcquisition tracks a raw in-flight O_EXCL so
  // a late win is adopted or released instead of orphaned. The recovery
  // report set keeps repeated recoveries (one per acquisition) from
  // re-emitting the same unresolved create.
  #leasePins = 0;
  #leaseMutex: Promise<void> = Promise.resolve();
  #leaseAcquisition: Promise<void> | null = null;
  #recoveryNeeded = false;
  #recoveryTask: Promise<void> | null = null;
  #recoveryReportedUnresolved = new Set<string>();
  // A bridge owns a distinct ledger instance. Windows share its on-disk root,
  // never the mutable in-memory ledger/queues/hook (as enforced by CLI wiring).
  constructor(readonly backend: PaseoBackend, readonly emit: Emit, readonly limits: BridgeLimits, readonly ledger: OwnedSessionLedger | null = null) {}

  async start(): Promise<void> {
    if (this.#state !== "new") throw coded("INVALID_STATE", "bridge can only be started once");
    this.#state = "starting";
    try {
      // The daemon lives beside lvu (same machine/container selected by
      // LVU_PASEO_URL, default ws://127.0.0.1:6767/ws). A connect failure is
      // therefore a daemon-reachability failure, not a bridge bug. Give it a
      // stable DAEMON_* marker so the host classifies by code, not by the
      // generic "bridge connection failed" wrapper the CLI adds later.
      try {
        await deadline(this.backend.connect(), this.limits.defaultTimeoutMs, "CONNECT_TIMEOUT");
      } catch (error) {
        if (errorCode(error) === "CONNECT_TIMEOUT") throw coded("DAEMON_TIMEOUT", `DAEMON_TIMEOUT: could not reach the Paseo daemon (${errorMessage(error)})`);
        if (errorCode(error) === "DAEMON_UNREACHABLE" || errorCode(error) === "DAEMON_TIMEOUT") throw error;
        throw coded("DAEMON_UNREACHABLE", `DAEMON_UNREACHABLE: could not reach the Paseo daemon (${errorMessage(error)})`);
      }
      if (this.ledger !== null) {
        await deadline(this.ledger.initialize(), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        this.ledger.hooks.onQuiescent = () => { void this.#notifyOwnedIdle().catch(() => {}); };
        try {
          await this.#acquireOwnedLease(this.limits.defaultTimeoutMs);
        } catch (error) {
          if (errorCode(error) !== "OWNED_ROOT_BUSY") throw error;
          // A second lvu window shares the same owned root while the first
          // still holds the exclusive bridge.lock. The lock is the cross-process
          // mutator guard (per-record queues, activity accounting, workspace
          // placement and recovery are all per-process), and EEXIST/PID checks
          // cannot prove it stale, so it is never removed here. Stay open as a
          // lease-less standby instead of exiting: unmanaged work still runs,
          // managed requests fail per-request with the same stable
          // OWNED_ROOT_BUSY marker and exact lock path, and the next managed
          // request after the owner goes idle or exits retries acquisition.
          // Recovery stays deferred until the lease is held so standby never
          // mutates the owner's durable state.
          if (this.#state !== "starting") {
            await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT").catch(() => {});
            throw coded("BRIDGE_CLOSED", "bridge closed while connecting");
          }
          this.#state = "open";
          return;
        }
      }
      if (this.#state !== "starting") {
        await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT").catch(() => {});
        throw coded("BRIDGE_CLOSED", "bridge closed while connecting");
      }
      this.#state = "open";
      this.#launchRecovery();
    } catch (error) {
      await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT").catch(() => {});
      if (this.ledger !== null) this.ledger.hooks.onQuiescent = null;
      await this.ledger?.releaseLease().catch(() => {});
      this.#state = "closed";
      throw error;
    }
  }

  async close(): Promise<void> {
    if (this.#state === "closed") return;
    if (this.#closePromise !== null) return this.#closePromise;
    this.#state = "closing";
    this.#closePromise = (async () => {
      // Bounded wait for pinned owned-setup windows entered before close: their
      // state checks then bail before further ledger writes. Production CLI
      // shutdown already settles admitted requests first (JsonlServer.stop),
      // so this only binds direct-API races; it never waits forever.
      const pinDeadline = Date.now() + this.limits.remoteCancelTimeoutMs;
      while (this.#leasePins > 0 && Date.now() < pinDeadline) await new Promise((resolve) => setTimeout(resolve, 5));
      if (this.#pendingCreates.size > 0) {
        this.#event("bridge", "create_reconciliation_unresolved", {
          requests: [...this.#pendingCreates].map(({ requestId, state }) => ({ request_id: requestId, state })),
        });
      }
      await deadline(Promise.allSettled([...this.#cleanupTasks]).then(() => undefined), this.limits.remoteCancelTimeoutMs, "RECOVERY_DRAIN_TIMEOUT").catch(() => {});
      for (const session of [...this.#sessions.values()]) {
        let archived = false;
        session.observing = false;
        session.cancelObserver?.();
        if (session.owned?.lifecycle === "ephemeral" && session.owned.state !== "archived" && session.owned.state !== "pending_cleanup") {
          const snapshot = session.remoteBusy || session.cancelPending ? null : await deadline(session.agent.refresh(), this.limits.defaultTimeoutMs, "CLOSE_REFRESH_TIMEOUT").catch(() => null);
          if (snapshot !== null && snapshot.exists && !snapshot.running && isTerminalStatus(snapshot.status)) {
            session.owned.state = "pending_cleanup";
            this.ledger!.recordActivity(session.owned, "cleanup_requested", { reason: "bridge_close", status: snapshot.status });
            session.owned = await deadline(this.ledger!.update(session.owned, { state: "pending_cleanup" }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
            archived = await this.#cleanupOwned(session.agent, session.owned.protocolRequestId, session.owned);
          } else {
            session.owned = await deadline(this.ledger!.update(session.owned, { state: "pending_cleanup", lastError: "bridge closed before remote terminal state was confirmed" }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
          }
        }
        this.#dispose(session);
        if (archived && session.owned !== null) await this.ledger!.releaseMemory(session.owned);
      }
      this.#sessions.clear();
      try {
        await deadline(Promise.allSettled([...this.#cleanupTasks]).then(() => undefined), this.limits.defaultTimeoutMs, "CLEANUP_TIMEOUT").catch(() => {});
        await deadline(this.backend.close(), this.limits.defaultTimeoutMs, "CLOSE_TIMEOUT");
      } finally {
        this.#state = "closed";
        // Deadline expiry does not cancel filesystem work. Keep ownership
        // until queued writes actually settle; their quiescence callback
        // releases our nonce, even after bounded close has returned.
        await this.#notifyOwnedIdle().catch(() => {});
      }
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
    finally { void this.#notifyOwnedIdle().catch(() => {}); }
  }

  async #capabilities(requestId: string): Promise<void> {
    let providers: unknown;
    // The daemon is a separate process, so it can be gone while this one is
    // healthy. Report which of the two failed instead of a bare error string.
    try { providers = await deadline(this.backend.listProviders(), this.limits.defaultTimeoutMs, "TIMEOUT"); }
    catch (error) { providers = { error: errorMessage(error), code: daemonErrorCode(error) }; }
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
    const purpose = request.purpose as SessionPurpose | undefined;
    const lifecycle: SessionLifecycle = purpose === "ask" || purpose === "source_assistance" ? "ephemeral" : "resumable";
    let owned: OwnedSessionRecord | null = null;
    let workspaceId: string | undefined;
    let sdkRequestId: string | undefined;
    // Pins span acquisition through tracking insertion so a concurrent
    // idle-release cannot drop the lease mid-setup; the tracked PendingCreate
    // guards everything after. Close during setup bails at the state checks
    // below before further ledger writes, leaving a pending marker for a
    // later lease-holder's recovery instead of writing lease-less.
    let enteredLease = false;
    if (purpose !== undefined) {
      if (this.ledger === null) { this.#sessionReservations--; throw coded("OWNED_ROOT_UNAVAILABLE", "managed assistance requires LVU_PASEO_OWNED_ROOT"); }
      // A standby or idle-released bridge holds no exclusive lock: serialize
      // managed creation on the lease so only one bridge mutates the owned
      // ledger. Busy propagates with the stable marker/path and no state is
      // removed; the next request retries after the owner goes idle or exits.
      try {
        await this.#enterOwned(request.timeout_ms ?? this.limits.defaultTimeoutMs);
        enteredLease = true;
        this.#assertOpenForOwned();
        const placement = await deadline(this.#ensureWorkspace(), request.timeout_ms ?? this.limits.defaultTimeoutMs, "TIMEOUT");
        this.#assertOpenForOwned();
        const ids = this.ledger.identifiers();
        sdkRequestId = ids.requestId;
        workspaceId = placement.id;
        owned = await deadline(this.ledger.createPending({ ...ids, protocolRequestId: request.request_id, purpose, lifecycle }), request.timeout_ms ?? this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        owned = await deadline(this.ledger.update(owned, { workspaceId }), request.timeout_ms ?? this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        this.#assertOpenForOwned();
      } catch (error) { this.#sessionReservations--; if (enteredLease) await this.#exitOwned(); throw error; }
    }
    const record: PendingCreate = { requestId: request.request_id, state: "creating", reservationHeld: true, owned };
    this.#pendingCreates.add(record);
    if (enteredLease) await this.#exitOwned();
    let outcome: Promise<{ ok: true; agent: AgentHandle } | { ok: false; error: unknown }> | null = null;
    let createdAgent: AgentHandle | null = null;
    try {
      outcome = this.backend.createAgent({ provider: request.provider, cwd: purpose === undefined ? request.cwd : this.ledger!.root, ...(request.mode_id === undefined ? {} : { modeId: request.mode_id }), ...(request.thinking_option_id === undefined ? {} : { thinkingOptionId: request.thinking_option_id }), ...(request.title === undefined ? {} : { title: request.title }), ...(workspaceId === undefined ? {} : { workspaceId }), ...(sdkRequestId === undefined ? {} : { requestId: sdkRequestId }), ...(owned === null ? {} : { labels: { "lvu.owner": owned.ownershipId, "lvu.request": owned.requestId, "lvu.lifecycle": owned.lifecycle, "lvu.purpose": owned.purpose } }) })
        .then((agent) => ({ ok: true as const, agent }), (error) => ({ ok: false as const, error }));
      let settled;
      try { settled = await deadline(outcome, request.timeout_ms ?? this.limits.defaultTimeoutMs, "TIMEOUT"); }
      catch (error) {
        if (errorCode(error) === "TIMEOUT") void outcome.then((late) => this.#reconcileCreate(record, late));
        else this.#releaseCreate(record);
        throw error;
      }
      if (!settled.ok) {
        if (record.owned !== null) record.owned = await deadline(this.ledger!.update(record.owned, { state: "create_failed", lastError: errorMessage(settled.error).slice(0, 4096) }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        this.#releaseCreate(record);
        // An unreachable daemon and an unauthenticated provider are different
        // problems with different remedies, and neither of them is "the bridge
        // is not running". Ask the daemon which one it was.
        throw await this.#explainCreateFailure(request.provider, settled.error);
      }
      const agent = settled.agent;
      createdAgent = agent;
      if (record.owned !== null) record.owned = await deadline(this.ledger!.update(record.owned, { state: "active", agentId: agent.id }), request.timeout_ms ?? this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
      if (this.#state !== "open") {
        await this.#reconcileCreate(record, settled);
        throw coded("BRIDGE_CLOSED", "bridge closed while creating session");
      }
      const session = this.#attach(agent, record.owned);
      this.#sessions.set(agent.id, session);
      this.#releaseCreate(record);
      this.#ok(request.request_id, { session_id: agent.id });
      if (request.prompt !== undefined) this.#launchTurn(agent.id, session, request.prompt, request.timeout_ms);
    } catch (error) {
      if (outcome === null) {
        if (record.owned !== null) record.owned = await deadline(this.ledger!.update(record.owned, { state: "create_failed", lastError: errorMessage(error).slice(0, 4096) }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        this.#releaseCreate(record);
      } else if (createdAgent !== null && record.reservationHeld && record.state === "creating") {
        record.state = "cleanup_failed";
        this.#event("bridge", "owned_cleanup_failed", { request_id: record.requestId, agent_id: createdAgent.id, error: "created agent ownership could not be persisted; cleanup deferred" });
      }
      throw error;
    }
  }

  // Turn an opaque session-create rejection into the specific reason: the
  // daemon is unreachable, the provider is unknown, the provider is not
  // authenticated, or the create genuinely failed for its own reason. Only
  // runs on the failure path, so a healthy create costs nothing.
  async #explainCreateFailure(provider: string, error: unknown): Promise<unknown> {
    let entries;
    try { entries = await deadline(this.backend.listProviders(), this.limits.defaultTimeoutMs, "TIMEOUT"); }
    catch (probe) {
      return coded(daemonErrorCode(probe), `the agent bridge is running but cannot reach the Paseo daemon (${errorMessage(probe)})`);
    }
    // lvu configures `provider/model`; Paseo enumerates the provider alone.
    const name = provider.split("/")[0] ?? provider;
    const entry = entries.find((candidate) => candidate.provider === name);
    const ready = entries.filter((candidate) => candidate.enabled && candidate.status === "ready").map((candidate) => candidate.provider);
    if (entry === undefined) {
      return coded("PROVIDER_UNKNOWN", ready.length === 0
        ? `no agent provider is authenticated in Paseo (${name} is not configured)`
        : `agent provider ${name} is not configured; authenticated providers: ${ready.join(", ")}`);
    }
    if (!entry.enabled || entry.status !== "ready") {
      return coded("PROVIDER_UNAVAILABLE", ready.length === 0
        ? `agent provider ${name} is ${entry.status}${entry.enabled ? "" : " and disabled"}, and no other provider is authenticated`
        : `agent provider ${name} is ${entry.status}${entry.enabled ? "" : " and disabled"}; authenticated providers: ${ready.join(", ")}`);
    }
    return error;
  }

  async #reconcileCreate(record: PendingCreate, outcome: { ok: true; agent: AgentHandle } | { ok: false; error: unknown }): Promise<void> {
    if (!record.reservationHeld) return;
    try {
      if (!outcome.ok) {
        if (record.owned !== null) record.owned = await deadline(this.ledger!.update(record.owned, { state: "create_failed", lastError: errorMessage(outcome.error).slice(0, 4096) }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        this.#event("bridge", "create_reconciliation_released", { request_id: record.requestId, result: "create_rejected", error: errorMessage(outcome.error).slice(0, 4096) });
        this.#releaseCreate(record);
        return;
      }
      record.state = "cleaning";
      if (record.owned !== null) {
        record.owned = await deadline(this.ledger!.update(record.owned, { state: "pending_cleanup", agentId: outcome.agent.id }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
        const snapshot = await deadline(outcome.agent.refresh(), this.limits.remoteCancelTimeoutMs, "RECONCILE_TIMEOUT").catch(() => null);
        if (snapshot === null || snapshot.running || !isTerminalStatus(snapshot.status)) {
          record.state = "cleanup_failed";
          record.owned = await deadline(this.ledger!.update(record.owned, { state: "pending_cleanup", lastError: "late create is not confirmed terminal" }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
          this.#event("bridge", "owned_cleanup_deferred", { request_id: record.requestId, agent_id: outcome.agent.id, remote_agent_may_still_be_running: true, activity_path: this.ledger!.activityPath(record.owned) });
          return;
        }
      }
      if (await this.#cleanupOwned(outcome.agent, record.requestId, record.owned)) {
        if (record.owned !== null) await this.ledger!.releaseMemory(record.owned);
        this.#event("bridge", "create_reconciliation_released", { request_id: record.requestId, result: "late_agent_archived", agent_id: outcome.agent.id });
        this.#releaseCreate(record);
      } else record.state = "cleanup_failed";
    } finally {
      void this.#notifyOwnedIdle().catch(() => {});
    }
  }

  #releaseCreate(record: PendingCreate): void {
    if (!record.reservationHeld) return;
    record.reservationHeld = false;
    this.#sessionReservations--;
    this.#pendingCreates.delete(record);
  }

  #sendPrompt(request: Extract<BridgeRequest, { method: "send_prompt" }>): void {
    const session = this.#get(request.session_id);
    this.#assertRunAdmission(session);
    if (session.remoteBusy || session.cancelPending) throw coded("SESSION_BUSY", "Paseo session still has a remote turn or cancellation in progress");
    this.#ok(request.request_id, { accepted: true });
    this.#launchTurn(request.session_id, session, request.prompt, request.timeout_ms);
  }

  #beginRemote(session: Session, prompt: string, timeoutMs: number, outputSchema?: Record<string, unknown>): { generation: number; run: Promise<RunResult> } {
    this.#assertRunAdmission(session);
    session.remoteBusy = true;
    session.terminalCleanupOnUpdate = false;
    session.turnSettlementPending = true;
    session.agentStatus = "running";
    session.observing = true;
    const generation = ++session.generation;
    const run = Promise.resolve().then(() => session.agent.run(prompt, { timeoutMs, ...(outputSchema === undefined ? {} : { outputSchema }) }));
    void run.then((result) => this.#remoteSettled(session, generation, result), () => this.#remoteSettled(session, generation, null));
    return { generation, run };
  }

  #remoteSettled(session: Session, generation: number, result: RunResult | null): void {
    if (generation !== session.generation) return;
    session.turnSettlementPending = false;
    if (result?.agentStatus !== undefined && result.agentStatus !== null) session.agentStatus = result.agentStatus;
    if (isTerminalStatus(session.agentStatus)) session.remoteBusy = false;
    session.observing = false;
    session.cancelObserver = null;
    if (result !== null) this.#recordActivity(session, "run_settled", result);
    if (result !== null && isTerminalStatus(result.agentStatus) && session.owned?.lifecycle === "ephemeral") this.#scheduleArchive(session, "terminal_result");
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
        if (cancelled) { session.remoteBusy = false; session.turnSettlementPending = false; session.generation++; session.cancelObserver = null; }
        else if (isTerminalStatus(session.agentStatus)) session.remoteBusy = false;
      };
      try { remoteCancelled = await deadline(cancellation, this.limits.remoteCancelTimeoutMs, "CANCEL_TIMEOUT"); finalize(remoteCancelled); }
      catch (error) {
        cancelError = errorMessage(error);
        void cancellation.then(finalize, () => finalize(false));
      }
    }
    if (remoteCancelled && session.owned?.lifecycle === "ephemeral") this.#scheduleArchive(session, "remote_cancelled");
    if ((!hadRemoteTurn || !this.backend.supportsRemoteCancel) && session.generation === generation) {
      session.cancelPending = false;
    }
    if (hadRemoteTurn && !remoteCancelled && session.owned?.lifecycle === "ephemeral") session.terminalCleanupOnUpdate = true;
    this.#event(request.session_id, "observation_cancelled", { remote_cancelled: remoteCancelled, remote_agent_may_still_be_running: hadRemoteTurn && !remoteCancelled });
    this.#ok(request.request_id, { cancelled: hadRemoteTurn, remote_cancelled: remoteCancelled, remote_agent_may_still_be_running: hadRemoteTurn && !remoteCancelled, ...(cancelError === null ? {} : { cancel_error: cancelError.slice(0, 4096) }) });
    if (session.owned?.purpose === "auto_setup" && !session.remoteBusy && !session.cancelPending) {
      this.#dispose(session);
      this.#sessions.delete(request.session_id);
    }
  }

  async #resume(request: Extract<BridgeRequest, { method: "resume_session" }>): Promise<void> {
    if (this.#sessions.has(request.session_id)) return this.#ok(request.request_id, { session_id: request.session_id, resumed: true });
    let pending = this.#resumePending.get(request.session_id);
    if (pending === undefined) {
      this.#reserveSession();
      pending = this.#resumeOne(request.session_id, request.purpose as SessionPurpose | undefined);
      this.#resumePending.set(request.session_id, pending);
      void pending.finally(() => this.#resumePending.delete(request.session_id)).catch(() => {});
    }
    const session = await pending;
    this.#ok(request.request_id, { session_id: session.agent.id, resumed: true });
  }

  async #resumeOne(id: string, purpose: SessionPurpose | undefined): Promise<Session> {
    try {
      const agent = this.backend.refAgent(id);
      const initial = this.ledger === null ? null : await deadline(this.ledger.readByAgent(id), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT");
      if (initial === null) {
        // Legacy unowned resume never touches the owned ledger, so it stays
        // available without the lease.
        const refreshed = await deadline(agent.refresh(), this.limits.defaultTimeoutMs, "TIMEOUT");
        if (!refreshed.exists) throw coded("NOT_FOUND", "Paseo session not found");
        if (refreshed.archivedAt !== null) throw coded("NOT_RESUMABLE", "archived Paseo session cannot be resumed");
        if (this.#state !== "open") throw coded("BRIDGE_CLOSED", "bridge closed while resuming session");
        const existing = this.#sessions.get(agent.id);
        if (existing !== undefined) return existing;
        const session = this.#attach(agent, null);
        session.remoteBusy = refreshed.running;
        session.agentStatus = refreshed.running ? "running" : "idle";
        session.observing = refreshed.running;
        this.#sessions.set(agent.id, session);
        return session;
      }
      // Managed resume mutates shared ownership state through later activity
      // writes, so it serializes on the same exclusive lease as creation.
      // Pins span acquisition through attach so a concurrent idle-release
      // cannot drop the lease mid-setup; the attached session guards
      // everything after.
      let entered = false;
      try {
        await this.#enterOwned(this.limits.defaultTimeoutMs);
        entered = true;
        // Re-read (and re-refresh below) after acquisition: the previous owner
        // may have settled the record while this bridge waited as standby.
        const owned = await deadline(this.ledger!.readByAgent(id), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT") ?? initial;
        if (owned.lifecycle !== "resumable") throw coded("NOT_RESUMABLE", "managed ephemeral session cannot be resumed");
        if (owned.state === "archived") throw coded("NOT_RESUMABLE", "archived managed session cannot be resumed");
        const refreshed = await deadline(agent.refresh(), this.limits.defaultTimeoutMs, "TIMEOUT");
        this.#assertOpenForOwned();
        if (!refreshed.exists) throw coded("NOT_FOUND", "Paseo session not found");
        if (refreshed.archivedAt !== null) throw coded("NOT_RESUMABLE", "archived Paseo session cannot be resumed");
        const placement = await this.#ensureWorkspace();
        this.#assertOpenForOwned();
        if (refreshed.workspaceId !== owned.workspaceId || owned.workspaceId !== placement.id || (purpose !== undefined && purpose !== owned.purpose)) throw coded("OWNERSHIP_MISMATCH", "managed session workspace or purpose does not match its ledger");
        if (this.#state !== "open") throw coded("BRIDGE_CLOSED", "bridge closed while resuming session");
        const existing = this.#sessions.get(agent.id);
        if (existing !== undefined) return existing;
        const session = this.#attach(agent, owned);
        session.remoteBusy = refreshed.running;
        session.agentStatus = refreshed.running ? "running" : "idle";
        session.observing = refreshed.running;
        this.#sessions.set(agent.id, session);
        return session;
      } finally {
        if (entered) await this.#exitOwned();
      }
    } finally { this.#sessionReservations--; }
  }

  async #proposal(request: Extract<BridgeRequest, { method: "request_proposal" }>): Promise<void> {
    const session = this.#get(request.session_id);
    this.#assertRunAdmission(session);
    if (session.remoteBusy || session.cancelPending) throw coded("SESSION_BUSY", "Paseo session still has a remote turn or cancellation in progress");
    const timeoutMs = request.timeout_ms ?? this.limits.defaultTimeoutMs;
    const operation = this.#beginRemote(session, proposalPrompt(request), timeoutMs, proposalJsonSchema(request.kind, request.originating_revision));
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
    // Automatic setup is a one-shot lvu operation, but its Paseo conversation
    // is durable user-visible evidence. Detach the local bridge observer once
    // the proposal has settled so the bounded live-session capacity is free;
    // the resumable Paseo agent and owned-session ledger entry remain intact.
    if (session.owned?.purpose === "auto_setup" && session.owned.lifecycle === "resumable") {
      this.#dispose(session);
      this.#sessions.delete(request.session_id);
    }
  }

  #attach(agent: AgentHandle, owned: OwnedSessionRecord | null = null): Session {
    const session: Session = { agent, remoteBusy: false, observing: false, generation: 0, cancelObserver: null, cancelPending: false, agentStatus: null, unsubscribers: [], owned, cleanupScheduled: false, terminalCleanupOnUpdate: false, turnSettlementPending: false };
    session.unsubscribers.push(agent.subscribeStream((payload) => { this.#recordActivity(session, "stream", payload); if (this.#state === "open" && session.observing) this.#event(agent.id, "stream", { payload }); }));
    session.unsubscribers.push(agent.subscribeUpdate((payload) => {
      this.#recordActivity(session, "update", payload);
      const status = statusFromUpdate(payload);
      if (status !== null) session.agentStatus = status;
      if (status === "running" || status === "initializing") session.remoteBusy = true;
      else if (isTerminalStatus(status) && !session.cancelPending && (!session.turnSettlementPending || session.terminalCleanupOnUpdate)) session.remoteBusy = false;
      if (this.#state === "open" && session.observing) this.#event(agent.id, "session_update", { payload });
      if (isTerminalStatus(status)) session.observing = false;
      if (isTerminalStatus(status) && session.terminalCleanupOnUpdate && session.owned?.lifecycle === "ephemeral") { session.turnSettlementPending = false; this.#scheduleArchive(session, "recovered_terminal_update", true); }
    }));
    return session;
  }
  #cleanupOwned(agent: AgentHandle, requestId: string, owned: OwnedSessionRecord | null = null, timeoutMs = this.limits.defaultTimeoutMs): Promise<boolean> {
    let task!: Promise<void>;
    let succeeded = false;
    const deadlineAt = Date.now() + timeoutMs;
    task = (async () => {
      if (owned !== null) {
        await deadline(this.ledger!.flush(owned), remaining(deadlineAt), "LEDGER_TIMEOUT");
        const current = await deadline(this.ledger!.read(owned.ownershipId), remaining(deadlineAt), "LEDGER_TIMEOUT");
        if (current === null || current.activityLost) throw new Error("owned activity persistence is incomplete");
      }
      await deadline(this.backend.cleanupOwnedAgent(agent, remaining(deadlineAt)), remaining(deadlineAt), "CLEANUP_TIMEOUT");
      const snapshot = await deadline(agent.refresh(), remaining(deadlineAt), "CLEANUP_TIMEOUT");
      if (snapshot.archivedAt === null) throw new Error("Paseo did not confirm archival");
      if (owned !== null) {
        await deadline(this.ledger!.flush(owned), remaining(deadlineAt), "LEDGER_TIMEOUT");
        await deadline(this.ledger!.update(owned, { state: "archived", archivedAt: snapshot.archivedAt, lastUserMessageAt: snapshot.lastUserMessageAt, ...(snapshot.updatedAt === null ? {} : { updatedAt: snapshot.updatedAt }) }), remaining(deadlineAt), "LEDGER_TIMEOUT");
      }
      succeeded = true;
      this.#event(agent.id, "session_archived", { request_id: requestId, archived_at: snapshot.archivedAt, ...(owned === null ? {} : { activity_path: this.ledger!.activityPath(owned) }) });
    })()
      .catch(async (error) => {
        if (owned !== null) await deadline(this.ledger!.update(owned, { state: "archive_failed", lastError: errorMessage(error).slice(0, 4096) }), Math.max(1, timeoutMs), "LEDGER_TIMEOUT");
        this.#event(agent.id, owned === null ? "owned_cleanup_failed" : "archive_failed", { request_id: requestId, agent_id: agent.id, error: errorMessage(error).slice(0, 4096), ...(owned === null ? {} : { activity_path: this.ledger!.activityPath(owned) }) });
      })
      .finally(() => this.#cleanupTasks.delete(task));
    this.#cleanupTasks.add(task);
    return task.then(() => succeeded);
  }
  #recordActivity(session: Session, kind: string, payload: unknown): void { if (session.owned !== null) this.ledger!.recordActivity(session.owned, kind, payload); }
  #scheduleArchive(session: Session, reason: string, recoverPending = false): void {
    if (session.owned === null || session.cleanupScheduled || session.owned.state === "archived" || (!recoverPending && session.owned.state === "pending_cleanup")) return;
    session.cleanupScheduled = true;
    session.owned.state = "pending_cleanup";
    this.ledger!.recordActivity(session.owned, "cleanup_requested", { reason });
    let task!: Promise<void>;
    task = deadline(this.ledger!.update(session.owned, { state: "pending_cleanup" }), this.limits.defaultTimeoutMs, "LEDGER_TIMEOUT")
      .then(async (record) => {
        session.owned = record;
        if (await this.#cleanupOwned(session.agent, record.protocolRequestId, record)) {
          this.#dispose(session);
          this.#sessions.delete(session.agent.id);
          await this.ledger!.releaseMemory(record);
        }
      })
      .catch((error) => this.#event(session.agent.id, "archive_failed", { request_id: session.owned!.protocolRequestId, error: errorMessage(error).slice(0, 4096), activity_path: this.ledger!.activityPath(session.owned!) }))
      .finally(() => { session.cleanupScheduled = false; this.#cleanupTasks.delete(task); void this.#notifyOwnedIdle().catch(() => {}); });
    this.#cleanupTasks.add(task);
  }
  async #ensureWorkspace(): Promise<{ id: string; projectId: string | null; directory: string }> {
    if (this.ledger === null) throw new Error("owned session ledger unavailable");
    if (this.#workspacePromise === null) {
      this.#workspacePending = true;
      this.#workspacePromise = (async () => {
      const stored = await this.ledger!.readWorkspace();
      const placement = await this.backend.ensureWorkspace(this.ledger!.root, stored?.workspaceId);
      if (!this.ledger!.ownsLease || this.#state === "closed") throw coded("BRIDGE_CLOSED", "bridge closed while preparing owned workspace");
      if (stored !== null && (placement.id !== stored.workspaceId || placement.directory !== stored.directory || placement.projectId !== stored.projectId)) throw new Error("stored lvu workspace identity changed");
      await this.ledger!.writeWorkspace({ version: 1, workspaceId: placement.id, projectId: placement.projectId, directory: placement.directory });
      return placement;
      })().catch((error) => { this.#workspacePromise = null; throw error; })
        .finally(() => { this.#workspacePending = false; void this.#notifyOwnedIdle().catch(() => {}); });
    }
    return this.#workspacePromise;
  }
  #launchRecovery(): Promise<void> {
    if (this.#recoveryTask !== null) return this.#recoveryTask;
    if (this.ledger === null || !this.ledger.ownsLease || !this.#recoveryNeeded) return Promise.resolve();
    this.#recoveryNeeded = false;
    let task!: Promise<void>;
    task = this.#recoverOwned(this.limits.remoteCancelTimeoutMs)
      .catch((error) => this.#event("bridge", "owned_recovery_deferred", { error: errorMessage(error).slice(0, 4096) }))
      .finally(() => {
        this.#cleanupTasks.delete(task);
        if (this.#recoveryTask === task) this.#recoveryTask = null;
        void this.#notifyOwnedIdle().catch(() => {});
      });
    this.#recoveryTask = task;
    this.#cleanupTasks.add(task);
    return task;
  }
  // One mutator, many windows: the exclusive bridge.lock serializes ledger
  // mutation across processes, while #leaseMutex serializes this bridge's own
  // lease transitions (acquire vs idle-release) and #leasePins spans each
  // setup window between acquisition and tracking insertion. A busy result
  // keeps the same stable OWNED_ROOT_BUSY marker and exact lock path the
  // startup path emits (never a deletion), and the bridge stays open so the
  // next request retries after the owner goes idle or exits. Recovery runs on
  // every fresh acquisition, while still held, with unresolved-create reports
  // deduplicated below.
  async #guarded<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.#leaseMutex;
    let release!: () => void;
    const current = new Promise<void>((resolve) => { release = resolve; });
    this.#leaseMutex = current;
    await previous;
    try { return await operation(); }
    finally { release(); }
  }
  async #enterOwned(timeoutMs: number): Promise<void> {
    if (this.ledger === null) throw coded("OWNED_ROOT_UNAVAILABLE", "managed assistance requires LVU_PASEO_OWNED_ROOT");
    const deadlineAt = Date.now() + timeoutMs;
    let recovery = Promise.resolve();
    await this.#guarded(async () => {
      if (this.#state !== "open") throw coded("BRIDGE_CLOSED", "bridge closed while acquiring owned lease");
      if (!this.ledger!.ownsLease) {
        await this.#acquireOwnedLease(remaining(deadlineAt));
        if (this.#state !== "open") {
          // Close raced a successful acquisition: release the just-won
          // nonce-guarded lease so no stale lock leaks, and report closure.
          // Foreign locks are never unlinked (release only removes its nonce).
          await this.#releaseIfIdleUnderGuard();
          throw coded("BRIDGE_CLOSED", "bridge closed while acquiring owned lease");
        }
      }
      this.#leasePins++;
      recovery = this.#launchRecovery();
    });
    try {
      // Recovery may archive old ephemeral sessions. It must finish before
      // a new marker/agent is created, including startup's existing recovery.
      await deadline(recovery, remaining(deadlineAt), "RECOVERY_TIMEOUT");
      this.#assertOpenForOwned();
    } catch (error) {
      await this.#exitOwned();
      throw error;
    }
  }

  async #acquireOwnedLease(timeoutMs: number): Promise<void> {
    if (this.#leaseAcquisition === null) {
      const raw = this.ledger!.acquireLease().then(() => { this.#recoveryNeeded = true; });
      this.#leaseAcquisition = raw;
      void raw.finally(() => {
        if (this.#leaseAcquisition === raw) this.#leaseAcquisition = null;
      }).catch(() => {});
    }
    const raw = this.#leaseAcquisition;
    try { await deadline(raw, timeoutMs, "OWNED_ROOT_BUSY"); }
    catch (error) {
      // Arm cleanup before any close-state early return. A public deadline
      // cannot cancel O_EXCL/write/fsync, including the startup acquisition.
      void raw.then(() => this.#adoptLateAcquisition(), () => {}).catch(() => {});
      if (this.#state !== "open" && this.#state !== "starting" && errorCode(error) === "OWNED_ROOT_BUSY") {
        throw coded("BRIDGE_CLOSED", "bridge closed while acquiring owned lease");
      }
      throw error;
    }
  }
  // Settles a previously timed-out acquisition that eventually won: adopt it
  // (with the recovery the win skipped) while open, or release its nonce
  // while closing/closed, so a live bridge never leaks a stale lock. Only a
  // win after process exit still needs the dead-process stale-lock guidance.
  // Never throws.
  async #adoptLateAcquisition(): Promise<void> {
    await this.#guarded(async () => {
      try {
        if (this.ledger === null || !this.ledger.ownsLease) return;
        if (this.#state !== "open") {
          await this.#releaseIfIdleUnderGuard();
          return;
        }
        this.#launchRecovery();
      } catch { /* opportunistic; close and later requests reconcile */ }
    });
  }
  async #exitOwned(): Promise<void> {
    await this.#guarded(async () => {
      if (this.#leasePins > 0) this.#leasePins--;
      await this.#releaseIfIdleUnderGuard();
    });
  }
  #notifyOwnedIdle(): Promise<void> {
    if (this.ledger === null) return Promise.resolve();
    return this.#guarded(() => this.#releaseIfIdleUnderGuard());
  }
  // Caller must hold #leaseMutex. Releases the lease once nothing remains
  // that could write to the ledger: no pins, no owned sessions (unmanaged
  // sessions never touch the ledger), no creates/resumes/cleanups/recovery in
  // flight, and no unsettled ledger queues. Detached auto_setup activity is
  // covered because its listeners are removed at detach (no new writes) and
  // its queued writes keep hasUnsettledWork true until they settle, at which
  // point the ledger hook re-triggers this check. A bridge whose startup
  // recovery settles with no live work releases even if it never did a
  // managed request, so idle windows never squat the lease. Never throws.
  async #releaseIfIdleUnderGuard(): Promise<void> {
    try {
      if (this.#state === "new" || this.#state === "starting") return;
      if (this.ledger === null) return;
      if (!this.ledger.ownsLease) {
        if (this.#state === "closed" && this.#leaseAcquisition === null
          && !this.#workspacePending && !this.ledger.hasUnsettledWork) this.ledger.hooks.onQuiescent = null;
        return;
      }
      if (this.#leasePins !== 0) return;
      if (this.#leaseAcquisition !== null || this.#workspacePending) return;
      if (this.#state === "open" && this.#recoveryNeeded) return;
      for (const session of this.#sessions.values()) if (session.owned !== null) return;
      if (this.#pendingCreates.size !== 0 || this.#cleanupTasks.size !== 0 || this.#resumePending.size !== 0) return;
      if (this.ledger.hasUnsettledWork) return;
      await this.ledger.releaseLease().catch(() => {});
      // Reload on next acquisition: re-read and re-verify workspace identity
      // instead of trusting the memo across another bridge's tenure.
      this.#workspacePromise = null;
      if (this.#state === "closed") this.ledger.hooks.onQuiescent = null;
    } catch { /* idle release is opportunistic; the next request retries */ }
  }
  #assertOpenForOwned(): void {
    if (this.#state !== "open") throw coded("BRIDGE_CLOSED", "bridge closed while preparing owned session");
  }
  async #recoverOwned(budgetMs: number): Promise<void> {
    if (this.ledger === null) return;
    const deadlineAt = Date.now() + budgetMs;
    const pending = await deadline(this.ledger.recoveryBatch(this.limits.maxSessions), remaining(deadlineAt), "RECOVERY_TIMEOUT");
    if (pending.length === 0) return;
    const placement = await deadline(this.#ensureWorkspace(), remaining(deadlineAt), "RECOVERY_TIMEOUT");
    for (const record of pending) {
      if (Date.now() >= deadlineAt) return;
      if (record.lifecycle === "resumable") continue;
      if (record.agentId === undefined) {
        // Agentless markers persist (nothing can resolve them), so repeated
        // recoveries across idle-release cycles report each one only once per
        // bridge lifetime. Bounded: one entry per distinct pending id, reset
        // once past twice the pending-marker limit.
        if (!this.#recoveryReportedUnresolved.has(record.ownershipId)) {
          if (this.#recoveryReportedUnresolved.size >= 2 * 1024) this.#recoveryReportedUnresolved.clear();
          this.#recoveryReportedUnresolved.add(record.ownershipId);
          this.#event("bridge", "owned_create_unresolved", { request_id: record.protocolRequestId, ownership_id: record.ownershipId, state: record.state });
        }
        continue;
      }
      const agent = this.backend.refAgent(record.agentId!);
      const snapshot = await deadline(agent.refresh(), remaining(deadlineAt), "RECOVERY_TIMEOUT").catch(() => null);
      if (snapshot === null || !snapshot.exists || snapshot.workspaceId !== record.workspaceId || record.workspaceId !== placement.id) continue;
      if (snapshot.running || !isTerminalStatus(snapshot.status)) {
        if (this.#state === "open" && this.#sessions.size + this.#sessionReservations < this.limits.maxSessions && !this.#sessions.has(agent.id)) {
          const session = this.#attach(agent, record); session.remoteBusy = snapshot.running; session.agentStatus = snapshot.status; session.observing = false; session.terminalCleanupOnUpdate = true; this.#sessions.set(agent.id, session);
        }
        continue;
      }
      this.ledger.recordActivity(record, "cleanup_recovered", { status: snapshot.status, archived_at: snapshot.archivedAt });
      if (snapshot.archivedAt !== null) {
        await deadline(this.ledger.flush(record), remaining(deadlineAt), "LEDGER_TIMEOUT");
        await deadline(this.ledger.update(record, { state: "archived", archivedAt: snapshot.archivedAt, lastUserMessageAt: snapshot.lastUserMessageAt }), remaining(deadlineAt), "LEDGER_TIMEOUT");
        this.#event(agent.id, "session_archived", { request_id: record.protocolRequestId, archived_at: snapshot.archivedAt, activity_path: this.ledger.activityPath(record), recovered: true });
        await this.ledger.releaseMemory(record);
        continue;
      }
      if (await this.#cleanupOwned(agent, record.protocolRequestId, record, remaining(deadlineAt))) await this.ledger.releaseMemory(record);
    }
  }
  #assertRunAdmission(session: Session): void {
    if (session.owned !== null && (session.cleanupScheduled || session.owned.activityLost || session.owned.state === "pending_cleanup" || session.owned.state === "archive_failed" || session.owned.state === "archived")) throw coded("SESSION_CLOSING", "managed session cleanup is pending");
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


function deadline<T>(promise: Promise<T>, timeoutMs: number, code: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(coded(code, `operation exceeded ${timeoutMs}ms`)), timeoutMs);
    promise.then((value) => { clearTimeout(timer); resolve(value); }, (error) => { clearTimeout(timer); reject(error); });
  });
}
function remaining(deadlineAt: number): number { return Math.max(1, deadlineAt - Date.now()); }
function coded(code: string, message: string): Error { return Object.assign(new Error(message), { code }); }
function errorCode(error: unknown): string { return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string" ? error.code : "INTERNAL_ERROR"; }
function errorMessage(error: unknown): string { return error instanceof Error ? error.message : String(error); }
// `Daemon client closed`, a refused socket and a connect timeout all mean the
// same thing to the user: this process is up, the daemon it talks to is not.
function daemonErrorCode(error: unknown): string {
  const message = errorMessage(error);
  if (errorCode(error) === "TIMEOUT") return "DAEMON_TIMEOUT";
  return /daemon|econnrefused|socket|websocket|closed|disconnect/i.test(message)
    ? "DAEMON_UNREACHABLE"
    : "PROVIDERS_UNAVAILABLE";
}
function isTerminalStatus(status: unknown): status is "idle" | "error" | "closed" { return status === "idle" || status === "error" || status === "closed"; }
function statusFromUpdate(update: unknown): unknown {
  if (typeof update !== "object" || update === null || !("kind" in update) || update.kind !== "upsert" || !("agent" in update) || typeof update.agent !== "object" || update.agent === null || !("status" in update.agent)) return null;
  return update.agent.status;
}
