//! FNV-1a 64, the digest `<ns>.files.tsv` records. The constants are those of
//! `ruff_charon::store::fnv64`, restated here because this crate does not
//! depend on `ruff_charon`; the known answers below pin them.

#[must_use]
pub fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The `{:016x}` spelling the store files use.
#[must_use]
pub fn hex(digest: u64) -> String {
    format!("{digest:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_answers() {
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hex(fnv64(b"foobar")), "85944171f73967e8");
    }
}
