//! Canonical carrier for SME/intake-requested methods, keyed by the bare
//! discover axis (e.g. `variant_calling`, `alignment`). Both capture
//! sources — the CLI classifier (`Classifier::classify -> methods_specified`)
//! and chat `set_intake_method` (`Session::intake_methods`) — fold into the
//! SAME `PreferredMethods` map so the composer treats them identically.
//!
//! Determinism: `BTreeMap` throughout (sorted iteration); `normalize_method_id`
//! is a pure string transform with no locale/random/time input.

use std::collections::BTreeMap;

use crate::builder::IntakeMethods;
use crate::classify::MethodSpec;

/// Provenance of a preferred method (fixed-literal rationale, never a timestamp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredMethodSource {
    /// SME named it via `set_intake_method` (chat path).
    Sme,
    /// Classifier matched a `method_keywords` entry in intake prose (CLI path).
    Classifier,
}

impl PreferredMethodSource {
    /// Fixed-literal rationale string stamped into `spec_preferred_methods`.
    pub fn rationale(self) -> &'static str {
        match self {
            PreferredMethodSource::Sme => "SME-requested method",
            PreferredMethodSource::Classifier => {
                "named in intake prose (classifier method keyword)"
            }
        }
    }
}

/// One requested method on one axis.
#[derive(Debug, Clone)]
pub struct PreferredMethod {
    /// Normalized, rankable id (the `spec_preferred_methods` map key).
    pub id: String,
    /// Source provenance → fixed rationale string.
    pub source: PreferredMethodSource,
}

/// Axis (bare discover stem) → preferred method. SME wins on collision.
#[derive(Debug, Clone, Default)]
pub struct PreferredMethods(pub BTreeMap<String, PreferredMethod>);

impl PreferredMethods {
    /// Empty.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// True when no axis carries a preference (the common case; guards
    /// byte-identical emit for non-requested intake).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Look up the preference for a bare discover axis.
    pub fn get(&self, axis: &str) -> Option<&PreferredMethod> {
        self.0.get(axis)
    }

    /// Fold CLI classifier `MethodSpec`s into the map. `MethodSpec.stage`
    /// is the bare axis. First-write-wins within this source.
    pub fn from_method_specs(specs: &[MethodSpec]) -> Self {
        let mut out = BTreeMap::new();
        for m in specs {
            if m.method.trim().is_empty() {
                continue;
            }
            let axis = strip_discover_prefix(&m.stage);
            out.entry(axis).or_insert_with(|| PreferredMethod {
                id: normalize_method_id(&m.method),
                source: PreferredMethodSource::Classifier,
            });
        }
        Self(out)
    }

    /// Fold chat `IntakeMethods` into the map. Key is the stage id
    /// (bare axis or `discover_<axis>`); strip the prefix so both shapes
    /// land on the same key. Skips empty method strings.
    pub fn from_intake_methods(methods: &IntakeMethods) -> Self {
        let mut out = BTreeMap::new();
        for (stage, res) in methods {
            if res.method.trim().is_empty() {
                continue;
            }
            let axis = strip_discover_prefix(stage);
            out.insert(
                axis,
                PreferredMethod {
                    id: normalize_method_id(&res.method),
                    source: PreferredMethodSource::Sme,
                },
            );
        }
        Self(out)
    }

    /// Merge `other` into `self`; `other`'s entries win on key collision.
    /// Used to let the SME (chat) source override the classifier source.
    pub fn merge_overriding(&mut self, other: PreferredMethods) {
        for (k, v) in other.0 {
            self.0.insert(k, v);
        }
    }
}

/// Strip a leading `discover_` from a stage/axis string.
fn strip_discover_prefix(stage: &str) -> String {
    stage.strip_prefix("discover_").unwrap_or(stage).to_string()
}

/// Normalize free-form method prose to a stable, rankable id.
///
/// Lowercase, trim, take the leading token-run before the first run of
/// whitespace (drops trailing flags/versions like `"LoFreq 2.1 --call-indels"`
/// → `"lofreq"`; `"GATK HaplotypeCaller"` is handled by the alias table),
/// strip non-alphanumeric trailing punctuation (`"LoFreq*"` → `"lofreq"`),
/// then collapse internal `-` runs to a single `_`. Finally map through a
/// small alias table that mirrors candidate_tools id conventions. Unknown
/// tokens pass through verbatim-normalized (VERBATIM PASSTHROUGH) so
/// out-of-catalog tools (lofreq, octopus, clair3, …) get a stable id.
pub fn normalize_method_id(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    // Alias table FIRST on the full lowered string so multi-token vendor
    // names ("gatk haplotypecaller") resolve before token-splitting.
    if let Some(alias) = alias_lookup(&lowered) {
        return alias.to_string();
    }
    // Take the leading run before the first whitespace gap (drops flags/versions).
    let head = lowered.split_whitespace().next().unwrap_or("");
    // Strip trailing non-alphanumeric (e.g. "lofreq*" -> "lofreq").
    let head = head.trim_end_matches(|c: char| !c.is_alphanumeric());
    // Collapse internal separators to single `_`.
    let collapsed: String = head.replace('-', "_");
    // Re-check the alias table on the collapsed single token.
    if let Some(alias) = alias_lookup(&collapsed) {
        return alias.to_string();
    }
    collapsed
}

/// Map common display/vendor strings to their candidate_tools id.
/// Conservative: only well-known aliases; everything else passes through.
fn alias_lookup(s: &str) -> Option<&'static str> {
    match s {
        "gatk haplotypecaller" | "gatk" | "haplotypecaller" | "haplotype caller" => {
            Some("haplotypecaller")
        }
        "deepvariant" | "deep variant" | "deep_variant" => Some("deepvariant"),
        "mutect2" | "gatk mutect2" | "mutect 2" => Some("mutect2"),
        "strelka2" | "strelka" | "strelka 2" => Some("strelka2"),
        "lofreq" | "lo freq" | "lo_freq" => Some("lofreq"),
        "cell ranger" | "cellranger" | "cell_ranger" => Some("cellranger"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_known_and_unknown() {
        assert_eq!(normalize_method_id("LoFreq"), "lofreq");
        assert_eq!(normalize_method_id("LoFreq*"), "lofreq");
        assert_eq!(
            normalize_method_id("GATK HaplotypeCaller"),
            "haplotypecaller"
        );
        assert_eq!(normalize_method_id("DeepVariant"), "deepvariant");
        assert_eq!(normalize_method_id("Cell Ranger"), "cellranger");
        // Unknown out-of-catalog tool: verbatim passthrough.
        assert_eq!(normalize_method_id("Octopus"), "octopus");
        assert_eq!(normalize_method_id("clair3 --model r941"), "clair3");
        // Idempotent.
        assert_eq!(normalize_method_id("lofreq"), "lofreq");
    }

    #[test]
    fn strip_discover_prefix_both_shapes() {
        assert_eq!(
            strip_discover_prefix("discover_variant_calling"),
            "variant_calling"
        );
        assert_eq!(strip_discover_prefix("variant_calling"), "variant_calling");
    }

    #[test]
    fn merge_sme_wins() {
        let mut clf = PreferredMethods::from_method_specs(&[MethodSpec {
            stage: "variant_calling".into(),
            method: "GATK HaplotypeCaller".into(),
        }]);
        let mut sme_map = std::collections::BTreeMap::new();
        sme_map.insert(
            "variant_calling".to_string(),
            crate::builder::IntakeResolution::new("LoFreq"),
        );
        let sme = PreferredMethods::from_intake_methods(&sme_map);
        clf.merge_overriding(sme);
        assert_eq!(clf.get("variant_calling").unwrap().id, "lofreq");
    }

    #[test]
    fn empty_sources_are_empty() {
        assert!(PreferredMethods::new().is_empty());
        assert!(PreferredMethods::from_method_specs(&[]).is_empty());
        assert!(PreferredMethods::from_intake_methods(&BTreeMap::new()).is_empty());
        // A blank method string is skipped (no phantom axis).
        assert!(PreferredMethods::from_method_specs(&[MethodSpec {
            stage: "variant_calling".into(),
            method: "   ".into(),
        }])
        .is_empty());
    }
}
