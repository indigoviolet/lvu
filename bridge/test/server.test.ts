import { PassThrough, Writable } from "node:stream";
import { describe, expect, it, vi } from "vitest";
import { Bridge } from "../src/bridge.js";
import { JsonlServer } from "../src/server.js";
import { FakeBackend } from "./fake-backend.js";

const tick = () => new Promise<void>((resolve) => setImmediate(resolve));
const bridgeLimits = { maxSessions: 2, defaultTimeoutMs: 50, remoteCancelTimeoutMs: 20, maxProposalBytes: 2048, maxEventBytes: 2048 };
const serverLimits = { maxLineBytes: 128, maxInFlight: 1, maxCancelInFlight: 1, maxQueuedOutputBytes: 512, shutdownDrainTimeoutMs: 20 };

async function harness(output: Writable = new PassThrough()) {
  const backend = new FakeBackend();
  const input = new PassThrough();
  const diagnostics: string[] = [];
  let server!: JsonlServer;
  const bridge = new Bridge(backend, (message) => server.send(message), bridgeLimits);
  await bridge.start();
  server = new JsonlServer(bridge, input, output, serverLimits, (message) => diagnostics.push(message));
  server.start();
  return { backend, input, output, diagnostics, bridge, server };
}

describe("bounded JSONL server", () => {
  it("drains an oversized unterminated line without retaining it", async () => {
    const output = new PassThrough(); const chunks: Buffer[] = []; output.on("data", (chunk) => chunks.push(chunk));
    const h = await harness(output);
    h.input.write(Buffer.alloc(4096, 0x78)); await tick();
    expect(Buffer.concat(chunks).toString()).toContain("REQUEST_TOO_LARGE");
    h.input.write("\n{broken\n"); await tick();
    expect(Buffer.concat(chunks).toString()).toContain("INVALID_JSON");
    h.input.end(); await h.server.stop();
  });

  it("rejects duplicate and excess in-flight IDs while reserving cancel capacity", async () => {
    const output = new PassThrough(); const chunks: Buffer[] = []; output.on("data", (chunk) => chunks.push(chunk));
    const h = await harness(output); h.backend.deferCreates = true;
    const request = (id: string) => JSON.stringify({ schema_version: 1, request_id: id, method: "start_session", provider: "fake", cwd: "/tmp" }) + "\n";
    h.input.write(request("same")); h.input.write(request("same")); h.input.write(request("other"));
    h.input.write(JSON.stringify({ schema_version: 1, request_id: "cancel", method: "cancel", session_id: "missing" }) + "\n"); await tick();
    const text = Buffer.concat(chunks).toString();
    expect(text).toContain("DUPLICATE_REQUEST_ID"); expect(text).toContain("TOO_MANY_REQUESTS"); expect(text).toContain("NOT_FOUND");
    h.backend.createDeferred[0]!.reject(new Error("done")); await tick(); h.input.end(); await h.server.stop();
  });

  it("terminates instead of accumulating an unbounded slow-output queue", async () => {
    const callbacks: Array<() => void> = [];
    const slow = new Writable({ highWaterMark: 1, write(_chunk, _encoding, callback) { callbacks.push(callback); } });
    const h = await harness(slow);
    for (let index = 0; index < 20; index++) h.server.send({ schema_version: 1, event: "x".repeat(80), index });
    await tick();
    expect(h.diagnostics).toContain("stdout backpressure queue exceeded byte limit");
    for (const callback of callbacks) callback();
    slow.emit("drain");
    await h.server.stop();
  });

  it("bounds shutdown when stdout permanently stops draining", async () => {
    vi.useFakeTimers();
    const slow = new Writable({ highWaterMark: 1, write() {} });
    const h = await harness(slow);
    h.server.send({ schema_version: 1, event: "blocked" });
    const stopping = h.server.stop();
    await vi.advanceTimersByTimeAsync(21); await stopping;
    expect(h.diagnostics).toContain("stdout did not drain before shutdown deadline");
    expect(h.input.isPaused()).toBe(true);
    vi.useRealTimers();
  });
});
