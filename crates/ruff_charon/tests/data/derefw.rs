//! Writes through dereferenced places, for `tests/effects.rs`.
//! `through_arg` writes twice through argument-rooted derefs, `through_local`
//! writes once through a local reference, and `plain` assigns only to locals.

pub struct S {
    pub f: i32,
}

pub fn through_arg(p: &mut i32, s: &mut S) {
    *p = 1;
    s.f = 2;
}

pub fn through_local(v: i32) -> i32 {
    let mut x = v;
    let r = &mut x;
    *r = 3;
    x
}

pub fn plain(a: i32) -> i32 {
    let y = a;
    y
}
