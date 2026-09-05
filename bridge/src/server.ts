import type { Readable, Writable } from "node:stream";
import type { Bridge } from "./bridge.js";
import { parseRequestLine } from "./jsonl.js";
import { SCHEMA_VERSION, type BridgeRequest } from "./protocol.js";

export interface ServerLimits { maxLineBytes: number; maxInFlight: number; maxCancelInFlight: number; maxQueuedOutputBytes: number; shutdownDrainTimeoutMs: number; }

export class JsonlServer {
  #line = Buffer.alloc(0);
  #drainingOversize = false;
  #accepting = true;
  #stopping: Promise<void> | null = null;
  readonly #inFlight = new Map<string, { request: BridgeRequest; promise: Promise<void> }>();
  #normalInFlight = 0;
  #cancelInFlight = 0;
  readonly #writer: BoundedWriter;

  constructor(readonly bridge: Bridge, readonly input: Readable, output: Writable, readonly limits: ServerLimits, readonly diagnostic: (message: string) => void = () => {}) {
    this.#writer = new BoundedWriter(output, input, limits.maxQueuedOutputBytes, (error) => { this.diagnostic(error.message); void this.stop(); });
  }

  start(): void {
    this.input.on("data", (chunk: Buffer | string) => this.#consume(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk)));
    this.input.once("end", () => {
      if (!this.#drainingOversize && this.#line.length > 0) this.#dispatchLine(this.#line);
      this.#line = Buffer.alloc(0);
      void this.stop();
    });
    this.input.once("error", (error) => { this.diagnostic(`stdin failed: ${error.message}`); void this.stop(); });
  }

  send(message: Record<string, unknown>): void { this.#writer.send(message); }

  async stop(): Promise<void> {
    if (this.#stopping !== null) return this.#stopping;
    this.#accepting = false;
    this.input.pause();
    this.#stopping = (async () => {
      await this.bridge.close().catch((error) => this.diagnostic(`bridge shutdown failed: ${String(error)}`));
      await Promise.allSettled([...this.#inFlight.values()].map(({ promise }) => promise));
      await this.#writer.stop(this.limits.shutdownDrainTimeoutMs);
    })();
    return this.#stopping;
  }

  #consume(chunk: Buffer): void {
    if (!this.#accepting) return;
    let offset = 0;
    while (offset < chunk.length) {
      const newline = chunk.indexOf(0x0a, offset);
      const end = newline === -1 ? chunk.length : newline;
      if (!this.#drainingOversize) {
        const part = chunk.subarray(offset, end);
        if (this.#line.length + part.length > this.limits.maxLineBytes) {
          this.#line = Buffer.alloc(0);
          this.#drainingOversize = true;
          this.#sendError(null, "REQUEST_TOO_LARGE", "input line exceeds byte limit");
        } else if (part.length > 0) {
          this.#line = this.#line.length === 0 ? Buffer.from(part) : Buffer.concat([this.#line, part], this.#line.length + part.length);
        }
      }
      if (newline === -1) return;
      if (this.#drainingOversize) this.#drainingOversize = false;
      else this.#dispatchLine(this.#line);
      this.#line = Buffer.alloc(0);
      offset = newline + 1;
    }
  }

  #dispatchLine(line: Buffer): void {
    if (line.length === 0) return;
    const parsed = parseRequestLine(line.toString("utf8"), this.limits.maxLineBytes);
    if (!parsed.ok) return void this.#writer.send(parsed.response);
    const request = parsed.request;
    if (this.#inFlight.has(request.request_id)) return this.#sendError(request.request_id, "DUPLICATE_REQUEST_ID", "request_id is already in flight");
    const isCancel = request.method === "cancel";
    if ((!isCancel && this.#normalInFlight >= this.limits.maxInFlight) || (isCancel && this.#cancelInFlight >= this.limits.maxCancelInFlight)) return this.#sendError(request.request_id, "TOO_MANY_REQUESTS", "in-flight request limit reached");
    if (isCancel) this.#cancelInFlight++; else this.#normalInFlight++;
    const promise = this.bridge.handle(request).finally(() => {
      this.#inFlight.delete(request.request_id);
      if (isCancel) this.#cancelInFlight--; else this.#normalInFlight--;
    });
    this.#inFlight.set(request.request_id, { request, promise });
  }

  #sendError(requestId: string | null, code: string, message: string): void { this.#writer.send({ schema_version: SCHEMA_VERSION, request_id: requestId, ok: false, error: { code, message } }); }
}

class BoundedWriter {
  readonly #queue: Array<{ text: string; bytes: number }> = [];
  #queuedBytes = 0;
  #blocked = false;
  #failed = false;
  #stopped = false;
  #flushWaiters: Array<() => void> = [];
  readonly #onDrain = () => { this.#blocked = false; this.#drain(); };
  readonly #onError = (error: Error) => this.#abort(error);
  constructor(readonly output: Writable, readonly input: Readable, readonly maxQueuedBytes: number, readonly fail: (error: Error) => void) {
    output.on("drain", this.#onDrain);
    output.once("error", this.#onError);
  }
  send(message: Record<string, unknown>): void {
    if (this.#failed || this.#stopped) return;
    const text = `${JSON.stringify(message)}\n`;
    const bytes = Buffer.byteLength(text);
    if (!this.#blocked && this.#queue.length === 0) return void this.#write(text);
    if (this.#queuedBytes + bytes > this.maxQueuedBytes) { this.#abort(new Error("stdout backpressure queue exceeded byte limit")); return; }
    this.#queue.push({ text, bytes });
    this.#queuedBytes += bytes;
  }
  async stop(timeoutMs: number): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    this.input.pause();
    if (!this.#failed && (this.#blocked || this.#queue.length > 0)) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const timeout = new Promise<"timeout">((resolve) => { timer = setTimeout(() => resolve("timeout"), timeoutMs); });
      const outcome = await Promise.race([this.#flush().then(() => "flushed" as const), timeout]);
      if (timer !== undefined) clearTimeout(timer);
      if (outcome === "timeout") this.#abort(new Error("stdout did not drain before shutdown deadline"));
    }
    this.#cleanupListeners();
    this.#resolveFlush();
  }
  #flush(): Promise<void> { if ((!this.#blocked && this.#queue.length === 0) || this.#failed) return Promise.resolve(); return new Promise((resolve) => this.#flushWaiters.push(resolve)); }
  #write(text: string): void { if (!this.output.write(text)) { this.#blocked = true; this.input.pause(); } }
  #drain(): void {
    while (!this.#blocked && this.#queue.length > 0) {
      const item = this.#queue.shift()!;
      this.#queuedBytes -= item.bytes;
      this.#write(item.text);
    }
    if (!this.#blocked && this.#queue.length === 0) { if (!this.#failed && !this.#stopped) this.input.resume(); this.#resolveFlush(); }
  }
  #abort(error: Error): void {
    if (this.#failed) return;
    this.#failed = true;
    this.#queue.length = 0;
    this.#queuedBytes = 0;
    this.input.pause();
    this.#cleanupListeners();
    this.#resolveFlush();
    this.fail(error);
  }
  #cleanupListeners(): void { this.output.off("drain", this.#onDrain); this.output.off("error", this.#onError); }
  #resolveFlush(): void { for (const resolve of this.#flushWaiters.splice(0)) resolve(); }
}
