import { PassThrough, Writable } from "node:stream";
import { describe, expect, it } from "vitest";
import { Bridge } from "../src/bridge.js";
import { CliLifecycle } from "../src/cli_lifecycle.js";
import { deferred, FakeBackend } from "./fake-backend.js";

const bridgeLimits = { maxSessions: 2, defaultTimeoutMs: 100, remoteCancelTimeoutMs: 20, maxProposalBytes: 2048, maxEventBytes: 2048 };
const serverLimits = { maxLineBytes: 128, maxInFlight: 1, maxCancelInFlight: 1, maxQueuedOutputBytes: 512, shutdownDrainTimeoutMs: 20 };

function fixture(output: Writable = new PassThrough()) {
  const backend = new FakeBackend();
  const input = new PassThrough();
  const startup: Array<Record<string, unknown>> = [];
  let send = (message: Record<string, unknown>) => { startup.push(message); };
  const bridge = new Bridge(backend, (message) => {
    send(message);
  }, bridgeLimits);
  const diagnostics: string[] = [];
  const lifecycle = new CliLifecycle(
    bridge,
    input,
    output,
    serverLimits,
    (message) => diagnostics.push(message),
    (server) => {
      send = (message) => server.send(message);
      for (const message of startup.splice(0)) send(message);
    },
  );
  return { backend, input, output, bridge, lifecycle, diagnostics };
}

describe("CLI startup lifecycle", () => {
  it("honours no-request stdin EOF while backend startup is delayed", async () => {
    const h = fixture();
    const connect = deferred<void>();
    h.backend.connectResult = connect;
    const starting = h.lifecycle.start();
    h.input.end();
    connect.resolve();
    expect(await starting).toBeNull();
    expect(h.backend.closeCalls).toBe(1);
  });

  it.each(["\n", ""])("preserves delayed-start request plus EOF (suffix %j)", async (suffix) => {
    const h = fixture();
    const connect = deferred<void>();
    h.backend.connectResult = connect;
    const chunks: Buffer[] = [];
    h.output.on("data", (chunk: Buffer) => chunks.push(chunk));
    const starting = h.lifecycle.start();
    h.input.end(`{"schema_version":1,"request_id":"probe","method":"capabilities"}${suffix}`);
    connect.resolve();
    const server = await starting;
    expect(server).not.toBeNull();
    await h.lifecycle.settleInput();
    expect(Buffer.concat(chunks).toString()).toContain('"request_id":"probe","ok":true');
    expect(h.backend.closed).toBe(true);
  });

  it("drains a second staged request after a synchronous error blocks output", async () => {
    const callbacks: Array<() => void> = [];
    const chunks: Buffer[] = [];
    const output = new Writable({
      highWaterMark: 1,
      write(chunk, _encoding, callback) {
        chunks.push(Buffer.from(chunk));
        callbacks.push(callback);
      },
    });
    const h = fixture(output);
    let capabilityCalls = 0;
    h.backend.listProviders = async () => {
      capabilityCalls++;
      return h.backend.providers;
    };
    const connect = deferred<void>();
    h.backend.connectResult = connect;
    const starting = h.lifecycle.start();
    h.input.write("{malformed\n");
    h.input.end('{"schema_version":1,"request_id":"probe","method":"capabilities"}\n');
    connect.resolve();
    expect(await starting).not.toBeNull();
    await h.lifecycle.settleInput();
    const text = Buffer.concat(chunks).toString();
    expect(text).toContain("INVALID_JSON");
    expect(capabilityCalls).toBe(1);
    expect(h.diagnostics).toContain("stdout did not drain before shutdown deadline");
    expect(h.backend.closeCalls).toBe(1);
    for (const callback of callbacks) callback();
  });

  it("settles when staged input errors after the server starts", async () => {
    const h = fixture();
    expect(await h.lifecycle.start()).not.toBeNull();
    h.input.destroy(new Error("fixture input failed"));
    await h.lifecycle.settleInput();
    expect(h.diagnostics).toContain("stdin failed: fixture input failed");
    expect(h.backend.closeCalls).toBe(1);
  });

  it("preserves live-input backpressure until upstream ends", async () => {
    let release!: () => void;
    let first = true;
    const chunks: Buffer[] = [];
    const output = new Writable({
      highWaterMark: 1,
      write(chunk, _encoding, callback) {
        chunks.push(Buffer.from(chunk));
        if (first) { first = false; release = callback; }
        else callback();
      },
    });
    const h = fixture(output);
    let capabilityCalls = 0;
    h.backend.listProviders = async () => { capabilityCalls++; return h.backend.providers; };
    const server = await h.lifecycle.start();
    const settling = h.lifecycle.settleInput();
    h.input.write("{malformed\n");
    h.input.write('{"schema_version":1,"request_id":"probe","method":"capabilities"}\n');
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(server!.input.isPaused()).toBe(true);
    expect(capabilityCalls).toBe(0);
    expect(h.backend.closeCalls).toBe(0);
    release();
    await new Promise<void>((resolve) => setImmediate(resolve));
    h.input.end();
    await settling;
    expect(capabilityCalls).toBe(1);
    expect(Buffer.concat(chunks).toString()).toContain('"request_id":"probe","ok":true');
    expect(h.diagnostics).toEqual([]);
  });

  it("reports an input error during delayed startup without an unhandled stream error", async () => {
    const h = fixture();
    const connect = deferred<void>();
    h.backend.connectResult = connect;
    const starting = h.lifecycle.start();
    h.input.destroy(new Error("early input failure"));
    await new Promise<void>((resolve) => setImmediate(resolve));
    connect.resolve();
    expect(await starting).toBeNull();
    await h.lifecycle.settleInput();
    expect(h.diagnostics).toEqual(["stdin failed: early input failure"]);
    expect(h.backend.closeCalls).toBe(1);
  });

  it("settles a stopped server while upstream remains open", async () => {
    const output = new PassThrough();
    const h = fixture(output);
    expect(await h.lifecycle.start()).not.toBeNull();
    const settling = h.lifecycle.settleInput();
    output.destroy(new Error("output failed"));
    await settling;
    expect(h.diagnostics).toContain("output failed");
    expect(h.backend.closeCalls).toBe(1);
    h.input.destroy();
  });

  it("propagates startup failure and releases open stdin after closing the backend", async () => {
    const h = fixture();
    const connect = deferred<void>();
    h.backend.connectResult = connect;
    const starting = h.lifecycle.start();
    connect.reject(new Error("connect failed"));
    await expect(starting).rejects.toThrow("connect failed");
    expect(h.backend.closeCalls).toBe(1);
    expect(h.input.destroyed).toBe(true);
  });
});
