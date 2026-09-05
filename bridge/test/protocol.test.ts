import { describe, expect, it } from "vitest";
import { parseProposal, proposalJsonSchema, requestSchema } from "../src/protocol.js";

const revision = { data: "data-4", definition: "definition-9" };
const sourceId = "11111111-1111-4111-8111-111111111111";
const viewId = "22222222-2222-4222-8222-222222222222";
const stageId = "33333333-3333-4333-8333-333333333333";
const commonSource = { schema_version: 1, id: sourceId, name: "app", identity_hints: {}, retention: null };

describe("proposal validation", () => {
  it.each([
    ["source", { ...commonSource, kind: "file", path: "/tmp/app.log", follow: true }],
    ["source", { ...commonSource, kind: "command", command: { program: { exec: { executable: "/usr/bin/tail", args: ["-f", "/tmp/app.log"] } }, cwd: null, environment: {}, restart: "never" } }],
    ["filter", { schema_version: 1, expression: "pl.col('status') >= 500" }],
    ["enrichment", { schema_version: 1, stages: [{ id: stageId, name: "parse", expressions: { request_id: "pl.col('raw').str.extract('(req-[0-9]+)')" } }] }],
    ["view", { schema_version: 1, id: viewId, name: "errors", source_ids: [sourceId], filter: null, recipe_stage_revisions: [] }],
  ] as const)("accepts a concrete %s definition", (kind, definition) => {
    expect(parseProposal({ kind, definition, explanation: "A bounded proposal", originating_revision: revision }, kind, revision)).toMatchObject({ kind, definition, originating_revision: revision });
  });

  it.each([
    ["source", { ...commonSource, kind: "file", path: "", follow: true }],
    ["source", { ...commonSource, kind: "command", command: { program: { shell: { text: "" } }, cwd: null, environment: {}, restart: "never" } }],
    ["filter", { schema_version: 1, expression: "" }],
    ["enrichment", { schema_version: 1, stages: [{ id: stageId, name: "empty", expressions: {} }] }],
    ["view", { schema_version: 1, id: viewId, name: "empty", source_ids: [], filter: null, recipe_stage_revisions: [] }],
  ] as const)("rejects malformed %s definition", (kind, definition) => {
    expect(() => parseProposal({ kind, definition, explanation: "x", originating_revision: revision }, kind, revision)).toThrow();
  });

  it("generates the SDK schema from the same concrete validator", () => {
    expect(proposalJsonSchema("source")).toMatchObject({ properties: { definition: { oneOf: expect.any(Array) } } });
  });

  it("rejects inline bulk data in inspection context", () => {
    expect(requestSchema.safeParse({ schema_version: 1, request_id: "x", method: "request_proposal", session_id: "s", kind: "filter", instruction: "x", originating_revision: revision, context: { manifest_path: "/tmp/manifest.json", dataset_paths: [], rows: [{ raw: "secret" }] } }).success).toBe(false);
  });
});
