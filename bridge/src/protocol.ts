import { z } from "zod";

export const SCHEMA_VERSION = 1 as const;
export const proposalKinds = ["source", "sources", "filter", "enrichment", "view"] as const;
export type ProposalKind = typeof proposalKinds[number];
export interface ProposalRevision { data: string; definition: string; }
// The wider sample tier (docs/larger-ask-sample.md) sends up to 96 KiB inline.
// The whole prompt still has to fit MAX_PROPOSAL_PROMPT_BYTES below, which is
// what actually bounds the request; this is the per-context ceiling.
export const MAX_INLINE_CONTEXT_BYTES = 96 * 1024;
export const MAX_PROPOSAL_PROMPT_BYTES = 128 * 1024;

const boundedId = z.string().uuid();
const boundedText = (max: number) => z.string().min(1).max(max);
const path = boundedText(4096);
const revisionSchema = z.object({ data: boundedText(256), definition: boundedText(256) }).strict();
const contextSchema = z.object({
  manifest_path: path,
  dataset_paths: z.array(path).max(64),
  inline_context: z.record(z.string(), z.unknown()).refine(
    (value) => Buffer.byteLength(JSON.stringify(value), "utf8") <= MAX_INLINE_CONTEXT_BYTES,
    `serialized assistance context exceeds ${MAX_INLINE_CONTEXT_BYTES / 1024} KiB`,
  ).optional(),
  inspection_command: z.array(path).min(1).max(16).optional(),
}).strict();
const shellProgram = z.object({ shell: z.object({ text: boundedText(131_072) }).strict() }).strict();
const execProgram = z.object({ exec: z.object({ executable: path, args: z.array(z.string().max(8192)).max(256) }).strict() }).strict();
const command = z.object({
  program: z.union([shellProgram, execProgram]), cwd: path.nullable(),
  environment: z.record(z.string().min(1).max(256), z.string().max(32_768)).refine((value) => Object.keys(value).length <= 128, "too many environment entries"),
  restart: z.enum(["never", "on_failure", "always"]),
}).strict();
const sourceCommon = {
  schema_version: z.literal(1), id: boundedId, name: boundedText(256),
  identity_hints: z.record(z.string().max(256), z.string().max(4096)).refine((value) => Object.keys(value).length <= 64),
  retention: z.object({ maximum_bytes: z.number().int().nonnegative().nullable(), maximum_age_seconds: z.number().int().nonnegative().nullable() }).strict().nullable(),
};
export const sourceDefinitionSchema = z.discriminatedUnion("kind", [
  z.object({ ...sourceCommon, kind: z.literal("file"), path, follow: z.boolean() }).strict(),
  z.object({ ...sourceCommon, kind: z.literal("command"), command }).strict(),
  z.object({ ...sourceCommon, kind: z.literal("http"), url: boundedText(8192), framing: z.enum(["newline", "sse"]), reconnect: z.object({ enabled: z.boolean(), delay: z.number().int().nonnegative().max(86_400_000) }).strict() }).strict(),
]);
// One reviewed Apply must fit the app's pending-start admission (8), so a
// multi-source proposal is bounded there rather than at the session limit.
export const MAX_SOURCES_PER_PROPOSAL = 8;
export const sourcesDefinitionSchema = z.object({
  schema_version: z.literal(1),
  sources: z.array(sourceDefinitionSchema).min(1).max(MAX_SOURCES_PER_PROPOSAL).refine(
    (sources) => new Set(sources.map((source) => source.id)).size === sources.length,
    "duplicate source ids",
  ),
}).strict();
export const filterDefinitionSchema = z.object({ schema_version: z.literal(1), expression: boundedText(131_072) }).strict();
const stageSchema = z.object({
  id: boundedId, name: boundedText(256),
  expressions: z.record(z.string().min(1).max(256), boundedText(131_072).describe("One Python expression returning pl.Expr, not assignments or statements")).refine((value) => Object.keys(value).length > 0 && Object.keys(value).length <= 128, "expressions must contain 1..128 fields"),
}).strict();
export const enrichmentDefinitionSchema = z.object({ schema_version: z.literal(1), stages: z.array(stageSchema).min(1).max(64) }).strict();
export const viewDefinitionSchema = z.object({
  schema_version: z.literal(1), id: boundedId, name: boundedText(256), source_ids: z.array(boundedId).min(1).max(64),
  filter: filterDefinitionSchema.nullable(), recipe_stage_revisions: z.array(boundedText(256)).max(256),
  enrichments: z.array(z.object({ id: boundedText(128), source: boundedText(16_384) }).strict()).max(32).optional(),
}).strict();

const definitions = { source: sourceDefinitionSchema, sources: sourcesDefinitionSchema, filter: filterDefinitionSchema, enrichment: enrichmentDefinitionSchema, view: viewDefinitionSchema } as const;
const base = z.object({ schema_version: z.literal(1), request_id: boundedText(128) });
const sessionConfig = { provider: boundedText(256), cwd: path, mode_id: boundedText(128).optional(), thinking_option_id: boundedText(128).optional(), title: z.string().max(256).optional() };
export const requestSchema = z.discriminatedUnion("method", [
  base.extend({ method: z.literal("capabilities") }).strict(),
  base.extend({ method: z.literal("start_session"), ...sessionConfig, purpose: z.enum(["ask", "source_assistance", "investigation"]).optional(), prompt: z.string().max(131_072).optional(), timeout_ms: z.number().int().min(1).max(600_000).optional() }).strict(),
  base.extend({ method: z.literal("send_prompt"), session_id: boundedText(256), prompt: boundedText(131_072), timeout_ms: z.number().int().min(1).max(600_000).optional() }).strict(),
  base.extend({ method: z.literal("cancel"), session_id: boundedText(256) }).strict(),
  base.extend({ method: z.literal("resume_session"), session_id: boundedText(256), purpose: z.enum(["ask", "source_assistance", "investigation"]).optional() }).strict(),
  base.extend({ method: z.literal("request_proposal"), session_id: boundedText(256), kind: z.enum(proposalKinds), instruction: boundedText(131_072), originating_revision: revisionSchema, context: contextSchema, timeout_ms: z.number().int().min(1).max(600_000).optional() }).strict(),
]);
export type BridgeRequest = z.infer<typeof requestSchema>;
export interface Proposal { kind: ProposalKind; definition: Record<string, unknown>; explanation: string; originating_revision: { data: string; definition: string }; needs_more_data?: boolean; }

export function proposalSchema(kind: ProposalKind, expectedRevision?: ProposalRevision) {
  const originatingRevision = expectedRevision === undefined
    ? revisionSchema
    : z.object({ data: z.literal(expectedRevision.data), definition: z.literal(expectedRevision.definition) }).strict();
  // `needs_more_data` is the agent's own report that the bounded sample it was
  // given was not enough to answer with. Optional: older agents omit it.
  return z.object({ kind: z.literal(kind), definition: definitions[kind], explanation: boundedText(16_384), originating_revision: originatingRevision, needs_more_data: z.boolean().optional() }).strict();
}
export function proposalJsonSchema(kind: ProposalKind, expectedRevision?: ProposalRevision): Record<string, unknown> { return z.toJSONSchema(proposalSchema(kind, expectedRevision), { target: "draft-7" }) as Record<string, unknown>; }
export function parseProposal(value: unknown, kind: ProposalKind, revision: ProposalRevision): Proposal {
  if (kind === "sources") {
    const plural = proposalSchema("sources").safeParse(value);
    if (plural.success) {
      const proposal = plural.data;
      if (proposal.originating_revision.data !== revision.data || proposal.originating_revision.definition !== revision.definition) throw new Error("proposal revision does not match the requested revision");
      return proposal as Proposal;
    }
    // A singular legacy agent answers a plural request with one source. Accept
    // it as a single-element batch rather than failing the whole request; the
    // prompt asks for the plural kind, so this is tolerance, not the contract.
    const single = proposalSchema("source").safeParse(value);
    if (single.success) {
      const proposal = single.data;
      if (proposal.originating_revision.data !== revision.data || proposal.originating_revision.definition !== revision.definition) throw new Error("proposal revision does not match the requested revision");
      return {
        kind: "sources",
        definition: { schema_version: 1 as const, sources: [proposal.definition] },
        explanation: proposal.explanation,
        originating_revision: proposal.originating_revision,
      } as Proposal;
    }
    throw new Error(JSON.stringify(plural.error.issues));
  }
  const proposal = proposalSchema(kind).parse(value);
  if (proposal.originating_revision.data !== revision.data || proposal.originating_revision.definition !== revision.definition) throw new Error("proposal revision does not match the requested revision");
  return proposal as Proposal;
}
export function parseJsonObject(text: string, maxBytes: number): unknown {
  if (Buffer.byteLength(text, "utf8") > maxBytes) throw new Error("proposal output exceeds byte limit");
  // Some local agent runtimes prepend a presentation separator even for
  // structured replies. Accept that exact wrapper, never arbitrary prose or
  // an object extracted from a larger response. Size and schema checks remain.
  const trimmed = text.trim().replace(/^---\r?\n[\t \r\n]*/, "");
  if (!trimmed.startsWith("{") || !trimmed.endsWith("}")) throw new Error("proposal output must be a single JSON object");
  return JSON.parse(trimmed) as unknown;
}
