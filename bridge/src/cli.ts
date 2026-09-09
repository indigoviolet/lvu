#!/usr/bin/env node
import { Bridge } from "./bridge.js";
import { SdkBackend } from "./backend.js";
import { JsonlServer } from "./server.js";
import { CliLifecycle } from "./cli_lifecycle.js";
import { OwnedSessionLedger } from "./owned_sessions.js";

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
const startupOutput: Array<Record<string, unknown>> = [];
let send: (message: Record<string, unknown>) => void = (message) => {
  if (startupOutput.length >= 128) throw new Error("too many lifecycle events before JSONL server startup");
  startupOutput.push(message);
};
const ownedRoot = process.env.LVU_PASEO_OWNED_ROOT;
const bridge = new Bridge(backend, (message) => send(message), {
  maxSessions: numberEnv("LVU_PASEO_MAX_SESSIONS", 8),
  defaultTimeoutMs: numberEnv("LVU_PASEO_TIMEOUT_MS", 120_000),
  remoteCancelTimeoutMs: numberEnv("LVU_PASEO_CANCEL_TIMEOUT_MS", 5_000),
  maxProposalBytes: numberEnv("LVU_PASEO_MAX_PROPOSAL_BYTES", 262_144),
  maxEventBytes: numberEnv("LVU_PASEO_MAX_EVENT_BYTES", 262_144),
}, ownedRoot === undefined ? null : new OwnedSessionLedger(ownedRoot));
const lifecycle = new CliLifecycle(
  bridge,
  process.stdin,
  process.stdout,
  serverLimits,
  (message) => process.stderr.write(`${message}\n`),
  (ready) => {
    server = ready;
    send = (message) => ready.send(message);
    for (const message of startupOutput.splice(0)) send(message);
  },
);
const stopForSignal = () => void lifecycle.shutdown().then(
  () => process.exit(0),
  () => process.exit(0),
);
process.once("SIGINT", stopForSignal);
process.once("SIGTERM", stopForSignal);

try {
  server = await lifecycle.start();
  if (server !== null) await lifecycle.settleInput();
} catch (error) {
  process.stderr.write(`bridge connection failed: ${String(error)}\n`);
  process.exitCode = 1;
  await bridge.close().catch(() => {});
}

function numberEnv(name: string, fallback: number): number {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}
