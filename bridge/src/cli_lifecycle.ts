import { PassThrough, type Readable, type Writable } from "node:stream";
import type { Bridge } from "./bridge.js";
import { JsonlServer, type ServerLimits } from "./server.js";

/**
 * Buffers bounded stdin while the backend connects and remembers an EOF that
 * arrives before the JSONL server can be installed.
 */
export class CliLifecycle {
  readonly #stagedInput = new PassThrough();
  #server: JsonlServer | null = null;
  #startTask: Promise<JsonlServer | null> | null = null;
  #shutdownRequested = false;
  #sawInput = false;
  #inputError: Error | null = null;
  readonly #upstreamSettled: Promise<void>;

  constructor(
    readonly bridge: Bridge,
    input: Readable,
    readonly output: Writable,
    readonly limits: ServerLimits,
    readonly diagnostic: (message: string) => void,
    readonly publish: (server: JsonlServer) => void,
  ) {
    let settleUpstream!: () => void;
    this.#upstreamSettled = new Promise((resolve) => { settleUpstream = resolve; });
    input.on("data", () => { this.#sawInput = true; });
    // Startup may still be awaiting the backend when an input error arrives.
    // Remember it here; only destroy the staged stream once its server owns
    // the error listener, otherwise Node would emit an unhandled error.
    input.once("error", (error) => {
      this.#inputError = error;
      if (this.#server !== null) this.#stagedInput.destroy(error);
      settleUpstream();
    });
    // Empty stdin has no protocol work to preserve, so remember its EOF as an
    // early shutdown request. With data, EOF belongs to the staged JSONL
    // stream: its data -> end ordering must dispatch even a final unterminated
    // line before JsonlServer.stop() closes the bridge.
    input.once("end", () => {
      settleUpstream();
      if (!this.#sawInput) void this.shutdown().catch(() => {});
    });
    input.pipe(this.#stagedInput);
  }

  start(): Promise<JsonlServer | null> {
    if (this.#startTask === null) this.#startTask = this.#start();
    return this.#startTask;
  }

  async shutdown(): Promise<void> {
    this.#shutdownRequested = true;
    if (this.#startTask !== null) await this.#startTask;
    if (this.#server !== null) await this.#server.stop();
  }

  /** Wait for protocol EOF after every staged byte has been delivered. */
  async settleInput(): Promise<void> {
    if (this.#server === null) return;
    // The CLI starts waiting immediately after startup. Preserve ordinary
    // backpressure until the real upstream end/error has been observed.
    const reason = await Promise.race([
      this.#upstreamSettled.then(() => "input"),
      this.#server.whenStopped().then(() => "stopped"),
    ]);
    if (reason === "input") await this.#server.finishInput();
  }

  async #start(): Promise<JsonlServer | null> {
    await this.bridge.start();
    // An EOF already queued by pipe teardown must win over publishing the
    // server as ready. Promise continuations run before stream events, so
    // checking immediately after bridge.start() misses that shutdown intent.
    await new Promise<void>((resolve) => setImmediate(resolve));
    if (this.#inputError !== null) {
      this.diagnostic(`stdin failed: ${this.#inputError.message}`);
      await this.bridge.close();
      return null;
    }
    if (this.#shutdownRequested) {
      await this.bridge.close();
      return null;
    }
    const server = new JsonlServer(
      this.bridge,
      this.#stagedInput,
      this.output,
      this.limits,
      this.diagnostic,
    );
    this.#server = server;
    // Responses may be produced synchronously when buffered input is admitted.
    // Install the caller's route before starting the stream.
    this.publish(server);
    server.start();
    return server;
  }
}
