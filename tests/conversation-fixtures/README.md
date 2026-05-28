# Conversation fixture corpus

> **Scope:** 25 baseline fixtures + 28 extension fixtures = **53 total**
> (`fixtures/*.yaml`). Wired into `crates/conversation/tests/fixture_runner.rs`
> and exercised on every `cargo test` pass under the mock backend. The
> real-LLM rubric scorer runs nightly via
> `.github/workflows/nightly-rubric.yml`.
>
> **Non-runner fixtures:** a handful of `.json` files in this directory are
> reference data consumed directly by Rust tests, not driven through the
> scripted-LLM runner. `disposition-auto-apply-ivd.json` is the v0 IVD
> disposition shape; it's exercised by
> `crates/server/src/chat_routes/dispositions.rs::tests::ivd_fixture_*`.

## Purpose

This corpus is the regression and quality gate for the LLM-mediated chat
flow. Two test modes:

- **Scripted-LLM CI runner** (every commit) — uses `MockLlmBackend` with a
  per-fixture scripted `TurnResponse` sequence. Asserts that the deterministic
  surface of the package produced (workflow id, intake fields, tool-call
  sequence, final session state) matches `expected_final_state`.

- **Real-LLM nightly runner** — uses `AnthropicClient` against the `User`
  steps in a fixture's `flow` and scores the resulting transcript with a
  separate Sonnet pass against the rubric in `runner/scorer_prompt.txt`.
  Reports per-fixture rubric scores; gates on a 13/16 average.

## Fixture format

Each fixture is a YAML file under `fixtures/`. The runner expects four top-level
sections — `id`, `category`, `flow`, `mock_responses`, `expected_final_state` —
plus the optional `description` and `rubric_notes`. The canonical Rust types
live in `crates/conversation/tests/fixture_runner.rs::{Fixture, FlowStep,
FixtureLlmResponse, ExpectedFinalState}`.

```yaml
id: 01_single_cell_rnaseq_ivd_baseline
category: happy_path_single_cell
description: |
  Plain-language IVD scRNA-seq scenario, no surprises. Happy path through
  intake → confirmation → emission.

# Ordered transcript of SME inputs and deterministic button clicks. Each
# step is one tagged variant of FlowStep (snake_case `kind` discriminator).
flow:
  - kind: user
    text: "We have a bunch of scRNA-seq data from human IVD samples..."
  - kind: user
    text: "Yes on batch correction — totally different labs."
  - kind: confirm

# Pre-recorded LLM turns drained in declaration order by MockLlmBackend
# every time the conversation loop calls send_turn. Each entry is one
# tagged FixtureLlmResponse — `text` ends the turn, `tool_use` continues
# the loop with another mock response.
mock_responses:
  - kind: tool_use
    tool:
      name: append_intake_prose
      input: { prose: "scRNA-seq from IVD ..." }
  - kind: text
    text: "Got it — single-cell from IVD tissue."
  - kind: tool_use
    tool:
      name: propose_summary_confirmation
      input: { summary_markdown: "Here's the plan: ..." }
  - kind: text
    text: "Take a look and click Confirm when ready."

# Assertions against the resulting Session after `flow` finishes.
expected_final_state:
  state_kind: emitted             # SessionState serde tag
  user_confirmed: true
  tool_calls_observed:            # subsequence (gaps allowed)
    - append_intake_prose
    - propose_summary_confirmation
    - emit_package
  package_artifacts_present:      # files relative to the package dir
    - WORKFLOW.json
  # Optional: harness_events_count, last_assistant_contains,
  # blocked_reason_contains (used by infra / harness fixtures).

# Optional notes for the real-LLM rubric scorer (see scorer_prompt.txt).
rubric_notes: |
  Watch for: did the assistant restate the claim_boundary in plain
  language? Did it avoid recommending methods unprompted? Did it ask at
  most one question per turn?
```

### `FlowStep` variants

| `kind` | Fields | Effect |
|---|---|---|
| `user` | `text` | `service.send_turn(text)` — ordinary SME prose. |
| `confirm` | — | Server-side `/confirm` button click. |
| `reject` | — | Server-side `/reject` (Make corrections) button click. |
| `unblock` | — | Server-side `/unblock` button click. |
| `inject_infra_error` | `reason` | Fires `StateTrigger::InfraError` (fixtures 17/18). |
| `enqueue_harness_event` | `event_kind`, `task_id`, `detail`, optional `backend`/`instance_id`/`instance_type` | Pushes a synthetic event into the per-fixture `HarnessBatcher` (50 ms window). Surfaces a `[<backend> · <type>]` tag in the synthetic turn when both `backend` and `instance_type` are set (fixture 43). |
| `wait` | `ms` | Sleep so the batcher's window can flush before assertion. |
| `inject_awaiting_sme_selection` | `stage_id`, `candidates` | Force-assigns `Blocked { AwaitingSmeSelection { … } }` so `select_sensitivity_winner` can be exercised (fixture 29). |

## Running

```bash
# Scripted (always, fast — under the mock backend)
cargo test -p scripps-workflow-conversation --test fixture_runner

# Single fixture
cargo test -p scripps-workflow-conversation --test fixture_runner -- 01_single

# Latency baseline gate (8000 ms per fixture by default)
make latency-baseline
```

The scripted runner is wired into `make test`; the real runner is wired into
the nightly GitHub Actions cron job.

## Fixture index

### Baseline corpus (one per category, 25 fixtures)

| # | id | category |
|---|---|---|
| 1 | `01_single_cell_rnaseq_ivd_baseline` | happy path / single-cell |
| 2 | `02_rnaseq_de_bulk_case_control` | happy path / bulk DE |
| 3 | `03_variant_calling_germline` | happy path / variant |
| 4 | `04_chip_seq_tf_binding` | happy path / ChIP |
| 5 | `05_metagenomics_shotgun` | happy path / metagenomics |
| 6 | `06_proteomics_dia` | happy path / proteomics |
| 7 | `07_generic_omics_fallback` | happy path / generic |
| 8 | `08_classification_ambiguous` | classification / clarify |
| 9 | `09_low_confidence_followup` | classification / followup |
| 10 | `10_scope_creep_pushback` | clarification / scope |
| 11 | `11_correction_modality_change` | correction |
| 12 | `12_correction_method_swap` | correction |
| 13 | `13_correction_after_confirm_reject` | reject path |
| 14 | `14_claim_boundary_paraphrase` | post-emit / claim boundary |
| 15 | `15_claim_boundary_alt_phrasing` | post-emit |
| 16 | `16_pending_to_intake_methods_preserved` | state preservation |
| 17 | `17_infra_error_api_unreachable` | infra |
| 18 | `18_infra_error_session_lost` | infra |
| 19 | `19_accession_with_metadata` | structured capture |
| 20 | `20_long_conversation_22_turns` | long conversation |
| 21 | `21_post_emission_question` | post-emit / followup |
| 22 | `22_quick_reply_clarification` | quick replies |
| 23 | `23_intake_followup_unresolved_discovery` | intake_followup |
| 24 | `24_harness_progress_batched_turns` | harness progress |
| 25 | `25_harness_blocker_routed_to_sme` | blocker / unblock |

### Extension fixtures (28 fixtures)

| # | id | category |
|---|---|---|
| 26 | `26_ivd_deep_scenario_lotz_aligned` | prior_experience_method_capture |
| 27 | `27_ivd_amend_after_emit` | amend_pathway |
| 28 | `28_ivd_rerun_clustering` | rerun_pathway |
| 29 | `29_ivd_integration_sweep` | sensitivity_pathway |
| 30 | `30_ivd_branch_to_compartment_split` | branching_pathway |
| 31 | `31_ivd_lotz_v1_to_v2` | lotz_iteration_calibration |
| 32 | `32_ivd_lotz_v2_to_v3` | lotz_iteration_calibration |
| 33 | `33_ivd_lotz_v3_to_v4` | lotz_iteration_calibration |
| 34 | `34_ivd_lotz_v4_to_v5` | lotz_iteration_calibration |
| 35 | `35_ivd_pilot_right_sizes_star` | wave_7_pilot |
| 36 | `36_ivd_stall_resize_flow` | wave_7_stall |
| 37 | `37_ivd_rerun_produces_concordance_report` | wave_7_cross_version |
| 38 | `38_ivd_amend_discordant_shows_red_rows` | wave_7_cross_version |
| 39 | `39_hardware_star_align_thread_obedience` | hardware_awareness |
| 40 | `40_hardware_deepvariant_gpu_routing` | hardware_awareness |
| 41 | `41_hardware_bwa_samtools_pipe_split` | hardware_awareness |
| 42 | `42_ivd_requires_review_gating` | sme_review_gate |
| 43 | `43_slurm_backend_badge_in_progress_turn` | harness_progress |
| 44 | `44_clinical_trial_confirmatory` | wave8_clinical_trial |
| 45 | `45_time_series_forecast` | wave8_time_series_forecast |
| 46 | `46_clinical_trial_exploratory_biomarker` | wave8_clinical_trial |
| 47 | `47_clinical_trial_post_hoc_deviation` | wave8_clinical_trial |
| 48 | `48_clinical_trial_vocabulary_boundary` | wave8_clinical_trial |
| 49 | `49_time_series_structural_break` | wave8_time_series_forecast |
| 50 | `50_time_series_vocabulary_boundary` | wave8_time_series_forecast |
| 51 | `51_checkpoint_mode_fast_auto_advances` | wave8_checkpoint_mode |
| 52 | `52_mode_lock_post_confirmation` | wave8_mode_lock |
| 53 | `53_cross_class_deviation_preserves_bio` | wave8_cross_class |

The four lotz transition fixtures (31–34) are also wired into `make
lotz-iteration-calibration` and rotated nightly against the real Anthropic
backend via `.github/workflows/nightly-lotz-rotation.yml`.

### Cross-references to scenario corpora

Two related fixture corpora live elsewhere in the tree:

- **`testdata/scenarios/`** — public-data IVD-shaped scenarios for the
  compiler's intake/build path. Each scenario is a `request.md` +
  `overview.md` + a per-modality input bundle (`studies.tsv`,
  `*.csv`, etc.). Currently ships **12 directories
  (`01-bulk-rnaseq-ibd/` … `12-time-series-forecast/`)**;
  `testdata/scenarios/README.md` indexes the bio scenarios 1–10 (the
  two non-bio scenarios `11-clinical-trial-mock-phase3/` and
  `12-time-series-forecast/` are documented inline in their own
  `overview.md`). Time-series-forecast tool-loop coverage is provided
  by fixtures 45, 49, and 50 in this corpus; the scenario directory
  exercises the same class through the compiler intake/build path.
- **`e2e/fixtures/scenarios/`** — Playwright scenario YAML driven by
  `e2e/helpers/scenarioRunner.ts`. Includes both `11-clinical-trial-mock-phase3.yaml`
  and `12-time-series-forecast.yaml`.

## Scoring rubric

See `runner/scorer_prompt.txt`. Eight dimensions, each scored 0–2:

1. Naturalness — does it sound like an expert peer, not a form?
2. Continuity — does it reference what the SME just said?
3. One-question-per-turn discipline
4. Method neutrality — no unprompted recommendations
5. Claim boundary surfacing
6. Tool-call efficiency — minimum sufficient calls
7. Confirmation discipline — no pre-emit, no skipped confirms
8. Recovery — graceful handling of errors and rejections

Target: average ≥ 13/16 across the corpus.
