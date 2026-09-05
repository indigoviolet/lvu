import { describe, expect, it } from "vitest";
import { parseRequestLine } from "../src/jsonl.js";

describe("JSONL framing", () => {
  it("rejects malformed JSON without fabricating a request id", () => {
    expect(parseRequestLine("{broken", 100)).toMatchObject({ ok: false, response: { request_id: null, error: { code: "INVALID_JSON" } } });
  });

  it("correlates a structurally invalid request when its id is usable", () => {
    expect(parseRequestLine(JSON.stringify({ schema_version: 1, request_id: "bad-7", method: "unknown" }), 1000)).toMatchObject({ ok: false, response: { request_id: "bad-7", error: { code: "INVALID_REQUEST" } } });
  });

  it("checks UTF-8 bytes rather than JavaScript character count", () => {
    expect(parseRequestLine(`{"${"💥".repeat(8)}":1}`, 16)).toMatchObject({ ok: false, response: { error: { code: "REQUEST_TOO_LARGE" } } });
  });
});
