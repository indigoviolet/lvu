import { requestSchema, SCHEMA_VERSION, type BridgeRequest } from "./protocol.js";

export type LineResult = { ok: true; request: BridgeRequest } | { ok: false; response: Record<string, unknown> };

export function parseRequestLine(line: string, maxBytes: number): LineResult {
  if (Buffer.byteLength(line, "utf8") > maxBytes) return failure(null, "REQUEST_TOO_LARGE", "input line exceeds byte limit");
  let value: unknown;
  try { value = JSON.parse(line); } catch { return failure(null, "INVALID_JSON", "input line is not valid JSON"); }
  const parsed = requestSchema.safeParse(value);
  if (parsed.success) return { ok: true, request: parsed.data };
  const requestId = typeof value === "object" && value !== null && "request_id" in value && typeof value.request_id === "string" ? value.request_id : null;
  return failure(requestId, "INVALID_REQUEST", parsed.error.issues.map((issue) => issue.message).join("; "));
}

function failure(requestId: string | null, code: string, message: string): LineResult {
  return { ok: false, response: { schema_version: SCHEMA_VERSION, request_id: requestId, ok: false, error: { code, message: message.slice(0, 4096) } } };
}
