pub(crate) fn pad_with_trailing_space(width: usize, src: &str) -> String {
    format!("{:<width$}", src, width = width)
}

pub(crate) fn compute_label_width(names: impl Iterator<Item = impl AsRef<str>>) -> usize {
    names.map(|n| n.as_ref().len()).max().unwrap_or(10).max(5)
}
