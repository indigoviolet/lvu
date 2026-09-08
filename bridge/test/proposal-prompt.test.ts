import { describe, expect, it } from "vitest";
import { proposalPrompt } from "../src/proposal_prompt.js";
import { MAX_INLINE_CONTEXT_BYTES, MAX_PROPOSAL_PROMPT_BYTES, requestSchema, type BridgeRequest, type ProposalKind } from "../src/protocol.js";

type Request = Extract<BridgeRequest, { method: "request_proposal" }>;
function request(inline: Record<string, unknown>, kind: ProposalKind = "enrichment"): Request {
  return {
    schema_version: 1, request_id: "prepared", method: "request_proposal",
    session_id: "session", kind, instruction: "Recognize the timestamp",
    originating_revision: { data: "frozen-3", definition: "view:7" },
    context: { manifest_path: "/tmp/context.json", dataset_paths: ["/tmp/not-in-prompt.parquet"], inline_context: inline },
  };
}

describe("bounded prepared proposal context", () => {
  it("directs source proposals to discovery JSON without data-definition boilerplate", () => {
    const value = request({ should_not_appear: "inline sample" }, "source");
    value.context.manifest_path = "/tmp/source-discovery/manifest.json";
    value.context.inspection_command = ["/tmp/inspect", "/tmp/parquet-manifest.json"];
    const prompt = proposalPrompt(value);
    expect(prompt).toContain("Source discovery JSON: /tmp/source-discovery/manifest.json");
    expect(prompt).toContain("candidates, cwd, discovery status and identity hints");
    expect(prompt).toContain("file, command or HTTP source");
    expect(prompt).toContain("preserve its concrete path, command arguments, URL and identity hints");
    expect(prompt).not.toContain("inline sample");
    expect(prompt).not.toContain("Parquet");
    expect(prompt).not.toContain("inspection_sample");
    expect(prompt).not.toContain("bounded typed context");
    expect(prompt).not.toContain("The evidence is a bounded sample");
    expect(prompt).not.toContain("bulk dataset contents");
    expect(prompt).not.toContain("pl.Expr");
    expect(prompt).not.toContain("timestamp_utc");
    expect(prompt).not.toContain("recipe_stage_revisions");
  });

  it.each([
    ["filter", "The filter definition's expression must be a single Python expression returning pl.Expr.", false],
    ["enrichment", "Each enrichment expressions value must be a single Python expression returning pl.Expr.", false],
    ["view", "A non-null filter expression must be a single Python expression returning pl.Expr.", true],
  ] as const)("gives %s proposals typed-data and only their applicable definition rules", (kind, expressionRule, viewRules) => {
    const prompt = proposalPrompt(request({ schemas: { s: [{ name: "message", dtype: "String" }] }, rows: [] }, kind));
    expect(prompt).toContain("bounded typed context");
    expect(prompt).toContain("needs_more_data");
    expect(prompt).toContain(expressionRule);
    expect(prompt).not.toContain("must be use");
    expect(prompt).toContain("must supply an explicit format");
    expect(prompt.includes("recipe_stage_revisions")).toBe(viewRules);
    expect(prompt.includes("complete ordered chain")).toBe(viewRules);
    expect(prompt).not.toContain("Source discovery JSON");
    expect(prompt).not.toContain("file, command or HTTP source");
  });

  it("supplies complete typed timestamp evidence without requiring inspection", () => {
    const timestamp = "2026-09-06T12:34:56.123456789+02:00";
    const value = request({
      schemas: { s1: [{ name: "observed_at", dtype: "String" }] },
      rows: [{ source_id: "a", schema_ref: "s1", observed_at: timestamp }, { source_id: "a", schema_ref: "s1", observed_at: null }],
      coverage: { sampled: 2, available: 500, omitted_rows: 498 },
    });
    expect(requestSchema.safeParse(value).success).toBe(true);
    const prompt = proposalPrompt(value);
    expect(prompt).toContain(timestamp);
    expect(prompt).toContain('"observed_at":null');
    expect(prompt).toContain('"omitted_rows":498');
    expect(prompt).toContain("no file or tool call is needed");
    expect(prompt).toContain("must supply an explicit format");
    expect(prompt).toContain("format='%+'");
    expect(prompt).toContain("never substitute capture time");
    expect(prompt).not.toContain("not-in-prompt.parquet");
    expect(prompt).not.toContain("Read schemas across all parts");
    expect(prompt).toContain('"const":"frozen-3"');
    expect(prompt).toContain('"const":"view:7"');
  });

  // Sized off the constant, not off a number: the ceiling moved once already
  // when the wider sample tier landed.
  it.each([
    "界".repeat(MAX_INLINE_CONTEXT_BYTES / 3 + 1_000),
    "\u0000".repeat(MAX_INLINE_CONTEXT_BYTES / 6 + 1_000),
  ])("counts UTF-8 and JSON escaping in the context limit", (text) => {
    const value = request({ schemas: { s: [text] }, coverage: { sampled: 0 } });
    expect(Buffer.byteLength(JSON.stringify(value.context.inline_context), "utf8")).toBeGreaterThan(MAX_INLINE_CONTEXT_BYTES);
    expect(requestSchema.safeParse(value).success).toBe(false);
    expect(() => proposalPrompt(value)).toThrow(`${MAX_INLINE_CONTEXT_BYTES / 1024} KiB`);
  });

  it("counts wide schemas and provenance even when rows are empty", () => {
    const fields = Math.ceil(MAX_INLINE_CONTEXT_BYTES / 80) + 100;
    const schemas = Object.fromEntries(Array.from({ length: fields }, (_, i) => [`field_${i}`, { dtype: "String", provenance: "x".repeat(80) }]));
    expect(requestSchema.safeParse(request({ schemas, rows: [] })).success).toBe(false);
  });

  it("accepts the exact serialized limit without truncating it", () => {
    const overhead = Buffer.byteLength(JSON.stringify({ value: "" }), "utf8");
    const value = request({ value: "a".repeat(MAX_INLINE_CONTEXT_BYTES - overhead) });
    expect(requestSchema.safeParse(value).success).toBe(true);
    expect(proposalPrompt(value)).toContain(JSON.stringify(value.context.inline_context));
    value.context.inline_context = { value: "a".repeat(MAX_INLINE_CONTEXT_BYTES - overhead + 1) };
    expect(requestSchema.safeParse(value).success).toBe(false);
  });

  it("retains explicit omissions and makes further inspection optional", () => {
    const value = request({ coverage: { sources: [{ id: "a", sampled: 1 }, { id: "b", sampled: 0 }], omitted_bytes: 9000, reason: "context_byte_limit" } });
    value.context.inspection_command = ["/usr/bin/python", "/tmp/inspect context.py", "/tmp/context.json"];
    const prompt = proposalPrompt(value);
    expect(prompt).toContain('"omitted_bytes":9000');
    expect(prompt).toContain("Optional further bounded inspection");
    expect(prompt).toContain(JSON.stringify(value.context.inspection_command));
  });

  it("bounds the complete prompt after adding instructions and output schema", () => {
    const value = request({ coverage: { sampled: 0 } });
    value.instruction = "界".repeat(MAX_PROPOSAL_PROMPT_BYTES / 2);
    expect(() => proposalPrompt(value)).toThrow("128 KiB");
  });
});
