//! Plot affordance IR — flexible plotting upgrade plan §1.
//!
//! Selects a publication-quality renderer for an output port via a
//! deterministic walk over a typed registry. Resolution variants
//! mirror the DAG composer's compatibility-proof pattern: every
//! affordance is proof-carrying, never boolean.

/// Affordance module.
pub mod affordance;
pub mod obligation;
/// Primitive module.
pub mod primitive;
pub mod promotion;
/// Registry module.
pub mod registry;
/// Safety module.
pub mod safety;
pub mod sandbox;
/// Selector module.
pub mod selector;
pub mod telemetry;

pub use affordance::{AffordanceProof, GeneratedReviewStatus, PlotAffordance};
pub use obligation::{check_all, check_atom, ObligationViolation};
pub use primitive::GenericPrimitive;
pub use promotion::{promote_renderer, PromotedRenderer, PromotionError, RendererPromotionRequest};
pub use registry::{PlotAffordanceRegistry, RegisteredAffordance, YamlPlotAffordanceRegistry};
pub use safety::PlotSafety;
pub use sandbox::{check_drafted_renderer, SandboxOutcome};
pub use selector::{resolve_affordance, PhysicalShape, PortDescriptor};
pub use telemetry::{AffordanceFallbackCounter, AffordanceFallbackRecord};
