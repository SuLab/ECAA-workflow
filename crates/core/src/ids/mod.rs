//! Strongly-typed string IDs (see ADR 0040).
//!
//! Each ID is a `#[repr(transparent)]` newtype over `Arc<str>`. Clone =
//! atomic refcount bump (no allocation). `#[serde(transparent)]` keeps
//! wire format bit-identical to a bare string. `#[ts(type = "string")]`
//! keeps generated TypeScript bindings as plain `string`.
//!
//! `impl Borrow<str>` lets `BTreeMap<TaskId, _>::get(&str)` work without
//! constructing a TaskId — important for hot lookup paths.

#![allow(dead_code)]

use std::borrow::Borrow;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// TaskId
// ---------------------------------------------------------------------------

/// Stable, semantically-named handle for a task in the workflow DAG.
///
/// Backed by `Arc<str>` — clones are atomic refcount bumps. The wire
/// format is a bare string; the TypeScript type is `string`.
#[repr(transparent)]
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct TaskId(Arc<str>);

impl TaskId {
    /// Construct a TaskId from any string-like input.
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }
    /// Return the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// True when the id is the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self(Arc::from(""))
    }
}

impl From<&TaskId> for String {
    fn from(id: &TaskId) -> String {
        id.0.to_string()
    }
}

impl fmt::Debug for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}
impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl Borrow<str> for TaskId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TaskId {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}
impl From<String> for TaskId {
    fn from(s: String) -> Self {
        Self(Arc::from(s.into_boxed_str()))
    }
}
impl From<&String> for TaskId {
    fn from(s: &String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}
impl From<TaskId> for String {
    fn from(id: TaskId) -> String {
        id.0.to_string()
    }
}

impl PartialEq<str> for TaskId {
    fn eq(&self, other: &str) -> bool {
        self.0.as_ref() == other
    }
}
impl PartialEq<String> for TaskId {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other.as_str()
    }
}

impl schemars::JsonSchema for TaskId {
    fn schema_name() -> String {
        "TaskId".to_string()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        String::json_schema(gen)
    }
}

// ---------------------------------------------------------------------------
// StageId
// ---------------------------------------------------------------------------

/// Stable handle for a stage in the workflow taxonomy. Backed by `Arc<str>`.
#[repr(transparent)]
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct StageId(Arc<str>);

impl StageId {
    /// Construct a StageId from any string-like input.
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }
    /// Return the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for StageId {
    fn default() -> Self {
        Self(Arc::from(""))
    }
}

impl fmt::Debug for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}
impl fmt::Display for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl AsRef<str> for StageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl Borrow<str> for StageId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StageId {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}
impl From<String> for StageId {
    fn from(s: String) -> Self {
        Self(Arc::from(s.into_boxed_str()))
    }
}
impl From<&String> for StageId {
    fn from(s: &String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}
impl From<StageId> for String {
    fn from(id: StageId) -> String {
        id.0.to_string()
    }
}
impl From<&StageId> for String {
    fn from(id: &StageId) -> String {
        id.0.to_string()
    }
}

impl PartialEq<str> for StageId {
    fn eq(&self, other: &str) -> bool {
        self.0.as_ref() == other
    }
}
impl PartialEq<String> for StageId {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other.as_str()
    }
}

impl schemars::JsonSchema for StageId {
    fn schema_name() -> String {
        "StageId".to_string()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        String::json_schema(gen)
    }
}

// ---------------------------------------------------------------------------
// AtomId
// ---------------------------------------------------------------------------

/// Stable handle for an atom in the operation × input-type × output-type catalog.
///
/// `Arc<str>`-backed for cheap clone across the planner's BFS frontier.
/// `Borrow<str>` enables `BTreeMap<AtomId, V>::get("literal")` without
/// an extra allocation.
#[repr(transparent)]
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct AtomId(Arc<str>);

impl AtomId {
    /// Construct an AtomId from any string-like input.
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }
    /// Return the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AtomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}
impl fmt::Display for AtomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl AsRef<str> for AtomId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
impl Borrow<str> for AtomId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AtomId {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}
impl From<String> for AtomId {
    fn from(s: String) -> Self {
        Self(Arc::from(s.into_boxed_str()))
    }
}
impl From<&String> for AtomId {
    fn from(s: &String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}
impl From<AtomId> for String {
    fn from(id: AtomId) -> String {
        id.0.to_string()
    }
}
impl From<&AtomId> for String {
    fn from(id: &AtomId) -> String {
        id.0.to_string()
    }
}

impl PartialEq<str> for AtomId {
    fn eq(&self, other: &str) -> bool {
        self.0.as_ref() == other
    }
}
impl PartialEq<String> for AtomId {
    fn eq(&self, other: &String) -> bool {
        self.0.as_ref() == other.as_str()
    }
}

impl schemars::JsonSchema for AtomId {
    fn schema_name() -> String {
        "AtomId".to_string()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        String::json_schema(gen)
    }
}
