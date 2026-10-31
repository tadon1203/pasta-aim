//! Order statistics over the timing samples.

/// The index of the `percent`-th percentile in `len` sorted values, rounded half up.
/// `len` must be at least 1.
pub(crate) fn percentile_index(len: usize, percent: usize) -> usize {
    ((len - 1) * percent + 50) / 100
}

/// The `percent`-th percentile of `values`, selected without a full sort.
/// `None` when there are no values. `values` is reordered.
pub(crate) fn percentile(values: &mut [f64], percent: usize) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let index = percentile_index(values.len(), percent);
    Some(*values.select_nth_unstable_by(index, f64::total_cmp).1)
}
