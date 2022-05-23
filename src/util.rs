pub(crate) fn pad_with_trailing_space(width: usize, src: &str) -> String {
    let mut ret: String = src.to_string();
    let size_diff = width - ret.len();
    for _ in 0..size_diff {
        ret.push(' ');
    }
    ret
}
