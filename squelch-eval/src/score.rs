//! Per-field agreement between a candidate decision and gold (Step C).
use crate::case::Case;
use crate::run::ResultRow;
use std::collections::BTreeMap;

/// The `grade` map written onto each result row. Empty until the Step C
/// scorer lands; `report` recomputes every metric from the raw rows anyway.
pub fn grade(_case: &Case, _row: &ResultRow) -> BTreeMap<String, f64> {
    BTreeMap::new()
}
