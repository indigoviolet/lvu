import { describe, expect, it } from "vitest";
import { MAX_SOURCES_PER_PROPOSAL, parseJsonObject, parseProposal, proposalJsonSchema, requestSchema } from "../src/protocol.js";

const revision = { data: "data-4", definition: "definition-9" };
const sourceId = "11111111-1111-4111-8111-111111111111";
const sourceId2 = "44444444-4444-4444-8444-444444444444";
const viewId = "22222222-2222-4222-8222-222222222222";
const stageId = "33333333-3333-4333-8333-333333333333";
const commonSource = { schema_version: 1, id: sourceId, name: "app", identity_hints: {}, retention: null };
const fileSource = { ...commonSource, kind: "file", path: "/tmp/app.log", follow: true };
const secondFileSource = { ...commonSource, id: sourceId2, name: "second", kind: "file", path: "/tmp/other.log", follow: false };

describe("proposal validation", () => {
  it.each([
    ["source", { ...commonSource, kind: "file", path: "/tmp/app.log", follow: true }],
    ["source", { ...commonSource, kind: "command", command: { program: { exec: { executable: "/usr/bin/tail", args: ["-f", "/tmp/app.log"] } }, cwd: null, environment: {}, restart: "never" } }],
    ["sources", { schema_version: 1, sources: [fileSource] }],
    ["sources", { schema_version: 1, sources: [fileSource, secondFileSource] }],
    ["filter", { schema_version: 1, expression: "pl.col('status') >= 500" }],
    ["enrichment", { schema_version: 1, stages: [{ id: stageId, name: "parse", expressions: { request_id: "pl.col('raw').str.extract('(req-[0-9]+)')" } }] }],
    ["view", { schema_version: 1, id: viewId, name: "errors", source_ids: [sourceId], filter: null, recipe_stage_revisions: [] }],
  ] as const)("accepts a concrete %s definition", (kind, definition) => {
    expect(parseProposal({ kind, definition, explanation: "A bounded proposal", originating_revision: revision }, kind, revision)).toMatchObject({ kind, definition, originating_revision: revision });
  });

  it.each([
    ["source", { ...commonSource, kind: "file", path: "", follow: true }],
    ["source", { ...commonSource, kind: "command", command: { program: { shell: { text: "" } }, cwd: null, environment: {}, restart: "never" } }],
    ["sources", { schema_version: 1, sources: [] }],
    ["sources", { schema_version: 1, sources: [fileSource, { ...secondFileSource, id: sourceId }] }],
    ["sources", { schema_version: 1, sources: [{ ...commonSource, kind: "file", path: "", follow: true }] }],
    ["filter", { schema_version: 1, expression: "" }],
    ["enrichment", { schema_version: 1, stages: [{ id: stageId, name: "empty", expressions: {} }] }],
    ["view", { schema_version: 1, id: viewId, name: "empty", source_ids: [], filter: null, recipe_stage_revisions: [] }],
  ] as const)("rejects malformed %s definition", (kind, definition) => {
    expect(() => parseProposal({ kind, definition, explanation: "x", originating_revision: revision }, kind, revision)).toThrow();
  });

  it("bounds a multi-source batch at eight with distinct identities", () => {
    const batch = Array.from({ length: MAX_SOURCES_PER_PROPOSAL }, (_, index) => ({
      ...fileSource,
      id: `11111111-1111-4111-8111-${String(index).padStart(12, "0")}`,
      path: `/tmp/app-${index}.log`,
    }));
    const definition = { schema_version: 1, sources: batch };
    expect(parseProposal({ kind: "sources", definition, explanation: "full batch", originating_revision: revision }, "sources", revision).definition).toEqual(definition);
    const over = { schema_version: 1, sources: [...batch, { ...secondFileSource }] };
    expect(() => parseProposal({ kind: "sources", definition: over, explanation: "x", originating_revision: revision }, "sources", revision)).toThrow();
  });

  it("accepts a singular legacy source answering a plural request", () => {
    const proposal = parseProposal({ kind: "source", definition: fileSource, explanation: "one source", originating_revision: revision }, "sources", revision);
    expect(proposal.kind).toBe("sources");
    expect(proposal.definition).toEqual({ schema_version: 1, sources: [fileSource] });
    expect(() => parseProposal({ kind: "source", definition: fileSource, explanation: "stale", originating_revision: { ...revision, data: "other" } }, "sources", revision)).toThrow("proposal revision does not match the requested revision");
  });

  it("generates the SDK schema from the same concrete validator", () => {
    expect(proposalJsonSchema("source")).toMatchObject({ properties: { definition: { oneOf: expect.any(Array) } } });
  });

  it.each(["source", "sources", "filter", "enrichment", "view"] as const)("binds %s proposal schemas to the expected revision", (kind) => {
    expect(proposalJsonSchema(kind, revision)).toMatchObject({
      properties: {
        originating_revision: {
          properties: {
            data: { const: revision.data },
            definition: { const: revision.definition },
          },
        },
      },
    });
  });

  it("keeps generic proposal schemas compatible when no revision is supplied", () => {
    expect(proposalJsonSchema("filter")).toMatchObject({
      properties: {
        originating_revision: {
          properties: {
            data: { type: "string" },
            definition: { type: "string" },
          },
        },
      },
    });
  });

  it("retains runtime rejection of a mismatched proposal revision", () => {
    expect(() => parseProposal({
      kind: "filter",
      definition: { schema_version: 1, expression: "pl.col('status') >= 500" },
      explanation: "stale",
      originating_revision: { ...revision, definition: "definition-8" },
    }, "filter", revision)).toThrow("proposal revision does not match the requested revision");
  });

  it("rejects inline bulk data in inspection context", () => {
    expect(requestSchema.safeParse({ schema_version: 1, request_id: "x", method: "request_proposal", session_id: "s", kind: "filter", instruction: "x", originating_revision: revision, context: { manifest_path: "/tmp/manifest.json", dataset_paths: [], rows: [{ raw: "secret" }] } }).success).toBe(false);
  });
});

it("accepts only the bounded assistance lifecycle purposes", () => {
  const start = { schema_version: 1, request_id: "start", method: "start_session", provider: "fake", cwd: "/tmp" };
  expect(requestSchema.safeParse(start).success).toBe(true);
  for (const purpose of ["ask", "source_assistance", "investigation"]) expect(requestSchema.safeParse({ ...start, purpose }).success).toBe(true);
  expect(requestSchema.safeParse({ ...start, purpose: "cleanup" }).success).toBe(false);
  expect(requestSchema.safeParse({ schema_version: 1, request_id: "resume", method: "resume_session", session_id: "agent", purpose: "investigation" }).success).toBe(true);
});


it("accepts only a single presentation separator before one JSON object", () => {
  expect(parseJsonObject('---\n\n{"kind":"filter"}', 1024)).toEqual({kind: "filter"});
  for (const value of ['preamble\n{"kind":"filter"}', '---\nprose\n{"kind":"filter"}', '---\n{}\n{}', '---\n{} trailing', '---\n---\n{}']) {
    expect(() => parseJsonObject(value, 1024)).toThrow();
  }
  expect(() => parseJsonObject('---\n{}', 4)).toThrow();
});

it("validates bounded inline ordered enrichment definitions in view proposals", () => {
  const definition = {schema_version: 1, id: viewId, name: "adapted", source_ids: [sourceId], filter: null, recipe_stage_revisions: [], enrichments: [{id: "existing-stage", source: "/(?P<code>[0-9]+)/"}]};
  const proposal = {kind: "view", definition, explanation: "adapt", originating_revision: revision};
  expect(parseProposal(proposal, "view", revision).definition).toEqual(definition);
  for (const enrichments of [[{id: "", source: "x"}], [{id: "a", source: "x".repeat(16_385)}], Array.from({length:33}, (_, i) => ({id:String(i), source:"x"})), [{id:"a", source:"x", command:"bad"}]]) {
    expect(() => parseProposal({...proposal, definition:{...definition, enrichments}}, "view", revision)).toThrow();
  }
});
