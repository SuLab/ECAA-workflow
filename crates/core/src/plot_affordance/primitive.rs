use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Universal renderer primitives — operate on physical structure
/// alone, no semantic-type assumptions. Every `StructuralFallback`
/// resolves to one of these; the Python module
/// `lib/plotting/primitives/structural.py` is the authoritative
/// implementation, with R parity at
/// `lib/plotting_r/primitives/structural.R`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GenericPrimitive {
    /// Heatmap of any 2D numeric array; auto-rasterizes above 50k cells.
    MatrixOverview,
    /// Histogram + KDE of any 1D numeric.
    Distribution,
    /// Bar + frequency table of any categorical.
    CategoricalSummary,
    /// Small-multiples scatter for any tabular numeric ≤ 8 columns.
    Pairs,
    /// Plain text + value card for any single-value output.
    ScalarCard,
}

impl GenericPrimitive {
    /// Renderer module.
    pub fn renderer_module(&self) -> &'static str {
        "runtime.plotting.primitives.structural"
    }

    /// Figure id.
    pub fn figure_id(&self) -> &'static str {
        match self {
            Self::MatrixOverview => "__structural_matrix_overview",
            Self::Distribution => "__structural_distribution",
            Self::CategoricalSummary => "__structural_categorical_summary",
            Self::Pairs => "__structural_pairs",
            Self::ScalarCard => "__structural_scalar_card",
        }
    }
}
