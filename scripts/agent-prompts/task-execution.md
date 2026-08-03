## Task execution — shared contract

This section is appended to the package `PROMPT.md` for every dispatch on
every backend (local / AWS / SLURM) so the per-task execution contract
cannot drift between executors. The package `PROMPT.md` above is
authoritative for this workflow's stages, policies, and discovery scoring;
the rules below restate the cross-cutting per-task obligations and how to
pace your work.

You are executing exactly one task — the one named by `$ECAA_TASK_ID` — in
the RO-Crate package at `$PACKAGE`. Do that one task, write your outputs,
record the state transition, and exit. The harness invokes a fresh agent
for the next ready task; never start another task yourself.

`$PACKAGE` (alias `$PKG_ROOT`) is an environment variable the harness sets
to the absolute package root; it is valid both at run time and during
replay. When any script you write needs an absolute path, build it from
`$PACKAGE` — e.g. `"$PACKAGE/inputs/<file>"`,
`"$PACKAGE/runtime/outputs/$ECAA_TASK_ID"`. Never recompute the package root
by counting `dirname` levels up from `$0` / `$BASH_SOURCE` / `__file__`:
that is off-by-one-prone (a wrong count silently reads from
`runtime/inputs/` and writes to `runtime/runtime/outputs/`) and resolves to
the wrong root during replay, where your script runs from a staged copy at a
different depth. Equivalently you may use paths relative to the working
directory, which the harness sets to `$PACKAGE`.

### Runtime-derived metadata

When executable code selects a method at run time, including an availability
fallback, compute that choice once and use the same variable for the analysis
call and every metadata artifact the script writes. For example, if an R
script sets `shrink_type` to `ashr` or `normal`, both `lfcShrink(type=...)` and
`de_summary.json::lfc_shrinkage` must read `shrink_type`; never hard-code the
preferred branch in the summary. Apply the same rule to engines, reference
databases, normalization methods, and other fallbacks. Do not repair a
contradictory summary by editing JSON after the script finishes: replay must
regenerate the same truthful metadata directly from the retained executable
code.

The same rule applies to parameters: emit a parameter only when the selected
implementation actually consumed it, and populate metadata from the variable
passed to that call. Do not declare a permutation count, cutoff, seed, or size
bound that exists only in the script's summary block.

Treat every software version as a quantitative provenance claim. Copy it only
from retained runtime evidence produced by this run: the executed stage's
`result.json`, `env.lock`, `env.explicit.lock`, `language_packages_installed`,
or the package-level install/dependency log. Never supply a version from model
memory or from the version you expected to install. If no retained source
records a package's resolved version, name the package without a version and
state that the resolved version was not retained.

For a `validate_*` task, compare the target's retained scripts and run logs
with its result and summary metadata. A script that can execute one branch but
records another is a validation failure even when the current result JSON was
post-edited to the value observed in the log.

Also apply every source-owned assertion for the target stage from
`policies/validation-contract.json` exactly as written. For a
`cross_stage_table_handoff` assertion, inspect the target stage's
`reads.jsonl`: the recorded path must be the exact declared upstream artifact,
the target and upstream row populations must agree, and a one-of handoff with
`alternative_ports` must select exactly one member. Zero selected members or
simultaneous reads through the declared port and any alternative port are
validation failures. A matching row count does not excuse a wrong path or a
multi-port read.

### Turn budget

You have a budget of {{MAX_TURNS_PER_TASK}} turns per task. Spend them on
productive work, not on re-reading files you already have in context or
re-deriving things you already computed.

Aim to land the task — outputs written, `result.json` and
`state.patch.json` in place — before going past
{{SOFT_TURNS_PER_TASK}} turns. Treat the gap between the soft target and
the hard cap as reserve for genuinely unforeseen work (a slow install, an
unexpected data shape), not as headroom to use by default. If you can see
you will not finish within the hard cap, stop burning turns: write a
partial `result.json` with `status: "blocked"` and a typed
`blocker_kind` of `TurnBudgetExceeded` describing exactly what remains and
what would let a fresh dispatch finish it. A precise blocker is worth more
than ten more turns of thrashing.

Keep token use lean: read only the task spec and the completed-dependency
outputs you actually need, prefer in-image tools over installing new ones
when scores are close, and keep your final narrative under ~500 words.

Exception: if your task spec carries `interpretation_exempt_from_word_budget:
true` (the `biological_interpretation`, `final_reporting`, and `reporting`
stages), the ~500 words cap does NOT apply. Those stages produce
findings-first prose whose length scales with the number of result rows you
are grounding — write as much as you need so that every claim cites its
result-table row or PMID. Do not pad, but do not truncate citations to hit a
word count. See "Report completeness contract" below for what
"as much as you need" requires for these stages specifically.

### What to write (and only this)

Write everything under `runtime/outputs/$ECAA_TASK_ID/`. Do not touch
`WORKFLOW.json` or any other task's directory — the harness is the sole
writer of task state.

1. **`state.patch.json`** — the single authoritative state transition. A
   patch-merge envelope whose `to` field is a fully-tagged `TaskState`
   object. The terminal `to` shapes the harness accepts are:

   ```json
   { "from": "running",
     "to": { "status": "completed", "result": { "summary": "…", "artifacts": ["cohort_manifest.tsv", "result.json"], "figures": ["figures/samples_per_study.png"] } },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>",
     "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```
   ```json
   { "from": "running",
     "to": { "status": "blocked", "record": { "reason": "<what is missing / what the SME must decide>", "attempts": [] } },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>", "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```
   ```json
   { "from": "running",
     "to": { "status": "failed", "reason": "<why the task could not complete>" },
     "harness_run_id": "<ECAA_HARNESS_RUN_ID>", "dispatch_epoch": <ECAA_DISPATCH_EPOCH> }
   ```

   The `to` object is REQUIRED and must carry the nested field for its
   status: `completed` requires `result` (any JSON — put your task summary,
   artifact list, and figure paths here), `blocked` requires `record`
   (with a `reason`), `failed` requires `reason`. A bare
   `{"status": "completed"}` is rejected as an unparseable patch. Copy
   `harness_run_id` and `dispatch_epoch` verbatim from the
   `ECAA_HARNESS_RUN_ID` and `ECAA_DISPATCH_EPOCH` environment values so
   the harness can reject a stale patch from a superseded dispatch.

2. **`result.json`** — the task result: `task_id`, `status`, a short
   narrative, the artifacts you produced, and (for analytical stages) the
   figures you rendered. When the task description or claim boundary says
   `no narrative`, `no synthesis`, or row-level claims only, OMIT free-text
   `narrative` / `narrative_text` / `summary` / `interpretation` fields and retain only
   the declared structured counts, rows, and provenance; that task-specific boundary wins
   over this general envelope convention. On a blocked exit include
   `blocker_kind` and a `what_would_unblock` note.

3. **`progress.log`** — append a human-readable line at each meaningful
   step. The harness reads recent activity here as a liveness signal; a
   long-running step with no progress line can look like a stall.

4. **`runtime/LOG.jsonl`** — append one JSON object per line for audit
   context (decisions, tool invocations, observations). Each entry's `ts`
   field must be the real wall-clock time in ISO-8601 UTC (the value of
   `date -u +%Y-%m-%dT%H:%M:%SZ` at the moment you write the line) — never a
   placeholder such as `2026-01-01T00:00:00Z`, which makes the audit trail's
   timestamps untrustworthy. Never write to `runtime/decisions.jsonl` — that
   file is owned by the conversation/server layer and carries only the typed
   `DecisionRecord` taxonomy.

5. **`reads.jsonl`** — a read manifest: one JSON object per line, one per
   INPUT file you actually read that another stage produced (an upstream
   producer's output under `runtime/outputs/<producer>/…`). Each object is
   exactly `{"path": "<package-root-relative path you read>",
   "declared_port": "<the input port from your task spec this file
   satisfied>"}`. Use the package-root-relative path, not an absolute one
   (e.g. `runtime/outputs/quantification/count_matrix.tsv`), and set
   `declared_port` to the input-port name your `task-spec.json` names for that
   input; omit `declared_port` only if the spec names no port for it. Record
   ONLY genuine cross-stage input reads — do not list your own
   `runtime/outputs/$ECAA_TASK_ID/` outputs or scratch, config files, or
   system paths (those are ignored anyway). When your task spec offers a
   mutually-exclusive one-of input group — e.g. a differential-expression
   stage that may read EITHER a raw count matrix OR a normalized count matrix
   — this manifest is how the run records WHICH one you actually consumed, so
   be precise: list the file you read and not the alternative. Write one line
   per file, in a stable order, with no timestamps. If the task read no
   cross-stage input files, omit the manifest (an absent file is fine).

### Canonical upstream handoffs

Treat the artifact bound to a declared input port by a direct dependency as
the canonical handoff for that port. Read that artifact itself. Do not reopen
an equivalent-looking file from an ancestor stage, and do not recreate the
handoff by applying a new filter or transformation to an ancestor file. A
filtered raw-count matrix remains raw counts, so a count-based model must use
the direct QC artifact bound to `raw_counts` when one is declared. Record the
exact artifact and port in `reads.jsonl`.

Before opening any cross-stage artifact, inspect `runtime/proofs.jsonl` for the
edge whose `to_node` is `$ECAA_TASK_ID` and whose `from_node` produced the
artifact. The `declared_port` written to `reads.jsonl` must equal that edge's
exact `to_port`; a semantically appealing name from prose is not a substitute.
Never invent `companion_in_*`, `residual_in_*`, or another port label. If no
`typed_data_flow` or `adapter_mediated` edge authorizes the read, and the task
spec provides no explicit read allowance for it, do not read the artifact.
Record a precise blocker if the task cannot complete without that undeclared
input.

An input named in `task-spec.json` but absent from the task's typed incoming
edges is not permission to assign an arbitrary dependency artifact to that
port. Runtime context such as `intake_facts` may already be present in the task
specification or `ECAA_HW_INTAKE_FACTS`; use that retained context directly.
Only record a cross-stage file under a port when `runtime/proofs.jsonl`
contains the producer, consumer, and exact port binding.

### Tabular file format

A table's filename extension MUST match its actual delimiter: `.tsv` is
tab-separated, `.csv` is comma-separated. Nothing else is acceptable — a
comma-delimited file named `.tsv` is a defect even when its contents are
otherwise correct, because every consumer that keys off the extension
(re-execution comparison, deposit verification, downstream joins, a human
opening it) then reads the wrong thing.

- **R** — for a `.tsv` write `write.table(x, file, sep = "\t", quote = FALSE,
  row.names = FALSE)`. `write.csv()` emits COMMAS; use it only for a `.csv`
  name. This is the most common source of the mismatch: `write.csv(x,
  "foo.tsv")` produces a comma file with a tab-file name.
- **Python** — `df.to_csv(path, sep = "\t", index = False)` for a `.tsv`,
  `df.to_csv(path, index = False)` for a `.csv`.

Write the header row with the same delimiter as the data rows, and do not
quote fields unless a value genuinely contains the delimiter.

### Identifiers

Never hardcode gene→Ensembl IDs; resolve via the pinned annotation or the
upstream table; on lookup failure mark the gene unresolved, never guess. Do not
embed a literal symbol→Ensembl map in your script and do not recall an Ensembl ID
from memory — resolve every gene symbol through the run's pinned genome
annotation (org.Hs.eg.db / biomaRt at the package's Ensembl release) or by
joining on the Ensembl IDs already present in the upstream differential-expression
table (de_results.tsv gene_id column). If a lookup fails, mark that gene
unresolved rather than falling back to a literal ID.

### Figures obligation

Do not author figure-rendering code. Run your compute in whichever language
fits the analysis — Python or R, your choice — because the compute language
has no bearing on figures. Do not import `matplotlib`, `ggplot2`, or any
plotting toolkit to draw figures, and do not write figures yourself.

Instead, emit the standardized figure-data-contract output **tables** for the
stage under `runtime/outputs/$ECAA_TASK_ID/`. A fixed, non-LLM Python render
step then turns those tables into the declared figures for you:

```
python3 -m runtime.plotting render --stage <STAGE> \
  --outputs runtime/outputs/$ECAA_TASK_ID --required <required_figures>
```

So your obligation is the tables, not the images. If the task spec's
`required_figures` is empty or absent, the render step is a no-op and there is
nothing to do. Otherwise, every required figure has source tables it is
rendered from: produce those tables from this task's real results — do not
stub, fabricate, or copy placeholder data. Missing a required output table is
a hard completion failure: an analytical stage that did not emit the tables
its declared figures are rendered from is not done.

### Narrative correctness (report-generation stages)

If your task writes report narrative prose — `reporting`, `final_reporting`,
a pathway-enrichment summary, or any stage that produces a `narrative_text`
field or a markdown report body — the following mistakes have shipped in
real runs and are cheap to avoid:

- **`final_reporting` preserves the validated report; it does not rewrite
  it.** Read `runtime/outputs/reporting/report.md` as bytes and place those
  bytes unchanged as one contiguous block in `final_report.md`. You may put
  project navigation or dashboard material before or after that block, but
  those additions must not make scientific claims. Do not summarize, edit,
  reorder, reformat, or regenerate any sentence or table in the copied block.
  A source-owned validator rejects a final report that does not contain the
  complete upstream report verbatim.
- **Direction words come from the sign of the statistic, never free text.**
  When you write "above"/"below", "higher"/"lower", "increased"/"decreased",
  or similar, derive the word from the actual sign or ratio you computed —
  do not describe direction from intuition or from what you expect the
  result to look like. If a metric basis declares `neutral_reference`, compare
  against that retained value rather than assuming one: 0.56 is below a
  reference of 1 but above a reference of 0.5. In the same sentence that names
  the metric field, state whether its retained value is above, below, or equal
  to the retained reference. A definition without that direction does not
  complete the interpretation.
- **Report what was tested, not what was merely loaded.** Any "N analyzed"
  figure must be the post-filter population actually supplied to the executed
  method, never the source inventory before eligibility, size, quality, or
  availability filters. If both populations are retained, label both and bind
  every `*_tested` field to the executed result population so a validator can
  check it against the result artifact.
- **Distinguish a source population from the population retained for
  analysis.** When preprocessing records different pre-filter and retained
  dimensions, name the former as source or original and the latter as filtered,
  analysis-ready, or tested. A retained population may be called an input only
  when the sentence names the downstream stage that received it. Never use the
  same unqualified population label for two different dimensions.
- **A "Top" result table is always the canonical ordered prefix.** Read the
  relevant artifact's `ranking` object from `report-data.json` and copy its
  first N rows in order. When no N is written, N is the number of displayed
  data rows. Do not choose illustrative rows, sample one row per group, or
  replace a finite source value with `—`, `NA`, or a blank. Preserve every
  displayed source value at the retained precision.
- **Keep every population transition in its own reconciliation bucket.**
  Identifier mapping, eligibility filtering, deduplication, aggregation, and
  ranking can each change a population. Retain the exact stage-declared
  handoff artifact and its row count. Record each transition under the fields
  declared by that stage, preserve its conservation identity, and never label
  a combined loss as if one transition caused all of it.
- **Keep grouping labels identical across retained outputs.** When a result
  schema declares a grouping column, summary arrays and narrative labels must
  use the exact distinct values retained in that column. Provider, database,
  or source labels belong in separate provenance fields and must not replace a
  result-group value.
- **Every threshold mention must identify its scope, field, comparator, and
  cutoff.** Two stages may use fields with the same display name over different
  populations or may use different cutoffs and comparator directions. Copy all
  four parts from that artifact's `result_schema` object in
  `report-data.json`. Its comparator is exact: `lt` means `<`, never `≤`, and
  `gt` means `>`, never `≥`. If a sentence names the implementation, copy that
  name from the stage's retained method field.
- **Describe generated significant-result attachments by their actual
  filter.** A `<artifact>.significant.tsv` written by report-data assembly is
  filtered only by the atom's declared significance rule. Copy that rule from
  `report-data.json` / the result schema. Do not append a stage-internal
  effect-size threshold unless that threshold actually selected the rows in
  this attachment; inspect the attachment row count when in doubt.
- **Quote p-values precisely enough to match the source table.** When you
  state a SPECIFIC p-value / padj / FDR in prose, copy it to at least TWO
  significant figures directly from the results table (e.g. "padj = 6.5e-05",
  "padj = 1.7e-04") — never round a p-value to a SINGLE significant figure
  (writing "1e-04" for a table 6.5e-05, or "2e-04" for 1.7e-04). A
  source-level validator compares every quoted p-value against the table
  within a tight relative tolerance and flags a coarse one-sig-fig round as a
  mismatch. If you only mean to convey the magnitude, write an inequality
  ("padj < 1e-04") instead of a rounded point value — an inequality carries no
  false precision to check.
- **Keep every quantitative prose sentence atomic.** A sentence that states an
  entity-specific effect or significance value must quantify exactly one
  entity, with that entity's own values. Put a second quantified entity in a
  separate sentence or in its own table row. Never place one entity's
  significance beside another entity's effect. Likewise, a prose sentence that
  states a retained count must assert exactly one named field and population;
  put separate lifecycle populations in separate sentences.
  This is part of the machine-verification contract, not a style preference.
- **Name the statistical model exactly as executed.** A fixed-effects
  design (e.g. a DESeq2/edgeR `~ covariate + condition` negative-binomial
  GLM, with no random-effect term) is NOT a "linear mixed model" — a mixed
  model requires an actual random-effects term. Copy the model label from
  what you ran; don't reach for a more familiar-sounding name.
- **Carry pathway DIRECTION into the overlap figure data.** When the
  `reporting` stage emits its `pathway_overlap` figure-data table (in
  `manifest.json`), give each entry a signed direction alongside its
  `count`: a numeric `nes` (normalized enrichment score — positive =
  enriched, negative = depleted) copied from the enrichment result, or,
  when you have no NES, a string `direction` of `"up"`/`"down"` (equivalently
  `"enriched"`/`"depleted"`). The renderer draws enriched and depleted
  pathways in distinct diverging colors only when this field is present;
  without it an enriched and a depleted set render as visually identical
  bars. This is purely additive — a legacy `[{label, count}]` entry still
  renders — so always include `nes` (or `direction`) when the sign is known.
- **Emit `top_gene_down` in the DE summary.** When the
  `differential_expression` stage writes `de_summary.json`, record BOTH the
  top up-regulated and top down-regulated gene: a `top_gene_up` (largest
  positive log2FC among significant genes) AND a `top_gene_down` (most
  negative log2FC among significant genes), each taken from your own
  `de_results.tsv` — do not report only the up direction. Report the
  gene identifier exactly as it appears in the results table (resolve to a
  symbol only via the run's pinned annotation, per the Identifiers section).

- **The data-provenance section is SYSTEM-GENERATED — do not assert where the
  data came from.** A "Data provenance" block delimited by
  `<!-- ECAA:data-provenance START -->` / `<!-- ECAA:data-provenance END -->`
  is rendered deterministically from the package's own acquisition metadata
  (`runtime/outputs/<stage>/per_accession_summary.json`, that stage's cohort
  manifest and `result.json` deviation note, and `runtime/inputs.json`) and
  appended to your report. Never hand-write that block, and never state the
  data source, the input path, the originating repository/software package,
  the journal, DOI, PMID, or "supplied by the SME"/"local copy" phrasing in
  your own prose — write "see the Data provenance section" instead. Real runs
  have shipped a report claiming an SME-supplied local copy that was never
  registered (`runtime/inputs.json` did not exist; the stage actually read a
  software data package) and citing the study to the wrong journal, while the
  package's own record carried the correct source, journal, DOI and PMID. A
  source-level validator compares every bibliographic and data-source
  assertion in the report against that record and BLOCKS the deposit on a
  contradiction. If the acquisition stage recorded a substitution (the
  requested source was unavailable and something else was used), that
  substitution is already in the generated block — do not restate, soften, or
  contradict it.
- **Never assert a QC conclusion you did not compute and retain an artifact
  for.** Do not write "no outlier samples were identified", "the cohort was
  outlier-free", "no samples were flagged", or any equivalent QC-negative
  statement unless this run actually performed that check AND left the
  evidence in the package — an outlier table or recorded outlier verdict, a
  PCA/MDS plot or score table, a sample-distance or sample-correlation matrix,
  a Cook's-distance output. A range of size factors, library sizes, or any
  other single summary statistic is NOT an outlier assessment. If you did not
  run the check, say so plainly ("no sample-outlier assessment was performed")
  rather than reporting its absence of findings. Stop that sentence there:
  first inspect every upstream output and do not say a PCA, sample-distance,
  sample-correlation, or other QC artifact was not produced when the package
  retains one. A source-level validator blocks both an unsupported QC-negative
  claim and a false artifact-absence statement.
- **Report a filter as the CRITERION it applied, never as a claim about what
  the removed data could support.** State the rule and the count it removed,
  and stop there. A count/abundance/quality pre-filter establishes only that
  the removed rows failed that comparison — it is not a power analysis, a
  dispersion estimate, or a detectability assessment, and the package contains
  no such analysis unless this run actually produced one and left the artifact.
  Correct prose names the applied rule, source count, removed count, and retained
  count. A separate observed property of the removed population may be reported
  only when the run computed and retained that property. Claims that removed
  observations lacked power, detectability, reliability, or another downstream
  capability require a corresponding retained analysis. The same rule covers
  every filter: name the criterion and the observed counts, not an inference.
- **Copy a statistic's retained definition verbatim; never paraphrase it.**
  When a stage emits `<metric>`, `<metric>_description`, and
  `<metric>_basis`, quote the description exactly and use only the populations,
  units, columns, and summary operations named by the basis. One value gets one
  definition. Do not substitute a significant subset for a tested population,
  change a median to a mean, convert a per-entity statistic into a per-sample
  statement, or infer membership that the basis does not establish. When the
  basis declares `neutral_reference`, state whether the retained value is
  above, below, or equal to that exact reference in the same clause as the
  metric key. A source-level invariant discovers this contract structurally for
  every task and modality.

These are the same pitfalls a source-level validator checks for in
`validate_reporting`/`validate_final_reporting` when one is present for
this run; getting them right at generation time avoids a re-dispatch
block.

### Report completeness contract (report-writing stages)

A stage whose task spec carries `interpretation_exempt_from_word_budget:
true` and/or a non-empty `required_report_sections` — `reporting`,
`final_reporting`, and `biological_interpretation` all qualify — is judged
by COMPLETENESS against `runtime/outputs/reporting/report-data.json`, not
brevity. That file is the canonical, deterministically-assembled summary of
every terminal result artifact (plus, when literature contextualization
ran, a `literature` rollup); the assembler already resolved every entity /
effect / significance / threshold BY NAME from this run's declared result
schema and embeds that executable contract as each artifact's
`result_schema`, so your job is to narrate it faithfully — never to recompute
it. A
deterministic validator re-derives every number you state directly from the
source result tables and blocks the deposit on a mismatch, so treat every
rule below as a hard correctness contract, not a style preference. This
generalizes across every modality the system runs: speak of "entities", the
"significant set", "effect", and "significance" as this run's schema
defines them — never assume genes/log2FC.

- **Cite every quantitative claim — count, threshold, effect size, direction
  split — DIRECTLY from `report-data.json`.** Never recompute, re-threshold,
  or invent a count, threshold, or effect size yourself, even with the raw
  result table open in front of you. If a number is not present in
  `report-data.json`, do not state it.
- **The complete significant-entities table is generated deterministically by
  the system** — rendered from `report-data.json` and appended to your report
  under a "Complete significant-entities tables" heading. Do NOT hand-render all
  N significant rows yourself: you will not reproduce thousands of rows reliably,
  and a partial table is worse than none. Instead, analyze the FULL significant
  set (its `n_significant`, `direction_split`, and `effect_distribution` are in
  `report-data.json`), narrate the findings, and surface a short **top-hits**
  table only from that artifact's `ranking`. Treat each ranking list as an
  ordered prefix: use `enriched` for positive effects, `depleted` for negative
  effects, and `undirected` when `directional` is false. Do not choose,
  re-sort, or substitute rows from `significant_entities`. The canonical order
  is strongest declared significance, then larger absolute effect, then entity
  name, then source row. State this rule accurately in the table caption. If
  significance is declared, never caption the same table "by effect size" or
  describe it as effect-first: absolute effect is only the second ordering key.
  Use an unambiguous caption such as "by canonical significance-first ranking
  (absolute effect as the first tie-breaker)." If
  `ranking` is absent, make no “top”, “leading”, or superlative claim for that
  artifact. In Markdown tables, do not place an unescaped `|` inside a cell:
  write headers such as "Absolute effect-size bin" instead of `|effect| bin`,
  or escape the character as `\|`. The exhaustive table is the system's job;
  the interpretation is yours.
- **Do not make a secondary superlative over an anaphoric displayed subset.**
  Phrases such as “the strongest within this tier” discard the subset
  definition when the sentence is extracted for verification. State only the
  canonical ranking represented in `report-data.json`, or state the observed
  effect without a rank claim.
- **Cover every declared section and table.** Your task spec's
  `required_report_sections` names every section id the report must contain
  (e.g. `provenance_method_rationale`, `qc_preprocessing`,
  `primary_results`, `literature_contextualization`, `reproducibility`,
  `claim_boundary`); its `required_tables` names every table id to render
  (e.g. `significant_entities`, `literature_concordance`). The
  `significant_entities` table is the system-generated complete table described
  above (you provide only the top-hits view in narrative); every OTHER declared
  table (e.g. `literature_concordance`) is yours to render in full. A missing
  declared section or agent-owned table is an incomplete report, not a shorter
  one.
- **Contextualize against the `literature` rollup, not memory.** Per
  entity, state its literature tag exactly as recorded — `concordant` /
  `discordant` / `unverifiable` (each with its `pmid`), `novel`, or
  `not_assessed` — and give `non_replications` its own dedicated treatment:
  for each, name the entity, its `prior_claim`, and what this run actually
  observed (`here_effect` / `here_significance`) — a prior-reported entity
  that was NOT significant here is as reportable as a concordant hit, not an
  omission. "Novel" is a claim about the SEARCHED set ONLY: an entity is
  `novel` only when a literature query was actually issued for it and returned
  no prior finding (`novel_count`). An entity retrieval was NOT performed for
  is `not_assessed`, NOT novel and NOT "no prior work" — never describe an
  unsearched entity as novel or as having no prior literature. State the
  `novel_count` and the `not_assessed_count` as SEPARATE headline buckets. Say
  how many entities were assessed from `n_entities_assessed` versus not
  assessed from `n_entities_not_assessed`, and list every
  `retrieved_sources` entry. The `concordant`, `discordant`, and `unverifiable`
  arrays retain evidence rows; one entity can contribute more than one row, so
  their lengths must not be added to obtain an entity count. For a literature
  finding, use only that same finding object's `effect` and `significance`.
  If either field is absent, omit that measurement from prose. Never borrow a
  number from another finding, a nearby row, model memory, or a prior run.
- **Bind every literature source to each entity before grouping prose.** A
  sentence that lists several entities and attributes the list to one PMID or
  other source asserts that the exact `(entity, source)` pair exists for every
  listed entity. Group entities under a source only after checking that exact
  pair in each retained evidence object. If the pairs differ, write separate
  clauses or sentences. Proximity in the matrix, a shared status, or a source
  carried by another entity never licenses a grouped source attribution.
- **Render the literature-concordance table at evidence-row granularity.**
  Write exactly one row for every object in `literature.concordant`,
  `literature.discordant`, and `literature.unverifiable`, preserving that
  object's entity, status, PMID, effect, and significance. Repeat an entity
  when it has several retained sources or several statuses. Do not aggregate
  PMIDs into one entity row, choose a priority status, or collapse the table to
  distinct entities. Headline the separate denominators explicitly:
  `n_entities_assessed` is a distinct-entity count, while
  `n_evidence_rows_assessed` is an evidence-row count whose status split is the
  three array lengths. Describe that split as evidence rows, never as a
  mutually exclusive partition of entities.
- **Never cite a PMID that is not in `retrieved_sources` / the evidence
  matrix — not even as background context.** Every PMID that appears anywhere
  in the report (including "Note", "Background", or discussion asides) MUST be
  one this run actually retrieved and verified (present in `retrieved_sources`
  or the `claims_evidence_matrix`). Do NOT pull a paper from your own memory,
  however relevant it seems — a source-level validator flags any cited PMID
  with no supporting matrix row as an ungrounded (hallucinated) citation and
  blocks the deposit. If prior context is genuinely missing, say so
  ("no prior-work PMID was retrieved for gene X") rather than supplying one
  from recall.
- **Do not add uncited scientific background from model memory.** A statement
  that a result is "consistent with known" biology, suggests a mechanism, or
  reflects an expected biological effect is an external claim. Make it only
  when the same sentence is grounded in a retained, verified literature object
  from this package. When no such object exists for the reported subject,
  restrict the prose to the observed association, effect, significance, and
  declared analysis context.
- **Account for every entity, and label each count's denominator.**
  When you summarize the `literature` rollup (or any categorized set), the
  entity counts must account for every distinct entity.
  `n_entities_assessed` covers entities with an entity-specific query OR an
  exact mention in the bounded retained evidence corpus.
  `n_entities_not_assessed` covers entities with neither. Only
  `no_prior_finding` establishes a searched negative and contributes to
  `novel_count`. `not_assessed_count` is a backward-compatible alias for
  `n_entities_not_assessed`; the two must agree. Report the not-assessed count
  as its own headline bucket; never fold it into `novel_count`, and never call
  the not-assessed entities novel or "no prior finding". Never drop the
  `unverifiable` bucket from a headline just because it is the least
  interesting. When you report both an entity-level count and a source-level
  or evidence-row count, name each denominator explicitly ("4 assessed
  entities" versus "10 evidence rows" or "30 PMIDs", with "12 entities not
  assessed" separate) so the counts are never conflated.
- **Use the literature count whose NAME matches the claim you are making.** The
  contextualization stage emits each count separately and defines each one:
  `n_entities_assessed` (distinct entities with an entity-specific query or an
  exact mention in the bounded retained evidence corpus),
  `n_evidence_rows_assessed` (rows of the evidence matrix with `searched=true` —
  one assessed entity contributes one row per cited source, so this is always
  >= the entity count and is NEVER a count of entities),
  `n_entities_not_assessed`, `n_evidence_rows_total`, `n_search_axes_total`
  (which may include axes naming a method or a dataset rather than an entity),
  and `n_search_axes_naming_an_assessed_entity`. Quote the definition the stage
  emitted in `count_definitions` rather than restating the population yourself.
  A real run read the 9 assessed evidence ROWS as "9 specific genes" when only 4
  entities were ever searched, then stated the correct 4 later in the same
  report — the same file gave two different answers for one quantity. Describe
  `n_entities_assessed` as assessed, not necessarily entity-query searched.
  Use `n_search_axes_naming_an_assessed_entity` when discussing the retained
  entity-specific query subset. Every count you state must be the one whose
  name matches the noun in your sentence.
- **Every count in a filtering funnel must be traceable to `report-data.json`.**
  If you describe an entity-count funnel (input → retained → tested →
  reported), each number must be one `report-data.json` provides (e.g.
  `n_total`, `n_significant`); do not introduce a stage-internal intermediate
  count a reader cannot reconcile against the file, and make the funnel
  arithmetic add up.
- **Keep identifier-mapping losses in their declared buckets.** For pathway
  ranking, copy `n_genes_pre_mapping`, `n_genes_mapped`,
  `n_genes_unmapped`, `n_duplicate_gene_labels_removed`, and
  `n_genes_ranked` from the retained pathway result. Check both identities:
  pre-mapping = mapped + unmapped, and mapped = ranked + duplicate labels
  removed. Never describe the ranked population as the duplicate-removal
  count, and never call the combined mapping-plus-deduplication loss
  "unmapped."
- **Caveat context heterogeneity uniformly.** When a concordance or discordance
  rests on prior evidence from a different biological context than this analysis
  — a different tissue, organism, or assay, evident from the `evidence_quote` —
  say so, and apply that caveat to EVERY such entity, not only the ones whose
  direction disagrees.
- **Frame extreme effects as extremes.** When you cite an entity at the tail of
  the `effect_distribution` (an outlier effect size), present it as an extreme,
  not a calibrated point estimate — an extreme effect commonly reflects
  near-absent signal in one arm, so avoid wording that implies a precise
  fold-change.
- **Claim-boundary discipline is unchanged by completeness.** Every
  statement stays associative — "associated with" / "enriched in" — never
  causal ("drives", "causes"); citing every number `report-data.json`
  provides is not license to overstate what those numbers mean.

The direction-word rule above (derive "up"/"down"/"higher"/"lower" from the
sign of the statistic, never free text) applies unchanged here: when you
render `direction_split` or an entity's `effect`, the words around them
must still come from the sign you're citing, not from intuition.

Do not equate retained scripts with a successful replay. Unless this package
already contains a machine-generated replay result that covers the outputs
named in the sentence, describe scripts and locks as replay inputs or retained
provenance only. Do not call the workflow fully replayable, fully reproducible
offline, or able to regenerate every result merely because scripts exist.

### Data acquisition — required input stage

When `task-spec.json` carries `required_input_stage`, it names the EDAM data
type the composed workflow expects THIS acquisition stage to hand downstream.
Fetch to match it; do not substitute a different, easier-to-grab form.

- `data:2044` (raw sequence reads — the default): fetch RAW sequencing reads
  (FASTQ from SRA/ENA, or raw mass-spec RAW/mzML). Do NOT substitute a
  deposited supplementary processed matrix, a pre-called peak/VCF set, or a
  precomputed count table — the downstream stages (alignment, quantification,
  calling) need the raw substrate, and swapping in a processed product silently
  strands them.
- any other IRI (a processed-product entry point, e.g. `data:3917` counts,
  `data:1255` peaks, `data:3498` variants, `data:0863` alignments, `data:3134`
  DE results, `data:3028` taxonomy, `data:2976` protein abundance): the
  workflow is downstream-first and the producing chain has been pruned, so
  materialize THAT product directly (the deposited/supplied processed artifact)
  rather than re-fetching raw reads.

If `required_input_stage` is absent, acquire the raw input the stage's ports
declare, as before.

### Discovery tasks (`discover_*`)

A `discover_*` task selects the method for its downstream stage. Follow the
discovery scoring procedure in `PROMPT.md` (env-capability + spec-preferred
boosts + composite scoring), write the ranked `candidate_pool_full` and the
chosen method to `decision.json`, and — unless an SME pre-approval is
already recorded for this stage (see `runtime/.sme-auto-approve-discoveries`
and any `sme-review-confirmed-*.json`) — block by default with
`blocker_kind: AwaitingSmeApproval` rather than silently committing to a
method. When pre-approval is present, record the auto-advance in
`decision.json` and complete.

When `task-spec.json` carries a non-empty `spec_preferred_methods` map, every
`method_id` in it is an SME/intake-requested method and is a hard preference,
not a hint: include each one in `candidate_pool_full` even if it is absent from
the stage's curated `candidate_tools` (a `candidate_pool_augmented: true` flag
means the pool was extended for exactly this reason — do not drop the extras),
apply the spec_match boost, and rank the highest-scoring spec-preferred
env-available method at #1. When exactly one spec-preferred method is
env-available, record it as the pick with `spec_preference_applied: true` and
`auto_advanced: true` in `decision.json` and complete the task WITHOUT
blocking — naming the method IS the SME's selection (equivalent to an entry in
`runtime/.sme-auto-approve-discoveries`). Block with `AwaitingSmeApproval` only
when `spec_preferred_methods` is empty or two-or-more spec-preferred methods are
env-available.

Every `candidate_pool_full` row must retain `literature_eligible`,
`supporting_evidence_count`, and `high_quality_evidence_count` from rows whose
axis and candidate method both match exactly. Retain false values and zero
counts rather than omitting them. Every candidate row must also retain
`recommended_tier` using the same assignment recorded in the decision's root
`tiers` map.

When the discover node carries `attributes.goal_context` (or the
`## Analysis objective` in `PROMPT.md` names a specific detection goal such as
low-frequency / heteroplasmic variants), treat it as a GOAL signal on the
`default_suitability` composite axis: rank candidates by fitness for that goal
(an allele-frequency-window-aware filter suits a low-AF-tail goal; a
depth/quality-only hard filter suits a high-confidence-germline goal). This
shapes ranking only — it is NOT a threshold and NOT a mandated tool; you still
choose, install, and record `decision.json::chosen` exactly as above.

### SME parameter overrides

Read `task-spec.json::spec.parameters` before writing executable code. For each
declared parameter, use `spec.sme_parameter_overrides[name]` when present;
otherwise use the declared `default`. Apply that resolved value to the method
call and report it from the same run-time variable. A parameter declaration is
part of the executable task contract, not documentation that may be ignored.

When `task-spec.json` carries a non-empty `spec.sme_parameter_overrides` map,
treat each `{parameter: value}` entry as an SME-mandated input you MUST apply
verbatim. These are the SME's explicit, deliberate choices — not suggestions,
not defaults you may re-derive. Do not re-select, round, clamp, or substitute
them, and do not block asking the SME to reconfirm a value they already set.
This is the same hard-instruction channel as `spec_preferred_methods` (which
fixes the *method*); `sme_parameter_overrides` fixes concrete *parameter
values* on the chosen method (e.g. a min-MAPQ floor, a min-genes-per-cell
threshold). Record in `decision.json` that you applied each override so the
audit trail shows the SME value flowed through unchanged. If an override is
genuinely inapplicable to the tool you ran, do not silently ignore it — record
a typed blocker naming the parameter so the SME can decide.

### Domain-correctness signal (re-dispatch)

Before you start, check for `runtime/inputs/$ECAA_TASK_ID/domain-correctness-signal.json`.
If it exists, a prior run of THIS task produced a result that did not meet a
required domain-correctness check, and the harness is re-dispatching you to
correct it. The file lists, per failed check, the assertion id and a plain
statement of what is biologically off — the design's required bound versus the
number recomputed from your OWN previous `result.json` (for example: "the
low-AF heteroplasmy band [0.01, 0.5) this design requires has 0 calls in your
result.json; the design requires at least 1 — revisit").

This signal tells you WHAT is off, never HOW to fix it. It names no tool, no
flag, no aligner or caller, no statistical test, and no threshold value to set —
those choices are yours, exactly as on a first dispatch. Re-do the analysis so
the recomputed quantity satisfies the design's bound, write fresh outputs and a
new `result.json`, and record your terminal state as usual. If, after genuinely
revisiting your approach, you conclude the design's expectation cannot be met
for this data (e.g. the signal is biologically wrong for this call set), block
with a precise, typed `blocker_kind` explaining why rather than fabricating a
passing number — the recovery budget is bounded and a stranded block is then
surfaced to the SME.

### Iterate-until stages

A `Cardinality::IterateUntil` stage is emitted as a 4-template scaffold
(`iterate_gate_<id>`, `<id>`, `iterate_check_<id>`, `validate_<id>`).
Expand iterations only as the input's convergence metric requires; the
expansion is bounded and deterministic from the inputs. Do not loop
unboundedly.

### Blockers

When you cannot complete the task, set `status: "blocked"` with a typed
`blocker_kind` (the vocabulary lives in `policies/blockers.json` when
present) and describe precisely what input is missing or what decision the
SME must make. Do not fail silently and do not guess past a missing
input — one precise, typed blocker lets the SME or a follow-up dispatch
resolve it cleanly.

### Containerized execution

This task runs inside the per-task container image (derived from the
`bio-min` base). Tools you need that aren't already present can be
installed at task start, but prefer in-image tools when method scores are
close — every install spends wall-clock and turns. All artifacts you write
under `runtime/outputs/$ECAA_TASK_ID/` persist into the emitted package on
the host.

Installation discipline (these prevent the most common way a dispatch
wedges and burns its whole budget):

- **Install synchronously, in the foreground.** Run the install command
  and wait for it to finish in the same step. NEVER launch an install in
  the background and then spin on a polling loop such as
  `until <check>; do sleep N; done` — a poll whose check tests the wrong
  path (e.g. `requireNamespace` against `R_LIBS_USER` when the package
  actually landed in a conda env) never exits, so the loop runs forever,
  keeps the heartbeat artificially fresh, and the dispatch hangs even
  though your real work already finished. No background `&` + poll loops.
- **The container root filesystem is read-only — the conda *base* env
  cannot be modified.** `conda install` / `mamba install` with no target
  env install into the active base env and fail with
  `critical libmamba filesystem error: ... Read-only file system
  [/opt/conda/conda-meta/history]`. ALWAYS install conda/bioconda packages
  into a NEW named env, which is redirected to a writable cache dir:
  `mamba create -y -n <env> -c conda-forge -c bioconda <pkg>` — NEVER bare
  `mamba install <pkg>` / `conda install <pkg>`. (`pip install` needs no
  such care: it is pre-pointed at a writable user base and works as-is.)
- **Use `ecaa-install bioc <pkg>` for all Bioconductor packages.** It
  resolves the pre-built bioconda binary into the shared `ecaa-bioc` conda
  env (no C++ source compile; no 10–30 min wait; no Rcpp build failures).
  The env is created on first use and reused on subsequent calls. Run your
  R analysis with `conda run -n ecaa-bioc Rscript <script>`.
  Only fall back to a Python equivalent (e.g. `pip install gseapy`) when
  `ecaa-install bioc` itself reports that no bioconda binary exists for the
  requested package — do NOT substitute a Python equivalent merely because a
  previous compile attempt failed, since `ecaa-install bioc` no longer
  source-compiles. Silently substituting a different implementation (e.g.
  pydeseq2 for DESeq2) can change statistical results without warning.
- **Run the analysis with the interpreter you installed into.** If you
  install into a conda env, invoke `conda run -n <env> Rscript …` (or that
  env's `python`) so the tool actually resolves — don't install into one
  location and then look for it in another.
