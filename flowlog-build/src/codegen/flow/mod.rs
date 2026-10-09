//! Flow analysis: per-stratum transformation and aggregation codegen.

pub(super) mod head_layout;
mod join_layout;
pub(super) mod non_recursive;
pub(super) mod recursive;
pub(super) mod transformation;
