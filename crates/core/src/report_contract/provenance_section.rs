//! Deterministic renderer for the system-owned DATA-PROVENANCE section.
//!
//! Where a run's data actually came from is already recorded by the package
//! itself: `runtime/outputs/<stage>/per_accession_summary.json` (accession,
//! bibliographic record, originating software package), the stage's
//! `cohort_manifest.tsv` (sample rows), that stage's `result.json` deviation
//! note, and the SME's `runtime/inputs.json` registrations. The reporting
//! agent narrates over those files but has, in a real deposit, ASSERTED a data
//! source that contradicted every one of them — an "SME-supplied local copy"
//! that was never registered (`runtime/inputs.json` absent; the stage actually
//! read a Bioconductor data package) cited to the wrong journal (NEJM, where
//! the package's own record says PLOS ONE with a matching DOI + PMID).
//!
//! This module renders the provenance statement deterministically from the
//! package's own metadata and (see the harness finalize step) injects it into
//! the terminal report, so the authoritative source statement is system-owned
//! rather than agent-authored. [`crate::reporting_invariants`]'s `RP-PROV` is
//! the backstop that faults an agent sentence contradicting it.
//!
//! Modality-agnostic: every field is read BY NAME out of the package's own
//! acquisition metadata. Nothing here knows what a gene, a read, or a count
//! matrix is.
//!
//! Deterministic: fixed stage order (directory names sorted), fixed row order,
//! no wall clock, no random ids. Re-rendering the same package yields the same
//! bytes.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

/// Marker opening the system-owned data-provenance block in a report.
pub const DATA_PROVENANCE_START: &str = "<!-- ECAA:data-provenance START -->";
/// Marker closing the system-owned data-provenance block in a report.
pub const DATA_PROVENANCE_END: &str = "<!-- ECAA:data-provenance END -->";

/// Longest markdown table cell the renderer emits; longer recorded free text
/// (a deviation note) is truncated with an ellipsis so one verbose note can't
/// dominate the report.
const CELL_MAX_CHARS: usize = 600;

/// Where the bytes a stage analyzed actually came from, as recorded by the
/// package — never inferred from narrative prose.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SourceKind {
    /// An SME-registered local path / uploaded file set (`runtime/inputs.json`).
    LocalPath,
    /// A public repository accession fetched by the acquisition stage.
    PublicAccession,
    /// A software distribution that ships the data (an R/Bioconductor data
    /// package, a Python dataset package, a bundled reference release).
    SoftwarePackage,
    /// The package records an acquisition stage but no resolvable source.
    /// Default: an unpopulated record has not yet resolved its source, and
    /// "unresolvable" is the honest reading of that state.
    #[default]
    Unrecorded,
}

impl SourceKind {
    /// Reader-facing label, phrased so it directly contradicts the wrong
    /// assertion a report might otherwise make.
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::LocalPath => "SME-registered local input",
            SourceKind::PublicAccession => "public repository accession",
            SourceKind::SoftwarePackage => {
                "software package (not a direct repository download, and not a local file \
                 registered by the SME)"
            }
            SourceKind::Unrecorded => "not recorded in the package's acquisition metadata",
        }
    }
}

/// One SME data registration read from `runtime/inputs.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SmeRegisteredInput {
    /// `input_id` as registered.
    pub input_id: String,
    /// Human label the SME gave the registration.
    pub label: String,
    /// `local_path` / `uploaded_files`.
    pub kind: String,
    /// Registered root path.
    pub root_path: String,
    /// Number of files enumerated under the registration.
    pub n_files: usize,
}

/// The package's own provenance record for one acquired dataset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataProvenanceRecord {
    /// Acquisition stage/task id the record was read from.
    pub stage_id: String,
    /// Repository accession, when one is recorded.
    pub accession: Option<String>,
    /// Study/dataset title.
    pub study_title: Option<String>,
    /// Journal name exactly as the package records it.
    pub journal: Option<String>,
    /// Publication year.
    pub year: Option<String>,
    /// DOI exactly as the package records it.
    pub doi: Option<String>,
    /// PubMed id exactly as the package records it.
    pub pmid: Option<String>,
    /// First author, used by `RP-PROV` to anchor a citation match.
    pub first_author: Option<String>,
    /// Originating software distribution (e.g. `airway (Bioconductor)`).
    pub source_package: Option<String>,
    /// Version of [`Self::source_package`].
    pub package_version: Option<String>,
    /// Organism, when recorded.
    pub organism: Option<String>,
    /// Sample count.
    pub n_samples: Option<u64>,
    /// `true` when [`Self::n_samples`] was counted from the stage's cohort
    /// manifest rather than declared in the accession summary.
    pub n_samples_from_manifest: bool,
    /// The `provenance` note recorded alongside the accession summary.
    pub acquisition_note: Option<String>,
    /// A recorded deviation from the requested source (the stage's
    /// `result.json::provenance_note` / `source_deviation`) — the field that
    /// records "the SME path was absent, so X was used instead".
    pub deviation: Option<String>,
    /// Classification derived from the fields above plus whether the SME
    /// registered any local input.
    pub source_kind: SourceKind,
}

impl DataProvenanceRecord {
    /// `true` when the record carries at least one substantive field; an
    /// accession summary that parsed but is empty has nothing to render.
    fn is_substantive(&self) -> bool {
        self.accession.is_some()
            || self.source_package.is_some()
            || self.study_title.is_some()
            || self.journal.is_some()
            || self.doi.is_some()
            || self.pmid.is_some()
            || self.n_samples.is_some()
    }
}

/// Everything the package itself records about where its data came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataProvenance {
    /// One record per acquired dataset, ordered by acquisition stage id then
    /// by position within that stage's summary.
    pub records: Vec<DataProvenanceRecord>,
    /// SME registrations from `runtime/inputs.json`, in file order.
    pub sme_inputs: Vec<SmeRegisteredInput>,
    /// Whether `runtime/inputs.json` exists at all — an absent file and an
    /// empty array both mean "no SME-registered local data", but they are
    /// distinguishable states worth reporting.
    pub inputs_json_present: bool,
}

impl DataProvenance {
    /// `true` when the package records no local registration — the state that
    /// makes an "supplied by the SME from a local copy" assertion false.
    pub fn has_sme_registered_inputs(&self) -> bool {
        !self.sme_inputs.is_empty()
    }

    /// `true` when there is nothing at all to render.
    fn is_empty(&self) -> bool {
        self.records.is_empty() && self.sme_inputs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// Read a JSON file, `None` when missing or unparseable.
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A trimmed non-empty string field; a JSON number renders as its digits so a
/// PMID recorded as `24926665` and as `"24926665"` collapse to one form.
fn str_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A bibliographic field, whether nested under `publication` (the shape the
/// acquisition prompt asks for) or flattened at the top level.
fn bib_field(v: &Value, key: &str) -> Option<String> {
    v.get("publication")
        .and_then(|p| str_field(p, key))
        .or_else(|| str_field(v, key))
}

/// A non-negative integer field, accepting the integral-float and
/// numeric-string encodings numpy / pandas / jsonlite routinely emit.
fn u64_field(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().replace(',', "").parse().ok(),
        _ => None,
    }
}

/// Every accession object a `per_accession_summary.json` document carries.
/// Tolerant of the three shapes agents emit: a bare object, an array of
/// objects, and `{"accessions": [...]}`.
fn accession_objects(doc: &Value) -> Vec<&Value> {
    match doc {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => {
            if let Some(Value::Array(items)) = doc.get("accessions") {
                items.iter().collect()
            } else {
                vec![doc]
            }
        }
        _ => Vec::new(),
    }
}

/// Count the data rows of a stage's cohort manifest, when one is present.
/// Used only as the fallback sample count; a declared `n_samples` wins.
fn cohort_manifest_rows(stage_dir: &Path) -> Option<u64> {
    for name in ["cohort_manifest.tsv", "cohort_manifest.csv"] {
        let path = stage_dir.join(name);
        if !path.exists() {
            continue;
        }
        if let Ok((_headers, rows)) = crate::report_contract::assemble::read_table(&path) {
            return Some(rows.len() as u64);
        }
    }
    None
}

/// The stage's own recorded deviation from the requested source.
fn recorded_deviation(stage_dir: &Path) -> Option<String> {
    let result = read_json(&stage_dir.join("result.json"))?;
    for key in [
        "provenance_note",
        "source_deviation",
        "provenance_deviation",
    ] {
        if let Some(note) = str_field(&result, key) {
            return Some(note);
        }
    }
    None
}

/// Read `runtime/inputs.json`. Returns `(present, registrations)`; an absent
/// or unparseable file is `(false, [])`, an empty array `(true, [])`.
fn read_sme_inputs(package_root: &Path) -> (bool, Vec<SmeRegisteredInput>) {
    let path = package_root.join("runtime").join("inputs.json");
    if !path.is_file() {
        return (false, Vec::new());
    }
    let Some(doc) = read_json(&path) else {
        return (true, Vec::new());
    };
    let items: Vec<&Value> = match &doc {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => match doc.get("inputs") {
            Some(Value::Array(a)) => a.iter().collect(),
            _ => vec![&doc],
        },
        _ => Vec::new(),
    };
    let registrations = items
        .into_iter()
        .filter_map(|item| {
            let root_path = str_field(item, "root_path").unwrap_or_default();
            let input_id = str_field(item, "input_id").unwrap_or_default();
            let label = str_field(item, "label").unwrap_or_default();
            let kind = str_field(item, "kind").unwrap_or_default();
            if root_path.is_empty() && input_id.is_empty() && label.is_empty() {
                return None;
            }
            let n_files = item
                .get("files")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            Some(SmeRegisteredInput {
                input_id,
                label,
                kind,
                root_path,
                n_files,
            })
        })
        .collect();
    (true, registrations)
}

/// Collect every provenance fact the package records about its own data.
///
/// Deterministic: acquisition stages are visited in sorted directory-name
/// order (a `BTreeMap`), records within a stage in document order, and no
/// value is read from the environment or the clock.
pub fn collect_data_provenance(package_root: &Path) -> DataProvenance {
    let (inputs_json_present, sme_inputs) = read_sme_inputs(package_root);
    let outputs = package_root.join("runtime").join("outputs");

    // Sorted stage id -> parsed accession summary. A stage without one is not
    // an acquisition stage as far as this renderer is concerned.
    let mut summaries: BTreeMap<String, Value> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(&outputs) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(stage_id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if let Some(doc) = read_json(&entry.path().join("per_accession_summary.json")) {
                summaries.insert(stage_id, doc);
            }
        }
    }

    let has_sme = !sme_inputs.is_empty();
    let mut records = Vec::new();
    for (stage_id, doc) in &summaries {
        let stage_dir = outputs.join(stage_id);
        let deviation = recorded_deviation(&stage_dir);
        let manifest_rows = cohort_manifest_rows(&stage_dir);
        for obj in accession_objects(doc) {
            let n_samples_declared = u64_field(obj, "n_samples");
            let source_package = str_field(obj, "source_package");
            let accession = str_field(obj, "accession");
            let source_kind = if source_package.is_some() {
                SourceKind::SoftwarePackage
            } else if has_sme {
                SourceKind::LocalPath
            } else if accession.is_some() {
                SourceKind::PublicAccession
            } else {
                SourceKind::Unrecorded
            };
            let record = DataProvenanceRecord {
                stage_id: stage_id.clone(),
                accession,
                study_title: str_field(obj, "study_title").or_else(|| str_field(obj, "title")),
                journal: bib_field(obj, "journal"),
                year: bib_field(obj, "year"),
                doi: bib_field(obj, "doi"),
                pmid: bib_field(obj, "pmid"),
                first_author: bib_field(obj, "first_author"),
                source_package,
                package_version: str_field(obj, "package_version"),
                organism: str_field(obj, "organism"),
                n_samples: n_samples_declared.or(manifest_rows),
                n_samples_from_manifest: n_samples_declared.is_none() && manifest_rows.is_some(),
                acquisition_note: str_field(obj, "provenance"),
                deviation: deviation.clone(),
                source_kind,
            };
            if record.is_substantive() {
                records.push(record);
            }
        }
    }

    DataProvenance {
        records,
        sme_inputs,
        inputs_json_present,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Escape and bound a value so it renders as one markdown table cell.
fn cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        let c = if ch.is_whitespace() { ' ' } else { ch };
        if c == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        if c == '|' {
            out.push('\\');
        }
        out.push(c);
    }
    let trimmed = out.trim();
    if trimmed.chars().count() > CELL_MAX_CHARS {
        let head: String = trimmed.chars().take(CELL_MAX_CHARS).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

fn push_row(body: &mut String, field: &str, value: &str) {
    body.push_str(&format!("| {field} | {} |\n", cell(value)));
}

fn push_opt_row(body: &mut String, field: &str, value: Option<&String>) {
    if let Some(v) = value {
        push_row(body, field, v);
    }
}

fn render_sme_inputs(prov: &DataProvenance, body: &mut String) {
    body.push_str("\n### SME-registered data inputs\n\n");
    if prov.sme_inputs.is_empty() {
        let state = if prov.inputs_json_present {
            "`runtime/inputs.json` is present but registers no input"
        } else {
            "`runtime/inputs.json` is absent"
        };
        body.push_str(&format!(
            "None. {state}, so no local path or uploaded file set was registered for this \
             run — this analysis read no locally registered data.\n"
        ));
        return;
    }
    body.push_str("| Input | Kind | Registered root | Files |\n");
    body.push_str("| --- | --- | --- | --- |\n");
    for input in &prov.sme_inputs {
        let name = if input.label.is_empty() {
            input.input_id.clone()
        } else if input.input_id.is_empty() {
            input.label.clone()
        } else {
            format!("{} (`{}`)", input.label, input.input_id)
        };
        body.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            cell(&name),
            cell(&input.kind),
            cell(&input.root_path),
            input.n_files
        ));
    }
}

fn render_record(record: &DataProvenanceRecord, body: &mut String) {
    let heading = match &record.accession {
        Some(a) => format!("{} — {a}", record.stage_id),
        None => record.stage_id.clone(),
    };
    body.push_str(&format!("\n### {heading}\n\n"));
    body.push_str("| Field | Value |\n");
    body.push_str("| --- | --- |\n");
    push_row(body, "Source kind", record.source_kind.label());
    if let Some(pkg) = &record.source_package {
        let value = match &record.package_version {
            Some(v) => format!("{pkg} v{v}"),
            None => pkg.clone(),
        };
        push_row(body, "Software package", &value);
    }
    push_opt_row(body, "Accession", record.accession.as_ref());
    push_opt_row(body, "Study", record.study_title.as_ref());
    if let Some(journal) = &record.journal {
        let value = match &record.year {
            Some(y) => format!("{journal} ({y})"),
            None => journal.clone(),
        };
        push_row(body, "Journal", &value);
    }
    push_opt_row(body, "DOI", record.doi.as_ref());
    push_opt_row(body, "PMID", record.pmid.as_ref());
    push_opt_row(body, "First author", record.first_author.as_ref());
    push_opt_row(body, "Organism", record.organism.as_ref());
    if let Some(n) = record.n_samples {
        let value = if record.n_samples_from_manifest {
            format!("{n} (counted from `cohort_manifest`)")
        } else {
            n.to_string()
        };
        push_row(body, "Samples", &value);
    }
    push_opt_row(
        body,
        "Recorded acquisition provenance",
        record.acquisition_note.as_ref(),
    );
    push_opt_row(body, "Recorded source deviation", record.deviation.as_ref());
}

/// Render `prov` as the marker-delimited markdown provenance section, or
/// `None` when the package records nothing. Pure and deterministic.
pub fn render_provenance_section(prov: &DataProvenance) -> Option<String> {
    if prov.is_empty() {
        return None;
    }
    let mut body = String::new();
    render_sme_inputs(prov, &mut body);
    for record in &prov.records {
        render_record(record, &mut body);
    }
    Some(format!(
        "{DATA_PROVENANCE_START}\n\
         ## Data provenance\n\n\
         _System-generated from this package's own acquisition metadata \
         (`runtime/outputs/<stage>/per_accession_summary.json`, the stage cohort manifest and \
         `result.json`, and `runtime/inputs.json`). This block, not the surrounding narrative, \
         is the authoritative statement of where this run's data came from._\n\
         {body}{DATA_PROVENANCE_END}\n"
    ))
}

/// Render the data-provenance section for `package_root`, or `None` when the
/// package records no acquisition provenance at all.
pub fn provenance_section(package_root: &Path) -> Option<String> {
    render_provenance_section(&collect_data_provenance(package_root))
}

/// Insert `block` (a [`provenance_section`] output) into `report_text`. If a
/// marker block already exists it is REPLACED (idempotent re-injection);
/// otherwise `block` is appended. Pure; never touches the filesystem.
///
/// Mirrors [`crate::report_contract::inject_full_tables`], which cannot be
/// reused because it hardcodes the full-table marker pair.
pub fn inject_provenance_section(report_text: &str, block: &str) -> String {
    if let (Some(s), Some(e)) = (
        report_text.find(DATA_PROVENANCE_START),
        report_text.find(DATA_PROVENANCE_END),
    ) {
        let end = e + DATA_PROVENANCE_END.len();
        let mut out = String::with_capacity(report_text.len() + block.len());
        out.push_str(&report_text[..s]);
        out.push_str(block.trim_end_matches('\n'));
        out.push_str(&report_text[end..]);
        return out;
    }
    let mut out = report_text.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(block);
    out
}

/// Remove the system-owned provenance block from `report_text`, returning the
/// agent-authored remainder. Narrative scanners (`RP-PROV`) must not read the
/// system's own block as an agent assertion.
pub fn strip_provenance_section(report_text: &str) -> String {
    let mut out = String::with_capacity(report_text.len());
    let mut rest = report_text;
    while let (Some(s), Some(e)) = (
        rest.find(DATA_PROVENANCE_START),
        rest.find(DATA_PROVENANCE_END),
    ) {
        if e < s {
            break;
        }
        out.push_str(&rest[..s]);
        rest = &rest[e + DATA_PROVENANCE_END.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The exact accession record the himes deposit shipped: a public
    /// accession whose bytes actually came from a Bioconductor data package,
    /// with the substitution recorded in the stage's `result.json`.
    fn himes_summary() -> String {
        serde_json::json!({
            "accession": "GSE52778",
            "study_title": "RNA-Seq Transcriptome Profiling Identifies CRISPLD2",
            "publication": {
                "pmid": "24926665",
                "doi": "10.1371/journal.pone.0099625",
                "journal": "PLOS ONE",
                "year": 2014,
                "first_author": "Himes BE"
            },
            "source_package": "airway (Bioconductor)",
            "package_version": "1.30.0",
            "organism": "Homo sapiens",
            "n_samples": 8,
            "provenance": "Retrieved from Bioconductor airway package (v1.30.0)."
        })
        .to_string()
    }

    #[test]
    fn provenance_section_renders_from_accession_metadata() {
        // -- case 1: a public accession, no software package, no SME inputs --
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/per_accession_summary.json",
            &serde_json::json!({
                "accession": "PXD012345",
                "study_title": "A proteome survey",
                "publication": { "journal": "Nature Methods", "year": 2021,
                                 "doi": "10.1038/s41592-021-01111-1", "pmid": "34000001" },
                "n_samples": 12
            })
            .to_string(),
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(
            s.starts_with(DATA_PROVENANCE_START) && s.trim_end().ends_with(DATA_PROVENANCE_END),
            "block is marker-wrapped: {s}"
        );
        assert!(
            s.contains("public repository accession") && s.contains("PXD012345"),
            "public-accession case must state the accession and its kind: {s}"
        );
        assert!(
            s.contains("Nature Methods (2021)")
                && s.contains("10.1038/s41592-021-01111-1")
                && s.contains("34000001"),
            "bibliographic fields come from the package's own record: {s}"
        );
        assert!(
            s.contains("`runtime/inputs.json` is absent"),
            "with no registration the section must SAY no SME data was supplied: {s}"
        );
        assert_eq!(
            provenance_section(tmp.path()).as_deref(),
            Some(s.as_str()),
            "deterministic: re-rendering the same package yields identical bytes"
        );

        // -- case 2: an SME-registered local path -----------------------------
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/inputs.json",
            &serde_json::json!([{
                "input_id": "sme-counts",
                "label": "Local count matrix",
                "kind": "local_path",
                "root_path": "/data/study-42",
                "files": [{ "relpath": "counts.tsv" }, { "relpath": "samples.csv" }]
            }])
            .to_string(),
        );
        write(
            tmp.path(),
            "runtime/outputs/data_import/per_accession_summary.json",
            &serde_json::json!({ "accession": "LOCAL-42", "n_samples": 6 }).to_string(),
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(
            s.contains("SME-registered local input") && s.contains("/data/study-42"),
            "local-path case must name the registered root: {s}"
        );
        assert!(
            s.contains("| 2 |"),
            "the registration's file count is rendered: {s}"
        );

        // -- case 3: a recorded SUBSTITUTION (requested source unavailable) ---
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/per_accession_summary.json",
            &himes_summary(),
        );
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/result.json",
            &serde_json::json!({
                "provenance_note": "SME-specified local path /home/a/himes-inputs was absent. \
                                    Data retrieved from the Bioconductor airway package instead."
            })
            .to_string(),
        );
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/cohort_manifest.tsv",
            "sample_id\tcondition\nS1\ta\nS2\tb\n",
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(
            s.contains("software package (not a direct repository download"),
            "a source_package record must classify as a software package: {s}"
        );
        assert!(
            s.contains("airway (Bioconductor) v1.30.0"),
            "the substituting package + version are named: {s}"
        );
        assert!(
            s.contains("Recorded source deviation") && s.contains("was absent"),
            "the recorded substitution must be surfaced: {s}"
        );
        assert!(
            s.contains("PLOS ONE (2014)") && !s.contains("NEJM"),
            "the journal comes from the package record, never from prose: {s}"
        );
        assert!(
            s.contains("| Samples | 8 |"),
            "a DECLARED n_samples wins over the 2-row cohort manifest: {s}"
        );
    }

    #[test]
    fn sample_count_falls_back_to_the_cohort_manifest() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/per_accession_summary.json",
            &serde_json::json!({ "accession": "GSE1" }).to_string(),
        );
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/cohort_manifest.tsv",
            "sample_id\tcondition\nS1\ta\nS2\tb\nS3\ta\n",
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(
            s.contains("| Samples | 3 (counted from `cohort_manifest`) |"),
            "manifest-derived counts are labeled as such: {s}"
        );
    }

    #[test]
    fn non_omics_record_renders_identically() {
        // Modality-agnostic: nothing in the renderer knows about genes/reads.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/data_import/per_accession_summary.json",
            &serde_json::json!({
                "accession": "USGS-08155300",
                "study_title": "Streamflow gauge record",
                "n_samples": 3650
            })
            .to_string(),
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(
            s.contains("USGS-08155300") && s.contains("| Samples | 3650 |"),
            "{s}"
        );
    }

    #[test]
    fn a_package_with_no_recorded_provenance_renders_nothing() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/reporting")).unwrap();
        assert!(provenance_section(tmp.path()).is_none());
        // An accession summary that parses but is empty is equally vacuous.
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/per_accession_summary.json",
            "{}",
        );
        assert!(provenance_section(tmp.path()).is_none());
    }

    #[test]
    fn multiple_accessions_and_stages_render_in_a_fixed_order() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/zz_second_acquisition/per_accession_summary.json",
            &serde_json::json!({ "accession": "GSE999" }).to_string(),
        );
        write(
            tmp.path(),
            "runtime/outputs/aa_first_acquisition/per_accession_summary.json",
            &serde_json::json!([{ "accession": "GSE111" }, { "accession": "GSE222" }]).to_string(),
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        let (i1, i2, i3) = (
            s.find("GSE111").unwrap(),
            s.find("GSE222").unwrap(),
            s.find("GSE999").unwrap(),
        );
        assert!(
            i1 < i2 && i2 < i3,
            "sorted by stage, then document order: {s}"
        );
    }

    #[test]
    fn inject_appends_then_replaces_idempotently() {
        let report = "# Report\n\nNarrative.\n";
        let block = format!("{DATA_PROVENANCE_START}\nA\n{DATA_PROVENANCE_END}\n");
        let once = inject_provenance_section(report, &block);
        assert!(once.starts_with("# Report") && once.contains("\nA\n"));
        assert_eq!(once.matches(DATA_PROVENANCE_START).count(), 1);
        let twice = inject_provenance_section(&once, &block);
        assert_eq!(
            twice, once,
            "idempotent: re-injecting the same block is a no-op"
        );
        let replaced = inject_provenance_section(
            &once,
            &format!("{DATA_PROVENANCE_START}\nB\n{DATA_PROVENANCE_END}\n"),
        );
        assert!(replaced.contains("\nB\n") && !replaced.contains("\nA\n"));
        assert_eq!(replaced.matches(DATA_PROVENANCE_START).count(), 1);
    }

    #[test]
    fn strip_removes_only_the_system_block() {
        let text = format!(
            "before\n{DATA_PROVENANCE_START}\nsupplied by the SME\n{DATA_PROVENANCE_END}\nafter\n"
        );
        let stripped = strip_provenance_section(&text);
        assert!(stripped.contains("before") && stripped.contains("after"));
        assert!(
            !stripped.contains("supplied by the SME"),
            "the system block must not be scanned as an agent assertion: {stripped}"
        );
        assert_eq!(strip_provenance_section("plain"), "plain");
    }

    #[test]
    fn a_pipe_in_recorded_free_text_cannot_break_the_table() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/outputs/data_acquisition/per_accession_summary.json",
            &serde_json::json!({
                "accession": "GSE1",
                "provenance": "piped | through\ntwo lines"
            })
            .to_string(),
        );
        let s = provenance_section(tmp.path()).expect("section rendered");
        assert!(s.contains("piped \\| through two lines"), "{s}");
    }
}
