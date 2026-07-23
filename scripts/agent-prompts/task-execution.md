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
   figures you rendered. On a blocked exit include `blocker_kind` and a
   `what_would_unblock` note.

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

- **Direction words come from the sign of the statistic, never free text.**
  When you write "above"/"below", "higher"/"lower", "increased"/"decreased",
  or similar, derive the word from the actual sign or ratio you computed —
  do not describe direction from intuition or from what you expect the
  result to look like. Example: if a ratio such as
  `top_effect_abundance_ratio` computes to `< 1`, the correct word is
  "below"/"lower" (e.g. 0.56 means the top effects sit at ~56% of the
  reference — below it, not "substantially above" it).
- **Report what was TESTED, not what was LOADED.** A gene-set /
  pathway count (or any "N analyzed" figure) must be the post-filter row
  count of the results table you actually computed (e.g. `nrow()` of the
  enrichment result after `minSize`/`maxSize` filtering) — never the
  pre-filter count of sets loaded from the collection file. If your script
  prints both numbers, only the post-filter one belongs in the narrative
  and in any `*_tested` field; record it in your JSON result too so a
  downstream validator can check it against the table rowcount.
- **Every "FDR" mention must name its family and threshold.** Gene-level
  differential-expression FDR (`padj`) and pathway-level enrichment FDR
  (e.g. `fgsea` padj) are different multiple-testing corrections over
  different universes with different conventional thresholds (commonly
  0.05 vs 0.25). Never write a bare "FDR" — write, every time it appears
  (not only in a reproducibility appendix), "gene-level FDR (padj) <
  0.05" / "pathway-level (fgsea) FDR < 0.25", using the thresholds this
  run actually applied.
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
schema, so your job is to narrate it faithfully — never to recompute it. A
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
- **Render the full significant set as a table.** For each artifact in
  `artifacts`, one row per entry in its `significant_entities` (`entity`,
  `effect`, `significance`, and its literature tag). When that artifact's
  `spilled_to_attachment_only` is `true` (the degenerate-output guard
  tripped), do NOT inline the set — state plainly that the full significant
  set is in the attached `significant_table_path`, and summarize it instead
  via `direction_split` / `effect_distribution`.
- **Cover every declared section and table.** Your task spec's
  `required_report_sections` names every section id the report must contain
  (e.g. `provenance_method_rationale`, `qc_preprocessing`,
  `primary_results`, `literature_contextualization`, `reproducibility`,
  `claim_boundary`); its `required_tables` names every table id to render
  (e.g. `significant_entities`, `literature_concordance`). A missing
  declared section or table is an incomplete report, not a shorter one.
- **Contextualize against the `literature` rollup, not memory.** Per
  entity, state its literature tag exactly as recorded — `concordant` /
  `discordant` / `unverifiable` (each with its `pmid`) or `novel` — and give
  `non_replications` its own dedicated treatment: for each, name the
  entity, its `prior_claim`, and what this run actually observed
  (`here_effect` / `here_significance`) — a prior-reported entity that was
  NOT significant here is as reportable as a concordant hit, not an
  omission. State the `novel_count` and what it covers, and list every
  `retrieved_sources` entry.
- **Account for every assessed entity, and label each count's denominator.**
  When you summarize the `literature` rollup (or any categorized set), the
  category counts must account for the WHOLE assessed set — `concordant` +
  `discordant` + `unverifiable` + `novel_count` covers every entity
  contextualized; never drop the `unverifiable` bucket from a headline just
  because it is the least interesting, and never state a total that omits it.
  When you report both an entity-level count and a source-level (PMID) count,
  name each denominator explicitly ("4 of 8 assessed entities" vs "10 of 30
  PMIDs") so the two are never conflated.
- **Every count in a filtering funnel must be traceable to `report-data.json`.**
  If you describe an entity-count funnel (input → retained → tested →
  reported), each number must be one `report-data.json` provides (e.g.
  `n_total`, `n_significant`); do not introduce a stage-internal intermediate
  count a reader cannot reconcile against the file, and make the funnel
  arithmetic add up.
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

When the discover node carries `attributes.goal_context` (or the
`## Analysis objective` in `PROMPT.md` names a specific detection goal such as
low-frequency / heteroplasmic variants), treat it as a GOAL signal on the
`default_suitability` composite axis: rank candidates by fitness for that goal
(an allele-frequency-window-aware filter suits a low-AF-tail goal; a
depth/quality-only hard filter suits a high-confidence-germline goal). This
shapes ranking only — it is NOT a threshold and NOT a mandated tool; you still
choose, install, and record `decision.json::chosen` exactly as above.

### SME parameter overrides

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
