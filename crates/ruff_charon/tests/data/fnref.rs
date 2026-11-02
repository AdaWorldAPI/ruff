//! A function used only through a function pointer, an unused private
//! function, and a call through a function-pointer argument.
fn helper(x: u32) -> u32 {
    x + 1
}
fn unused_private() -> u32 {
    0
}
pub fn take() -> fn(u32) -> u32 {
    helper
}
pub fn apply(f: fn(u32) -> u32) -> u32 {
    f(1)
}
