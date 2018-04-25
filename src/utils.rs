// TODO: Remove when Rust will get the ? syntax for Option.
#[macro_export]
macro_rules! tryopt {
    ($e:expr) => {
        match $e {
            Some(s) => s,
            None => return None,
        }
    };
}
