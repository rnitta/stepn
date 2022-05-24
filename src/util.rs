use tokio::process::Command;

pub(crate) fn pad_with_trailing_space(width: usize, src: &str) -> String {
    let mut ret: String = src.to_string();
    let size_diff = width - ret.len();
    for _ in 0..size_diff {
        ret.push(' ');
    }
    ret
}

pub trait MethodChain {
    fn then(self: &mut Self, f: Box<dyn Fn(&mut Self) -> &mut Self>) -> &mut Self;
}

impl MethodChain for Command {
    fn then(self: &mut Self, f: Box<dyn Fn(&mut Self) -> &mut Self>) -> &mut Self {
        f(self)
    }
}
