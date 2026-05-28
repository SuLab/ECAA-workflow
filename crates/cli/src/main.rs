//! `scripps-workflow` — CLI front-end for the deterministic bioinformatics
//! compiler and package emitter.
//!
//! Dispatches subcommands: `chat`, `chat-llm`, `intake`, `build`, `dag`, `serve`,
//! `migrate-sessions`. The compiler path (`intake`/`build`) is synchronous; only
//! `chat-llm` and `serve` pull in `tokio`.

mod chat;
mod chat_llm;
// v3 P7 — `migrate-sessions` subcommand. Applies the v3 P7
// `u32 → SemVer` migrator chain to on-disk session JSON in place.
mod migrate_sessions;

use anyhow::Result;
use clap::{Parser, Subcommand};
use scripps_workflow_core::dag::dag_to_dot;

#[derive(Parser)]
#[command(
    name = "scripps-workflow",
    about = "Scripps workflow compiler and package emitter"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start an intake conversation to define and emit a workflow package
    Chat {
        /// Path to config directory (default:./config)
        #[arg(long, default_value = "config")]
        config: String,
        /// Output directory for emitted package
        #[arg(short, long, default_value = "output-package")]
        output: String,
    },
    /// LLM-mediated intake REPL (dev/test surface — requires SWFC_ANTHROPIC_API_KEY)
    ChatLlm {
        /// Path to config directory (default:./config)
        #[arg(long, default_value = "config")]
        config: String,
        /// Output directory for emitted package
        #[arg(short, long, default_value = "output-package")]
        output: String,
    },
    /// Build a package directly from an archetype YAML
    /// (bypasses intake chat, useful for testing).
    Build {
        /// Path to archetype YAML.
        #[arg(long)]
        archetype: String,
        /// Output directory for emitted package
        #[arg(short, long, default_value = "output-package")]
        output: String,
        /// Emit IEEE 2791-2020 BioCompute Object alongside the package
        /// (`bco.json` at the package root). Auto-on for the
        /// `clinical_trial` project class even when this flag is unset.
        #[arg(long)]
        emit_bco: bool,
    },
    /// Show the DAG from an existing package directory
    Dag {
        /// Path to an emitted package directory (must contain WORKFLOW.json)
        #[arg(short, long)]
        package: String,
        /// Emit DOT format instead of human-readable
        #[arg(long)]
        dot: bool,
    },
    /// Start the workflow planning server (UI backend)
    Serve {
        /// Port to listen on
        // Default dev server port. Port conventions documented in docs/api-reference.md.
        // 3000 (here) is the user-facing dev port; 3737 is the harness-callback port
        // (in crates/server/src/chat_routes/execution/start.rs).
        #[arg(long, default_value = "3000")]
        port: u16,
    },
    /// Classify intake text, build DAG, and emit package (non-interactive)
    Intake {
        /// Path to intake text file
        #[arg(short, long)]
        input: String,
        /// Output directory for emitted package
        #[arg(short, long, default_value = "output-package")]
        output: String,
        /// Path to config directory
        #[arg(long, default_value = "config")]
        config: String,
        /// Emit IEEE 2791-2020 BioCompute Object alongside the package
        /// (`bco.json` at the package root). Auto-on for the
        /// `clinical_trial` project class even when this flag is unset.
        #[arg(long)]
        emit_bco: bool,
    },
    /// List discoverable archetypes (or atoms) from
    /// `config/archetypes/` and `config/stage-atoms/`. SME tooling
    /// surface for "what can the composer route to?" without firing
    /// up the chat REPL.
    List {
        /// What to list. `archetypes` reads `config/archetypes/*.yaml`;
        /// `atoms` reads `config/stage-atoms/*.yaml`.
        #[arg(value_enum)]
        kind: ListKind,
        /// Path to config directory
        #[arg(long, default_value = "config")]
        config: String,
        /// Output as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// v3 P7 — apply schema-version migrations to on-disk session
    /// JSON in place. Walks `$SWFC_CHAT_SESSIONS_DIR` (or
    /// `$HOME/.scripps-workflow/sessions`) and rewrites legacy
    /// `schema_version: u32` values to the canonical SemVer string.
    /// `--dry-run` reports counts without writing back.
    MigrateSessions(migrate_sessions::MigrateSessionsArgs),
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum ListKind {
    /// List archetypes from `config/archetypes/`
    Archetypes,
    /// List atoms from `config/stage-atoms/`
    Atoms,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat { config, output } => {
            chat::run_chat(&config, &output)?;
        }
        Commands::ChatLlm { config, output } => {
            chat_llm::run_chat_llm(&config, &output)?;
        }
        Commands::Build {
            archetype,
            output,
            emit_bco,
        } => {
            // Only the canonical `--archetype` flag is accepted; the
            // `--taxonomy` alias is no longer recognized.
            run_build(&archetype, &output, emit_bco)?;
        }
        Commands::Dag { package, dot } => {
            run_dag(&package, dot)?;
        }
        Commands::Serve { port } => {
            run_serve(port)?;
        }
        Commands::Intake {
            input,
            output,
            config,
            emit_bco,
        } => {
            run_intake(&input, &output, &config, emit_bco)?;
        }
        Commands::List { kind, config, json } => {
            run_list(kind, &config, json)?;
        }
        Commands::MigrateSessions(args) => {
            migrate_sessions::run(args)?;
        }
    }
    Ok(())
}

/// `scripps-workflow list archetypes|atoms` lists the
/// discoverable composer surface from `config/`. Writes JSON
/// (machine-readable, `--json`) or a human-readable summary table.
/// Reads via `ArchetypeRegistry::load_from_dir` / `AtomRegistry::load_from_dir`
/// so the loader's schema validation runs end-to-end; an
/// archetype/atom YAML that fails its `*.schema.json` sidecar is
/// surfaced before the SME picks it.
fn run_list(kind: ListKind, config: &str, as_json: bool) -> Result<()> {
    use colored::Colorize;
    use std::path::Path;

    let config_dir = Path::new(config);
    match kind {
        ListKind::Archetypes => {
            let dir = config_dir.join("archetypes");
            let registry =
                scripps_workflow_core::archetype_registry::ArchetypeRegistry::load_from_dir(&dir)?;
            if as_json {
                #[derive(serde::Serialize)]
                struct Row<'a> {
                    id: &'a str,
                    version: &'a str,
                    project_class: &'a str,
                    goal_data: &'a str,
                    goal_format: Option<&'a str>,
                    description: &'a str,
                }
                let rows: Vec<Row> = registry
                    .iter()
                    .map(|(_, a)| Row {
                        id: &a.id,
                        version: &a.version,
                        project_class: &a.project_class,
                        goal_data: &a.goal_data,
                        goal_format: a.goal_format.as_deref(),
                        description: &a.description,
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{}",
                    format!("# {} archetypes registered", registry.iter().count()).bold()
                );
                for (_, arch) in registry.iter() {
                    println!(
                        "  {} v{} ({}) {} → {}{}",
                        arch.id.green(),
                        arch.version,
                        arch.project_class,
                        arch.sme_summary,
                        arch.goal_data,
                        arch.goal_format
                            .as_deref()
                            .map(|f| format!(" / {}", f))
                            .unwrap_or_default(),
                    );
                }
            }
        }
        ListKind::Atoms => {
            let dir = config_dir.join("stage-atoms");
            let registry = scripps_workflow_core::atom_registry::AtomRegistry::load_from_dir(&dir)?;
            if as_json {
                #[derive(serde::Serialize)]
                struct Row<'a> {
                    id: &'a str,
                    version: &'a str,
                    role: String,
                    edam_operation: &'a str,
                    edam_data: Option<&'a str>,
                    edam_format: Option<&'a str>,
                    description: &'a str,
                }
                let rows: Vec<Row> = registry
                    .iter()
                    .map(|(_, a)| Row {
                        id: &a.id,
                        version: &a.version,
                        role: format!("{:?}", a.role),
                        edam_operation: &a.edam_operation,
                        edam_data: a.edam_data.as_deref(),
                        edam_format: a.edam_format.as_deref(),
                        description: &a.description,
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{}",
                    format!("# {} atoms registered", registry.iter().count()).bold()
                );
                for (_, atom) in registry.iter() {
                    let role = format!("{:?}", atom.role).to_lowercase();
                    println!(
                        "  {} v{} ({}) → {}{}",
                        atom.id.green(),
                        atom.version,
                        role,
                        atom.edam_operation,
                        atom.edam_data
                            .as_deref()
                            .map(|d| format!(" → {}", d))
                            .unwrap_or_default(),
                    );
                }
            }
        }
    }
    Ok(())
}

fn run_build(archetype: &str, output: &str, emit_bco_flag: bool) -> Result<()> {
    use colored::Colorize;
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    use scripps_workflow_core::atom_registry::AtomRegistry;
    use scripps_workflow_core::bco::emit_bco;
    use scripps_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
    use scripps_workflow_core::emitter::{emit_package, EmitConfig};
    use scripps_workflow_core::goal_spec::GoalSpec;
    use std::collections::BTreeMap;
    use std::path::Path;

    let archetype_path = Path::new(archetype);
    let archetype_bytes = std::fs::read(archetype_path).map_err(|e| {
        anyhow::anyhow!(
            "Cannot read archetype YAML '{}': {}",
            archetype_path.display(),
            e
        )
    })?;
    // Phase B4 — load via ArchetypeRegistry on the parent directory so
    // schema validation runs end-to-end (matches the production load
    // path). The CLI argument is a file path; the registry handles the
    // directory containing it.
    let archetype_dir = archetype_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    let archetype_id_from_filename = archetype_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("archetype path has no stem"))?
        .to_string();
    let registry_for_lookup = ArchetypeRegistry::load_from_dir(&archetype_dir)?;
    let archetype_obj = registry_for_lookup
        .get(&archetype_id_from_filename)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "archetype id '{}' (derived from filename) not found in registry at {}",
                archetype_id_from_filename,
                archetype_dir.display()
            )
        })?
        .clone();

    println!("{}", "Building workflow package...".cyan());

    let workflow_id = workflow_id_from_bytes(&archetype_bytes);

    // Infer config root from archetype path (typically config/archetypes/<id>.yaml).
    let config_root = archetype_path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("."));

    // Load atom + archetype registries from the inferred config dir so
    // the composer has the catalog in scope.
    let atoms = AtomRegistry::load_from_dir(&config_root.join("stage-atoms"))?;
    let archetypes = ArchetypeRegistry::load_from_dir(&config_root.join("archetypes"))?;

    // Phase B4 — build via the v4 composer using the archetype's
    // declared goal and modality. `--build` skips intake/classifier so
    // we synthesize a GoalSpec from the archetype's `goal_data` field.
    // Carry `goal_kind_hint` into `modifiers["kind"]` when set so the
    // planner's score_archetype_full tie-breaker (e.g. proteomics_dda vs
    // proteomics_dia, both sharing modality_hint + goal_data) can pick a
    // single seed instead of returning None to the search path (which
    // then can't satisfy the local-extension parent-walk for
    // protein_quantification's `swfc:protein_abundance_matrix` →
    // `data:2976`).
    let mut goal_modifiers: BTreeMap<String, String> = BTreeMap::new();
    if let Some(kind) = archetype_obj.goal_kind_hint.as_ref() {
        goal_modifiers.insert("kind".to_string(), kind.clone());
    }
    let goal = GoalSpec {
        edam_data: archetype_obj.goal_data.clone(),
        edam_format: archetype_obj.goal_format.clone(),
        modifiers: goal_modifiers,
        source_prose: Some(format!("--build from archetype {}", archetype_obj.id)),
        confidence: 1.0,
    };
    // Cross-omics archetypes declare their two-or-more constituent
    // modalities under `cross_omics_modalities`; pass those through so
    // the composer's `find_match_cross_omics` set-equality matcher
    // selects this archetype and `resolve_inheritance` expands the
    // `compose:` directive. Single-modality archetypes fall through to
    // `[modality_hint or self.id]` as before so existing scenarios
    // (`bulk_rnaseq_de`, `gwas_coloc`, etc.) keep their current shapes.
    let modality_owned: Vec<String> = if !archetype_obj.cross_omics_modalities.is_empty() {
        archetype_obj.cross_omics_modalities.clone()
    } else {
        vec![archetype_obj
            .modality_hint
            .clone()
            .unwrap_or_else(|| archetype_obj.id.clone())]
    };
    let modalities: Vec<&str> = modality_owned.iter().map(|s| s.as_str()).collect();
    let output_compose = compose_with_version_and_modalities_full(
        &goal,
        &archetype_obj.project_class,
        &atoms,
        &archetypes,
        4,
        &modalities,
        None,
        // R1/R2 closure — bare CLI caller; no chat session to attribute
        // opaque observations to. Sink stays None so the engine falls
        // back to its log-only short-circuit.
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("v4 composer dispatch failed: {:?}", e))?;
    let dag = if let Some(workflow_dag) = output_compose.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(workflow_dag, &workflow_id)?
    } else {
        build_dag_from_composition(
            &output_compose.composition,
            &workflow_id,
            &BTreeMap::new(),
            &[],
        )?
    };

    // Build a minimal classification from archetype metadata.
    // For cross-omics archetypes the classification carries the primary
    // modality (first entry of `cross_omics_modalities`) and surfaces
    // the rest as `additional_modalities` so the emitted package's
    // intake/classification facts remain consistent with the composer
    // dispatch above.
    let primary_modality = modality_owned[0].clone();
    let additional_modalities = if archetype_obj.cross_omics_modalities.is_empty() {
        vec![]
    } else {
        archetype_obj
            .cross_omics_modalities
            .iter()
            .skip(1)
            .map(|m| scripps_workflow_core::classify::ModalityCandidate {
                modality: m.clone(),
                taxonomy_path: String::new(),
                edam_topic: String::new(),
                edam_operation: String::new(),
                confidence: 1.0,
                keyword_hits: 0,
            })
            .collect()
    };
    let clf = scripps_workflow_core::classify::ClassificationResult {
        modality: primary_modality,
        taxonomy_path: String::new(),
        domain: "computational biology".into(),
        workflow_description: archetype_obj.description.clone(),
        edam_topic: String::new(),
        edam_operation: String::new(),
        confidence: 1.0,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: format!("Built directly from archetype: {}", archetype),
        goal: Some(goal),
        archetype_id: Some(archetype_obj.id.clone()),
        additional_modalities,
        tie_candidates: vec![],
    };

    let policies_dir = config_root.join("downstream-policy");

    let out_path = Path::new(output);
    let compute_profiles_dir = config_root.join("compute-profiles");
    let compute_profiles_opt = if compute_profiles_dir.exists() {
        Some(compute_profiles_dir.as_path())
    } else {
        None
    };
    let intake_facts = scripps_workflow_core::intake_facts::IntakeFacts::from_classification(&clf);
    // Aggregate runtime prereqs from the archetype's baseline + composed atoms.
    let composed_atoms: Vec<_> = output_compose
        .composition
        .atoms
        .iter()
        .map(|ca| ca.atom.clone())
        .collect();
    let runtime_prereqs = scripps_workflow_core::runtime_prereqs::aggregate_archetype(
        &archetype_obj,
        &composed_atoms,
    );
    // Build the per-atom runtime-prereqs map alongside the union manifest so
    // the emitter writes one policies/atom-prereqs/<atom_id>.json per
    // buildable atom. Empty atoms (no install delta) are skipped at emit
    // time so the package surface stays minimal for legacy modalities.
    let per_atom_prereqs: std::collections::BTreeMap<
        String,
        scripps_workflow_core::runtime_prereqs::RuntimePrereqs,
    > = composed_atoms
        .iter()
        .map(|a| (a.id.clone(), a.runtime_packages.clone()))
        .collect();
    let preferred_container_str = archetype_obj
        .preferred_container
        .as_ref()
        .map(|c| c.image.clone());
    emit_package(&EmitConfig {
        output_dir: out_path,
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: compute_profiles_opt,
        intake_facts: Some(&intake_facts),
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: preferred_container_str.as_deref(),
        runtime_prereqs: Some(&runtime_prereqs),
        per_atom_runtime_prereqs: Some(&per_atom_prereqs),
    })?;

    // Opt-in BCO emit alongside the package.
    let auto_bco = archetype_obj.project_class == "clinical_trial";
    if emit_bco_flag || auto_bco {
        match emit_bco(&output_compose.composition, &intake_facts, out_path) {
            Ok(()) => {
                println!(
                    "  {} bco.json emitted (IEEE 2791-2020 BioCompute Object)",
                    "✓".green()
                );
            }
            Err(err) => {
                eprintln!("[bco] emit failed: {}; continuing without BCO", err);
            }
        }
    }

    println!("{} Package emitted → {}", "✓".green().bold(), output.cyan());
    println!(
        "  Run: scripps-workflow-harness --package {} --agent claude",
        output
    );
    Ok(())
}

fn run_dag(package: &str, dot: bool) -> Result<()> {
    use colored::Colorize;
    use scripps_workflow_core::dag::{TaskState, DAG};
    use std::path::Path;

    let wf_path = Path::new(package).join("WORKFLOW.json");
    let content = std::fs::read_to_string(&wf_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", wf_path.display(), e))?;
    let dag: DAG = serde_json::from_str(&content)?;

    if dot {
        println!("{}", dag_to_dot(&dag));
        return Ok(());
    }

    let (completed, ready, blocked, pending) = dag.progress();
    println!("{}", format!("Workflow: {}", dag.workflow_id).bold());
    println!(
        "  {} completed  {} ready  {} blocked  {} pending\n",
        completed.to_string().green(),
        ready.to_string().blue(),
        blocked.to_string().red(),
        pending.to_string().white()
    );

    for (id, task) in &dag.tasks {
        let status = match &task.state {
            TaskState::Pending => "pending  ".white(),
            TaskState::Ready => "ready    ".blue().bold(),
            TaskState::Running { .. } => "running  ".yellow().bold(),
            TaskState::Completed { .. } => "completed".green(),
            TaskState::Failed { .. } => "failed   ".red().bold(),
            TaskState::Blocked { .. } => "BLOCKED  ".red().bold(),
        };
        let deps = if task.depends_on.is_empty() {
            String::new()
        } else {
            format!(" ← [{}]", task.depends_on.join(", "))
        };
        println!("  {} {}{}", status, id.as_str().cyan(), deps.dimmed());
    }
    Ok(())
}

fn run_intake(input: &str, output: &str, config: &str, emit_bco_flag: bool) -> Result<()> {
    use colored::Colorize;
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    use scripps_workflow_core::atom_registry::AtomRegistry;
    use scripps_workflow_core::bco::emit_bco;
    use scripps_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use scripps_workflow_core::classify::Classifier;
    use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
    use scripps_workflow_core::emitter::{emit_package, EmitConfig};
    use scripps_workflow_core::goal_spec::GoalSpec;
    use scripps_workflow_core::project_class::ProjectClass;
    use std::collections::BTreeMap;
    use std::path::Path;

    let config_path = Path::new(config);
    let keywords_path = config_path.join("modality-keywords.yaml");
    let policies_dir = config_path.join("downstream-policy");

    let intake_text = std::fs::read_to_string(input)
        .map_err(|e| anyhow::anyhow!("Cannot read intake file '{}': {}", input, e))?;

    println!(
        "{}",
        "Scripps Workflow Compiler — intake mode".bold().cyan()
    );
    println!("  Input: {}", input.cyan());

    let classifier = Classifier::load(&keywords_path)
        .map_err(|e| anyhow::anyhow!("Cannot load classifier config: {}", e))?;

    let clf = classifier.classify(&intake_text);

    println!(
        "\n  {} {} [confidence: {} — {:.0}%]",
        "Classified:".dimmed(),
        clf.modality.cyan().bold(),
        clf.confidence_label,
        clf.confidence * 100.0
    );
    if !clf.edam_topic.is_empty() {
        println!(
            "  {} · {}",
            clf.edam_topic.dimmed(),
            clf.edam_operation.dimmed()
        );
    }
    if !clf.organisms.is_empty() {
        for org in &clf.organisms {
            println!("  Organism: {} (taxon:{})", org.name.cyan(), org.taxon_id);
        }
    }
    if !clf.data_sources.is_empty() {
        let ds_display: Vec<String> = clf
            .data_sources
            .iter()
            .map(|d| d.accession.clone())
            .collect();
        println!("  Data sources: {}", ds_display.join(", ").cyan());
    }

    // Phase B4 — route through the v4 composer. The legacy
    // taxonomy YAML loader was retired with `config/stage-taxonomies/`.
    let atoms = AtomRegistry::load_from_dir(&config_path.join("stage-atoms"))?;
    let archetypes = ArchetypeRegistry::load_from_dir(&config_path.join("archetypes"))?;

    // Synthesize a goal from the classifier; on bare-modality intake
    // fall back to a non-archetype goal so the modality-only path
    // takes over.
    let goal = clf.goal.clone().unwrap_or_else(|| GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("intake bare-modality fallback".into()),
        confidence: 0.0,
    });
    // Determine project class via keyword classifier (best-effort).
    let project_class_path = config_path.join("project-class-keywords.yaml");
    let project_class = if project_class_path.exists() {
        match scripps_workflow_core::classify::load_project_class_keywords(&project_class_path) {
            Ok(cfg) => scripps_workflow_core::classify::classify_project_class(&intake_text, &cfg),
            Err(_) => ProjectClass::Bioinformatics,
        }
    } else {
        ProjectClass::Bioinformatics
    };
    let project_class_str = match project_class {
        ProjectClass::Bioinformatics => "bioinformatics",
        ProjectClass::ClinicalTrial => "clinical_trial",
        ProjectClass::TimeSeriesForecast => "time_series_forecast",
    };
    let modalities: Vec<&str> = std::iter::once(clf.modality.as_str())
        .chain(
            clf.additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();

    let workflow_id = workflow_id_from_intake(&intake_text);
    let output_compose = compose_with_version_and_modalities_full(
        &goal,
        project_class_str,
        &atoms,
        &archetypes,
        4,
        &modalities,
        None,
        // R1/R2 closure — `intake` is the deterministic CLI entry; no
        // chat session attached, so the opaque sink stays None.
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("v4 composer dispatch failed: {:?}", e))?;
    let dag = if let Some(workflow_dag) = output_compose.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(workflow_dag, &workflow_id)?
    } else {
        build_dag_from_composition(
            &output_compose.composition,
            &workflow_id,
            &BTreeMap::new(),
            &[],
        )?
    };

    let (_, ready, blocked, pending) = dag.progress();
    println!(
        "\n  Workflow: {} tasks ({} ready, {} pending, {} blocked)",
        dag.tasks.len().to_string().bold(),
        ready.to_string().blue(),
        pending.to_string().white(),
        blocked.to_string().red()
    );

    let archetype_id = output_compose
        .composition
        .matched_archetype
        .clone()
        .unwrap_or_else(|| clf.modality.clone());
    let archetype_obj = archetypes
        .iter()
        .find(|(id, _)| id.as_str() == archetype_id.as_str());
    let preferred_container_str =
        archetype_obj.and_then(|(_, a)| a.preferred_container.as_ref().map(|c| c.image.clone()));
    let archetype_description = archetype_obj
        .map(|(_, a)| a.description.clone())
        .unwrap_or_else(|| clf.workflow_description.clone());

    // Build full classification result with archetype metadata.
    let full_clf = scripps_workflow_core::classify::ClassificationResult {
        domain: "computational biology".into(),
        workflow_description: archetype_description,
        archetype_id: Some(archetype_id),
        ..clf
    };

    let out_path = Path::new(output);
    let compute_profiles_dir = config_path.join("compute-profiles");
    let compute_profiles_opt = if compute_profiles_dir.exists() {
        Some(compute_profiles_dir.as_path())
    } else {
        None
    };
    let intake_facts =
        scripps_workflow_core::intake_facts::IntakeFacts::from_classification(&full_clf);
    let composed_atoms: Vec<_> = output_compose
        .composition
        .atoms
        .iter()
        .map(|ca| ca.atom.clone())
        .collect();
    let runtime_prereqs = if let Some((_, a)) = archetype_obj {
        scripps_workflow_core::runtime_prereqs::aggregate_archetype(a, &composed_atoms)
    } else {
        scripps_workflow_core::runtime_prereqs::RuntimePrereqs::new()
    };
    // Per-atom prereqs map for the SWFC_PER_TASK_IMAGES path. See
    // sibling site in the chat-driven intake above for the wiring rationale.
    let per_atom_prereqs: std::collections::BTreeMap<
        String,
        scripps_workflow_core::runtime_prereqs::RuntimePrereqs,
    > = composed_atoms
        .iter()
        .map(|a| (a.id.clone(), a.runtime_packages.clone()))
        .collect();
    emit_package(&EmitConfig {
        output_dir: out_path,
        dag: &dag,
        classification: &full_clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: compute_profiles_opt,
        intake_facts: Some(&intake_facts),
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: preferred_container_str.as_deref(),
        runtime_prereqs: Some(&runtime_prereqs),
        per_atom_runtime_prereqs: Some(&per_atom_prereqs),
    })?;

    // Opt-in BCO emit alongside the package, plus
    // auto-on for clinical_trial project class.
    let auto_bco = project_class == ProjectClass::ClinicalTrial;
    if emit_bco_flag || auto_bco {
        match emit_bco(&output_compose.composition, &intake_facts, out_path) {
            Ok(()) => {
                println!(
                    "  {} bco.json emitted (IEEE 2791-2020 BioCompute Object)",
                    "✓".green()
                );
            }
            Err(err) => {
                eprintln!("[bco] emit failed: {}; continuing without BCO", err);
            }
        }
    }

    println!(
        "\n{} Package emitted → {}",
        "✓".green().bold(),
        output.cyan()
    );
    println!(
        "  Run: scripps-workflow-harness --package {} --agent claude\n",
        output
    );
    Ok(())
}

fn run_serve(port: u16) -> Result<()> {
    use colored::Colorize;
    use std::process::Command;

    // Locate the server binary — try PATH first, then cargo run
    let server_bin = "scripps-workflow-server";
    let status = Command::new(server_bin)
        .arg("--port")
        .arg(port.to_string())
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(anyhow::anyhow!("Server exited with non-zero status")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Genuine PATH miss — fall back to `cargo run`.
            // Permission-denied / EAGAIN / other spawn failures fall
            // through to the next arm so we surface them instead of
            // masking with a misleading "not found" message and then
            // spawning a parallel build.
            println!(
                "{}",
                format!("'{}' not found in PATH — using cargo run", server_bin).yellow()
            );
            let s = Command::new("cargo")
                .args([
                    "run",
                    "-p",
                    "scripps-workflow-server",
                    "--",
                    "--port",
                    &port.to_string(),
                ])
                .status()?;
            if s.success() {
                Ok(())
            } else {
                Err(anyhow::anyhow!("cargo run failed"))
            }
        }
        Err(e) => Err(anyhow::anyhow!(
            "failed to spawn '{}': {} (kind: {:?})",
            server_bin,
            e,
            e.kind()
        )),
    }
}

/// Workflow id derived from the SHA-256 of the input intake text.
/// Two runs of the same intake produce the same `workflow_id`,
/// which is a hard dependency for the byte-reproducibility contract
/// (CLAUDE.md "deterministic output").
fn workflow_id_from_intake(intake: &str) -> String {
    workflow_id_from_bytes(intake.as_bytes())
}

fn workflow_id_from_bytes(bytes: &[u8]) -> String {
    // 12 hex chars = 48 bits — collision-resistant enough for a
    // build-tag without bloating the WORKFLOW.json.
    let hex = scripps_workflow_core::hash_utils::sha256_short(bytes, 12);
    format!("workflow-{}", hex)
}
