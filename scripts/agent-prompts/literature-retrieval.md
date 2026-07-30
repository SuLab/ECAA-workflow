## Literature retrieval — runbook (this task carries `retrieval_tools`)

This section is appended to your prompt only when this task's spec declares
a non-empty `attributes.retrieval_tools` (e.g. the `survey_method_landscape`
atom). It tells you how to gather source-anchored method evidence with the
bundled retrieval helper. The package `PROMPT.md` above and the per-task
`task-spec.json` remain authoritative for what this stage must produce; the
rules here are the cross-cutting retrieval contract.

**Use the bundled helper `lib/agent_literature_fetch.py` to write `method_landscape.csv`,
`retrieval_scope.json`, and `evidence/manifest.json` — do NOT hand-roll their
schemas.** The post-task literature validators enforce the exact column set and
manifest fields documented below; a hand-rolled CSV or a manifest in any other
shape (e.g. a top-level `sources` array, `artifact_path` keys, or PMID-only
entries) WILL be rejected and block the task. The helper emits the schema the
validators expect, including the per-source `sha256`, `license`, and
`redistributable` provenance fields.

**ALWAYS set `redistributable: true` for PubMed/PMC sources.** Every source you
retrieve here (PubMed abstracts + efetch/esearch XML are public-domain US-Gov
work; PMC OA is CC-licensed) IS redistributable — set `redistributable: true` on
its CSV row AND its manifest entry. The legal gate rejects any literature row
that is unmarked, so an omitted/false flag blocks the task. Only a locally-stored
external PDF (`source_kind: external_pdf_local_only`) is non-redistributable.

**Corroboration — call the helper ONCE PER CANDIDATE METHOD.** The
corroboration validator wants ≥2 distinct verified PMIDs grouped under the SAME
`candidate_method`. So iterate the axis's candidate methods (its task-spec
`attributes.candidate_tools`) and run the helper once per method, passing
`--candidate <method>` and a method-scoped query, e.g.

```
python3 lib/agent_literature_fetch.py <out_dir> <axis> \
  "<method> <analysis context>" primary_literature --candidate <method>
```

`--candidate` tags every PMID the query returns with that one method, so the
PMIDs accumulate under it (≥2 → corroborated). WITHOUT `--candidate` each paper
becomes its own single-PMID candidate and the survey fails
`insufficient_corroboration`. If only one source genuinely exists for a method,
retrieve it and proceed; do not fabricate a second — the validator de-ranks an
under-corroborated method rather than failing the axis, as long as some method
on the axis is adequately corroborated.

Your job on this task is to RETRIEVE and RECORD, not to rank, recommend,
paraphrase, or synthesize. Honour the atom's `claim_boundary`: every row you
write must be a verbatim quote from one resolvable source, tagged with its
locator, source class, and evidence role. Ranking is the downstream
`discover_*` task's deterministic job, not yours.

### What you produce

Write everything under `runtime/outputs/$ECAA_TASK_ID/`:

1. **`method_landscape.csv`** — one row per (axis, candidate method, source)
   with columns:
   `axis, candidate_method, source_ref_kind, source_ref, source_class,
   evidence_role, evidence_quote, evidence_quote_offset, source_kind,
   source_hash, retrieval_ts, redistributable, verified` (plus optional
   `version_context`). `source_hash` is `sha256:<hex>`. `verified` is a
   **quote-presence check only**: it is `true` only when `evidence_quote`
   substring-matches the stored snapshot (the full source text, e.g. the whole
   abstract) after the `collapse_whitespace_lowercase_v1` normalization
   (collapse runs of whitespace to one space, lowercase, trim). It confirms the
   quote was copied verbatim from the source; it does **NOT** assess whether the
   source supports a downstream claim or the directionality of any effect. Mark
   `verified=false` on any
   quote that does not match — never fabricate a match.
2. **`method_landscape.json`** — the same content as a JSON object keyed by
   axis, for the UI and the downstream loader.
3. **`retrieval_scope.json`** — the helper-maintained list of every query axis
   attempted, including axes that returned zero rows. Do not derive this list
   from `method_landscape.csv`: a zero-result search has no evidence row and
   would disappear.
4. **`evidence/manifest.json`** — the FOUNDATION evidence manifest
   (`{"schema_version": 2, "entries": [...]}`) with one entry per fetched
   source: `source_kind, path, sha256_binary, sha256_extracted_text,
   extracted_text_normalization, bytes, retrieval_ts, retrieval_query_id,
   redistributable, license`, and (for typed locators) `source_ref_kind,
   source_ref, source_class, evidence_role` plus `version_context` for tool
   docs. A batched PubMed efetch entry — one XML snapshot covering many PMIDs
   from a single efetch request — lists them under `pmids_in_batch: [...]` with
   `source_kind: pubmed_efetch_xml_batch` and `redistributable: true` (PubMed
   abstracts are public-domain US-Gov work); the validator resolves a claim row
   to its snapshot via any member of `pmids_in_batch`.
5. **`result.json`** — the usual task result (summary, artifacts, status).

### How to retrieve

Use the bundled helper rather than hand-rolling HTTP — it enforces the host
allowlist, snapshots each source by content hash, and writes the manifest +
CSV in the exact schema above:

```
python3 lib/agent_literature_fetch.py <out_dir> <axis> "<query>" [class ...]
```

where `<out_dir>` is `runtime/outputs/$ECAA_TASK_ID`, `<axis>` is the
runtime method-choice axis (e.g. `alignment`, `differential_expression`),
and the trailing `class` args are the source classes to query
(`primary_literature`, `conference_proceedings`, `tool_documentation`). Run
it once per axis. You can also `import sys; sys.path.insert(0, "lib")`
then `import agent_literature_fetch` and call `fetch_for_axis(...)` directly
when you need to pass explicit `routes` (e.g. specific tool-doc URLs).

Read the enabled source classes from `$ECAA_LIT_SOURCE_SCOPE` (and, when the
harness injects it, the method-source authority knob). When neither is set,
default to `primary_literature` only. Derive the per-axis query from the
analysis facts in your dependency outputs (organism, protocol, sample count,
goal/modality) — a context-scoped query, not a generic method name.

### Egress is bounded — do not fetch outside the allowlist

The atom declares an egress allowlist under `safety.network`; the helper
re-checks every target host against the per-class allowlist BEFORE any
request and raises on a host that is not listed. Do not bypass the helper to
fetch from an arbitrary host. If a class you need is not enabled by the
scope, leave it out — do not reach for an un-allowlisted source.

### When retrieval finds nothing — curated fallback (never block)

If a class is disabled, you are offline, every route fails, or an axis yields
no usable rows, that is fine and must NOT block the task. The helper handles
this for you: pass the axis's curated candidate pool (its task-spec
`attributes.candidate_tools`) to `fetch_for_axis(..., curated=[...])`. When
retrieval produces zero usable rows for the axis, the helper emits one
fallback row per curated candidate with `source_class=curated_baseline`,
`verified=false`, and no locator (empty `source_ref_kind`/`source_ref` and
empty `evidence_quote`). These rows let the downstream `discover_*` task still
offer the curated pool; the locator-resolution and corroboration validators
skip `curated_baseline` rows, so nothing fails. A transport/availability
failure inside the helper degrades to this fallback automatically — only a
route-allowlist misconfiguration surfaces as an error.

`fetch_for_axis` also (re)writes `method_landscape.json` from the full CSV on
every call — in both the normal and the fallback path — so the downstream UI
and loader always have a current per-axis candidate view. You do not need to
write that file by hand.
