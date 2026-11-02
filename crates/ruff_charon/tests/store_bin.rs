//! Tests for the binary rows format: the digest vectors, the round trip, and
//! one refusal test per `ReadError` the reader promises.

use ruff_charon::store_bin::{
    BinRow, CODEC9_MAX, HEADER_LEN, NO_ID, Prov, ReadError, WriteError, digest, read, tag, write,
};

// ---- helpers ---------------------------------------------------------------

fn row(
    pred: &str,
    subj: &str,
    obj: &str,
    site: Option<(&str, usize, usize)>,
    fun: Option<u32>,
) -> BinRow {
    BinRow {
        predicate: pred.into(),
        subject: subj.into(),
        object: obj.into(),
        grade: "CHARON_RESOLVED".into(),
        site: site.map(|(f, s, e)| (f.into(), s, e)),
        fun,
    }
}

fn prov() -> Prov {
    Prov {
        charon_version: Some("0.1.2".into()),
        charon_commit: Some("abc123".into()),
        llbc_schema_version: Some("7".into()),
        llbc_digest: None,
        rustc_commit: Some("def456".into()),
        target_config: None,
        source_revision: Some("rev".into()),
    }
}

fn sample() -> Vec<BinRow> {
    vec![
        row(
            "DEFINES",
            "krate::a::f",
            "krate::a",
            Some(("src/a.rs", 0, 10)),
            Some(0),
        ),
        row(
            "CALLS_RESOLVED",
            "krate::a::f",
            "krate::b::g",
            Some(("src/a.rs", 4, 8)),
            Some(0),
        ),
        row(
            "MAY_DISPATCH",
            "krate::b::g",
            "Trait::run via dyn",
            Some(("src/b.rs", 1, 2)),
            Some(1),
        ),
        row("UNRESOLVED_CALL", "krate::b::g", "<dyn>", None, None),
        row(
            "DEFINES",
            "krate::b::\u{00fc}ber",
            "krate::b",
            Some(("src/b.rs", 3, 9)),
            Some(2),
        ),
    ]
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Recompute the digest field so a test can isolate a check that runs after it.
fn reseal(b: &mut [u8]) {
    let d = digest(&b[32..]);
    b[16..24].copy_from_slice(&d.to_le_bytes());
}

/// Offset of the directory entry for `t`.
fn entry(b: &[u8], t: u32) -> usize {
    let n = rd32(b, 44) as usize;
    (0..n)
        .map(|i| HEADER_LEN + 16 * i)
        .find(|&at| rd32(b, at) == t)
        .expect("tag in directory")
}

/// The bytes of section `t`.
fn section(b: &[u8], t: u32) -> &[u8] {
    let at = entry(b, t);
    let (off, len) = (rd32(b, at + 8) as usize, rd32(b, at + 12) as usize);
    &b[off..off + len]
}

fn files_rows(n: usize) -> Vec<BinRow> {
    let mut rows: Vec<BinRow> = (0..n)
        .map(|i| {
            row(
                "DEFINES",
                &format!("k::f{i}"),
                "k",
                Some((&format!("src/f{i}.rs"), i, i + 1)),
                None,
            )
        })
        .collect();
    rows.push(row("UNRESOLVED_CALL", "k::z", "<dyn>", None, None));
    rows
}

// ---- digest ----------------------------------------------------------------

/// Red if the mix, the tail handling or the length fold differs from lance-graph's.
#[test]
fn digest_matches_the_golden_vectors() {
    assert_eq!(digest(b""), 0xd0dd_3f66_b426_3ceb);
    assert_eq!(digest(b"a"), 0x9fc7_5a5c_9bce_51be);
    assert_eq!(digest(b"CHRNROWS digest golden v1"), 0xfe0a_b233_a400_27ce);
}

// ---- round trip ------------------------------------------------------------

/// Red if any row field, the partial flag or the provenance does not survive.
#[test]
fn a_round_trip_returns_every_row() {
    let rows = sample();
    assert!(rows.len() >= 5);
    assert!(rows.iter().any(|r| r.predicate == "MAY_DISPATCH"));
    assert!(rows.iter().any(|r| r.site.is_none()));
    assert!(rows.iter().any(|r| r.fun.is_none()));
    assert!(rows.iter().any(|r| !r.subject.is_ascii()));
    let p = vec![prov()];
    let bytes = write(&rows, true, &p).unwrap();
    let d = read(&bytes).unwrap();
    assert_eq!(d.rows, rows);
    assert!(d.partial);
    assert_eq!(d.prov, p);
    let bytes = write(&rows, false, &p).unwrap();
    assert!(!read(&bytes).unwrap().partial);
}

/// Red if any table is built in hash order.
#[test]
fn writing_is_deterministic() {
    let a = write(&sample(), false, &[prov()]).unwrap();
    let b = write(&sample(), false, &[prov()]).unwrap();
    assert_eq!(a, b);
}

/// Red if the writer accepts an empty provenance list.
#[test]
fn an_empty_prov_list_is_refused() {
    assert_eq!(write(&sample(), false, &[]), Err(WriteError::NoProv));
}

/// Red if a function index equal to the absent id is written.
#[test]
fn a_fundecl_equal_to_the_absent_id_is_refused() {
    let rows = vec![row("DEFINES", "k::a", "k", None, Some(NO_ID))];
    assert_eq!(
        write(&rows, false, &[prov()]),
        Err(WriteError::FunDeclTooLarge { value: NO_ID })
    );
}

// ---- F3: one refusal per variant -------------------------------------------

fn good() -> Vec<u8> {
    write(&sample(), false, &[prov()]).unwrap()
}

/// Red if a flipped body byte is not caught by the digest.
#[test]
fn a_flipped_body_byte_is_a_digest_error() {
    let mut b = good();
    let last = b.len() - 9;
    b[last] ^= 0x55;
    assert!(matches!(read(&b), Err(ReadError::Digest { .. })));
}

/// Red if a cut file is not caught by the length check.
#[test]
fn a_truncated_file_is_refused() {
    let b = good();
    assert!(matches!(
        read(&b[..b.len() - 8]),
        Err(ReadError::Truncated { .. })
    ));
    assert!(matches!(read(&b[..10]), Err(ReadError::Truncated { .. })));
}

/// Red if the magic is not checked.
#[test]
fn a_wrong_magic_is_refused() {
    let mut b = good();
    b[0] = b'X';
    assert_eq!(read(&b), Err(ReadError::Magic));
}

/// Red if a newer major version is read.
#[test]
fn a_newer_major_is_refused() {
    let mut b = good();
    b[8] += 1;
    assert_eq!(read(&b), Err(ReadError::Major(2)));
}

/// Red if a newer minor version is read.
#[test]
fn a_newer_minor_is_refused() {
    let mut b = good();
    b[10] += 1;
    assert_eq!(read(&b), Err(ReadError::Minor(1)));
}

/// Red if reserved header bytes may carry data.
#[test]
fn a_reserved_byte_is_refused() {
    let mut b = good();
    b[50] = 1;
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::Reserved));
}

/// Red if a missing required section is not named.
#[test]
fn a_missing_section_is_named() {
    let mut b = good();
    let n = rd32(&b, 44) as usize;
    let at = entry(&b, tag::SUBJ);
    let end = HEADER_LEN + 16 * n;
    b.copy_within(at + 16..end, at);
    b[end - 16..end].fill(0);
    wr32(&mut b, 44, (n - 1) as u32);
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::MissingSection { tag: tag::SUBJ }));
}

/// Red if a symbol id at the table size is dereferenced.
#[test]
fn a_symbol_id_past_the_table_is_refused() {
    let mut b = good();
    let n_syms = rd32(&b, 36);
    let at = entry(&b, tag::SUBJ);
    let off = rd32(&b, at + 8) as usize;
    wr32(&mut b, off, n_syms);
    reseal(&mut b);
    assert_eq!(
        read(&b),
        Err(ReadError::IdRange {
            tag: tag::SUBJ,
            row: 0
        })
    );
}

/// Red if a section may start outside the file.
#[test]
fn a_section_offset_past_the_file_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::END);
    let len = b.len() as u32;
    wr32(&mut b, at + 8, len + 8);
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::Section { tag: tag::END }));
}

/// Red if a section starts off an 8-byte boundary.
#[test]
fn a_misaligned_section_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::END);
    let off = rd32(&b, at + 8);
    wr32(&mut b, at + 8, off + 4);
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::Section { tag: tag::END }));
}

/// Red if an unknown must-understand section is skipped.
#[test]
fn an_unknown_required_tag_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::END);
    wr32(&mut b, at, 0x8000_00FF);
    reseal(&mut b);
    assert_eq!(
        read(&b),
        Err(ReadError::UnknownRequired { tag: 0x8000_00FF })
    );
}

/// Red if an unknown optional section (bit 31 clear) is refused rather than skipped.
#[test]
fn an_unknown_optional_tag_is_skipped() {
    let mut b = good();
    let n = rd32(&b, 44) as usize;
    let dir_end = HEADER_LEN + 16 * n;
    // Make room for one more directory entry; every section moves by 16.
    b.splice(dir_end..dir_end, [0u8; 16]);
    for i in 0..n {
        let at = HEADER_LEN + 16 * i;
        let off = rd32(&b, at + 8);
        wr32(&mut b, at + 8, off + 16);
    }
    let new_len = b.len() as u32;
    b[24..32].copy_from_slice(&u64::from(new_len).to_le_bytes());
    wr32(&mut b, 44, (n + 1) as u32);
    wr32(&mut b, dir_end, 0x0000_00FF);
    wr32(&mut b, dir_end + 8, new_len);
    reseal(&mut b);
    assert_eq!(read(&b).unwrap().rows, sample());
}

/// Red if a lane of the wrong length is accepted.
#[test]
fn a_short_lane_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::START);
    wr32(&mut b, at + 12, 4);
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::LaneLength { tag: tag::START }));
}

/// Red if invalid UTF-8 in a symbol label is accepted.
#[test]
fn invalid_utf8_in_a_symbol_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::SYM_BLOB);
    let off = rd32(&b, at + 8) as usize;
    b[off] = 0xFF;
    reseal(&mut b);
    assert_eq!(read(&b), Err(ReadError::Utf8 { tag: tag::SYM_BLOB }));
}

/// Red if an object-kind codeword outside 1..=3 is accepted.
#[test]
fn a_bad_object_kind_codeword_is_refused() {
    let mut b = good();
    let at = entry(&b, tag::OBJ_KIND_MAG);
    let off = rd32(&b, at + 8) as usize;
    b[off + 2] = 9;
    reseal(&mut b);
    assert_eq!(
        read(&b),
        Err(ReadError::Codeword {
            tag: tag::OBJ_KIND_MAG,
            row: 2
        })
    );
}

// ---- F6: codec9 -------------------------------------------------------------

/// Red if the file lane stops being codec9 at 511 files, or loses a row.
#[test]
fn five_hundred_eleven_files_use_codec9_and_round_trip() {
    let rows = files_rows(CODEC9_MAX);
    let b = write(&rows, false, &[prov()]).unwrap();
    let at = entry(&b, tag::FILE);
    assert_eq!(u16::from_le_bytes([b[at + 6], b[at + 7]]), 1, "enc CODEC9");
    assert_eq!(u16::from_le_bytes([b[at + 4], b[at + 5]]), 1, "u8 lane");
    let d = read(&b).unwrap();
    assert_eq!(d.rows, rows);
    assert!(
        d.rows.last().unwrap().site.is_none(),
        "codeword 0 is not file 0"
    );
}

/// Red if the 512th file does not switch the lane to raw u32, or loses a row.
#[test]
fn five_hundred_twelve_files_use_a_raw_lane_and_round_trip() {
    let rows = files_rows(CODEC9_MAX + 1);
    let b = write(&rows, false, &[prov()]).unwrap();
    let at = entry(&b, tag::FILE);
    assert_eq!(u16::from_le_bytes([b[at + 6], b[at + 7]]), 0, "enc RAW");
    assert_eq!(u16::from_le_bytes([b[at + 4], b[at + 5]]), 4, "u32 lane");
    assert!(
        (0..rd32(&b, 44) as usize).all(|i| rd32(&b, HEADER_LEN + 16 * i) != tag::FILE_PRES),
        "no FILE_PRES with a raw lane"
    );
    let d = read(&b).unwrap();
    assert_eq!(d.rows, rows);
    assert!(d.rows.last().unwrap().site.is_none(), "NO_ID is not file 0");
}

/// Red if codewords 255 and 256 are not split into magnitude byte and presence bit.
#[test]
fn the_codeword_split_is_magnitude_and_presence() {
    let rows: Vec<BinRow> = (0..300)
        .map(|i| row(&format!("P{i}"), "k::a", "k", None, None))
        .collect();
    let b = write(&rows, false, &[prov()]).unwrap();
    let mag = section(&b, tag::PRED_MAG);
    let pres = section(&b, tag::PRED_PRES);
    let word = |w: usize| u64::from_le_bytes(pres[w * 8..w * 8 + 8].try_into().unwrap());
    // Row 254 has codeword 255, row 255 has codeword 256, row 256 has 257.
    assert_eq!(mag[254], 255);
    assert_eq!(word(3) >> 62 & 1, 0);
    assert_eq!(mag[255], 0);
    assert_eq!(word(3) >> 63 & 1, 1);
    assert_eq!(mag[256], 1);
    assert_eq!(word(4) & 1, 1);
    assert_eq!(read(&b).unwrap().rows, rows);
}

/// Red if a predicate codebook past 511 entries is written.
#[test]
fn five_hundred_twelve_predicates_are_refused() {
    let rows: Vec<BinRow> = (0..=CODEC9_MAX)
        .map(|i| row(&format!("P{i}"), "k::a", "k", None, None))
        .collect();
    assert!(matches!(
        write(&rows, false, &[prov()]),
        Err(WriteError::CodebookTooLarge {
            section: "PRED_CB",
            len: 512
        })
    ));
    assert!(write(&rows[..CODEC9_MAX], false, &[prov()]).is_ok());
}

// ---- F9: provenance ----------------------------------------------------------

/// Red if the provenance is not covered by the digest.
#[test]
fn a_different_charon_commit_changes_the_digest() {
    let mut p2 = prov();
    p2.charon_commit = Some("abc124".into());
    let a = write(&sample(), false, &[prov()]).unwrap();
    let b = write(&sample(), false, &[p2]).unwrap();
    assert_ne!(a[16..24], b[16..24]);
}

/// Red if UNKNOWN (None) and the string "UNKNOWN" are written or read the same.
#[test]
fn unknown_is_distinct_from_the_string_unknown() {
    let mut p_none = prov();
    p_none.rustc_commit = None;
    let mut p_str = prov();
    p_str.rustc_commit = Some("UNKNOWN".into());
    let a = write(&sample(), false, &[p_none.clone()]).unwrap();
    let b = write(&sample(), false, &[p_str.clone()]).unwrap();
    assert_ne!(a, b);
    assert_eq!(read(&a).unwrap().prov, vec![p_none]);
    assert_eq!(read(&b).unwrap().prov, vec![p_str]);
}

// ---- F10 (writer side) ---------------------------------------------------------

/// Red if the function id is lost, or merged with the subject.
#[test]
fn fundecl_is_per_row_and_independent_of_the_subject() {
    let rows = vec![
        row("CALLS_RESOLVED", "k::a", "k::x", None, Some(7)),
        row("CALLS_RESOLVED", "k::b", "k::x", None, Some(7)),
        row("CALLS_RESOLVED", "k::b", "k::y", None, None),
    ];
    let b = write(&rows, false, &[prov()]).unwrap();
    let lane: Vec<u32> = section(&b, tag::FUNDECL)
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(lane, vec![7, 7, NO_ID]);
    let d = read(&b).unwrap();
    assert_eq!(d.rows, rows);
    assert_ne!(d.rows[0].subject, d.rows[1].subject);
    assert_eq!(d.rows[0].fun, d.rows[1].fun);
}
