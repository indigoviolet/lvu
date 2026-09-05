#!/usr/bin/env node
import { Bridge } from "./bridge.js";
import { SdkBackend } from "./backend.js";
import { JsonlServer } from "./server.js";

const backend = new SdkBackend({
  url: process.env.LVU_PASEO_URL ?? "ws://127.0.0.1:6767/ws",
  ...(process.env.LVU_PASEO_PASSWORD === undefined ? {} : { password: process.env.LVU_PASEO_PASSWORD }),
  connectTimeoutMs: numberEnv("LVU_PASEO_CONNECT_TIMEOUT_MS", 10_000),
  cliPath: process.env.LVU_PASEO_CLI === "disabled" ? null : (process.env.LVU_PASEO_CLI ?? "paseo"),
  ...(process.env.LVU_PASEO_CLI_HOST === undefined ? {} : { cliHost: process.env.LVU_PASEO_CLI_HOST }),
});
const serverLimits = {
  maxLineBytes: numberEnv("LVU_PASEO_MAX_LINE_BYTES", 262_144),
  maxInFlight: numberEnv("LVU_PASEO_MAX_IN_FLIGHT", 16),
  maxCancelInFlight: numberEnv("LVU_PASEO_MAX_CANCEL_IN_FLIGHT", 4),
  maxQueuedOutputBytes: numberEnv("LVU_PASEO_MAX_QUEUED_OUTPUT_BYTES", 1_048_576),
  shutdownDrainTimeoutMs: numberEnv("LVU_PASEO_SHUTDOWN_DRAIN_TIMEOUT_MS", 2_000),
};
let server: JsonlServer | null = null;
let send: (message: Record<string, unknown>) => void = () => { throw new Error("JSONL server is not ready"); };
const bridge = new Bridge(backend, (message) => send(message), {
  maxSessions: numberEnv("LVU_PASEO_MAX_SESSIONS", 8),
  defaultTimeoutMs: numberEnv("LVU_PASEO_TIMEOUT_MS", 120_000),
  remoteCancelTimeoutMs: numberEnv("LVU_PASEO_CANCEL_TIMEOUT_MS", 5_000),
  maxProposalBytes: numberEnv("LVU_PASEO_MAX_PROPOSAL_BYTES", 262_144),
  maxEventBytes: numberEnv("LVU_PASEO_MAX_EVENT_BYTES", 262_144),
});

try {
  await bridge.start();
  server = new JsonlServer(bridge, process.stdin, process.stdout, serverLimits, (message) => process.stderr.write(`${message}\n`));
  send = (message) => server!.send(message);
  server.start();
} catch (error) {
  process.stderr.write(`bridge connection failed: ${String(error)}\n`);
  process.exitCode = 1;
  await bridge.close().catch(() => {});
}

async function shutdown(): Promise<void> { await server?.stop(); }
process.once("SIGINT", () => void shutdown().finally(() => process.exit(0)));
process.once("SIGTERM", () => void shutdown().finally(() => process.exit(0)));

function numberEnv(name: string, fallback: number): number {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}
