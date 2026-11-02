//! The binary rows format `<crate>.rows.bin`: a pure writer and a verifying
//! reader for the lanes of lance-graph's
//! `.claude/plans/d-codegraph-0-binary-lanes-v1.md` §2 and §8.
//!
//! The format is columnar: one lane per field, each a section found through a
//! directory, every integer little-endian. Ids into the codebooks use the
//! `EpiLane9` codeword split (ndarray `src/hpc/codec9.rs`): a codeword `c` in
//! `1..=511` is a magnitude byte `c & 0xFF` plus a presence bit `c >> 8`, and
//! codeword 0 means absent. The header digest covers everything after the
//! first 32 bytes, so a reader refuses a damaged file before it decodes a lane.
//!
//! This module only encodes and decodes bytes. It does not touch the TSV store.
//! [`read`] rebuilds strings and is the debug/test path; the Codegraph reader in
//! lance-graph is separate and zero-copy.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// File magic, bytes 0..8.
pub const MAGIC: [u8; 8] = *b"CHRNROWS";
/// Format major version. A reader refuses any other.
pub const MAJOR: u16 = 1;
/// Format minor version. A reader refuses a file with a larger minor.
pub const MINOR: u16 = 0;
/// Length of the fixed header.
pub const HEADER_LEN: usize = 64;
/// An absent id, span, owner or function declaration.
pub const NO_ID: u32 = u32::MAX;
/// Header flag bit 0: the crate description was partial (Charon reported errors).
pub const FLAG_PARTIAL: u32 = 1;
/// Codebook entries addressable by a codec9 lane (codewords `1..=511`).
pub const CODEC9_MAX: usize = 511;

/// Section tags. Bit 31 means must-understand; every tag here has it set.
pub mod tag {
    /// Predicate magnitude lane.
    pub const PRED_MAG: u32 = 0x8000_0001;
    /// Predicate presence plane.
    pub const PRED_PRES: u32 = 0x8000_0002;
    /// Predicate codebook.
    pub const PRED_CB: u32 = 0x8000_0003;
    /// Grade magnitude lane.
    pub const GRADE_MAG: u32 = 0x8000_0004;
    /// Grade presence plane.
    pub const GRADE_PRES: u32 = 0x8000_0005;
    /// Grade codebook.
    pub const GRADE_CB: u32 = 0x8000_0006;
    /// File lane: codec9 magnitude bytes, or raw `u32` when there are more than 511 files.
    pub const FILE: u32 = 0x8000_0007;
    /// File presence plane, present only when `FILE` is codec9.
    pub const FILE_PRES: u32 = 0x8000_0008;
    /// File codebook.
    pub const FILE_CB: u32 = 0x8000_0009;
    /// Subject symbol ids.
    pub const SUBJ: u32 = 0x8000_000A;
    /// Object symbol ids.
    pub const OBJ: u32 = 0x8000_000B;
    /// Dispatch-target symbol ids (`MAY_DISPATCH` rows only).
    pub const MAYT: u32 = 0x8000_000C;
    /// Span starts.
    pub const START: u32 = 0x8000_000D;
    /// Span ends.
    pub const END: u32 = 0x8000_000E;
    /// Symbol blob offsets, `n_symbols + 1` entries.
    pub const SYM_OFF: u32 = 0x8000_000F;
    /// Symbol label bytes.
    pub const SYM_BLOB: u32 = 0x8000_0010;
    /// Per-symbol digest of the label.
    pub const SYM_HASH: u32 = 0x8000_0011;
    /// Per-symbol owner id.
    pub const SYM_OWNER: u32 = 0x8000_0012;
    /// Per-row Charon function declaration index.
    pub const FUNDECL: u32 = 0x8000_0013;
    /// Object-kind magnitude lane.
    pub const OBJ_KIND_MAG: u32 = 0x8000_0014;
    /// Object-kind presence plane.
    pub const OBJ_KIND_PRES: u32 = 0x8000_0015;
    /// Provenance record.
    pub const PROV: u32 = 0x8000_0016;
    /// Every tag a version-1 reader understands, in ascending order.
    pub const ALL: [u32; 22] = [
        PRED_MAG,
        PRED_PRES,
        PRED_CB,
        GRADE_MAG,
        GRADE_PRES,
        GRADE_CB,
        FILE,
        FILE_PRES,
        FILE_CB,
        SUBJ,
        OBJ,
        MAYT,
        START,
        END,
        SYM_OFF,
        SYM_BLOB,
        SYM_HASH,
        SYM_OWNER,
        FUNDECL,
        OBJ_KIND_MAG,
        OBJ_KIND_PRES,
        PROV,
    ];
}

const ENC_RAW: u16 = 0;
const ENC_CODEC9: u16 = 1;
const ENC_PRES: u16 = 2;
const ENC_KV: u16 = 3;

const KIND_SYMBOL: usize = 1;
const KIND_MAX: usize = 3;
const DIR_ENTRY_LEN: usize = 16;
const PROV_FIELDS: usize = 7;
const MAY_DISPATCH: &str = "MAY_DISPATCH";

// ---------------------------------------------------------------------------
// digest
// ---------------------------------------------------------------------------

const K: u64 = 0x517c_c1b7_2722_0a95;
const SEED: u64 = 0xcbf2_9ce4_8422_2325;

fn step(h: u64, w: u64) -> u64 {
    (h.rotate_left(5) ^ w).wrapping_mul(K)
}

fn len_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn fold(mut h: u64, bytes: &[u8]) -> u64 {
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        h = step(h, u64::from_le_bytes(*c));
    }
    if !rest.is_empty() {
        let mut w = [0u8; 8];
        w[..rest.len()].copy_from_slice(rest);
        // The tail length is folded in so "a" and "a\0" differ.
        h = step(h, u64::from_le_bytes(w) ^ (len_u64(rest.len()) << 56));
    }
    h
}

/// The change-detection digest, identical to lance-graph's
/// `lance-graph-codegraph` `artifact::digest`: eight bytes per step, the byte
/// length folded in last. Not cryptographic.
#[must_use]
pub fn digest(bytes: &[u8]) -> u64 {
    let h = fold(SEED, bytes);
    let h = fold(h, &len_u64(bytes.len()).to_le_bytes());
    h.rotate_left(26)
}

// ---------------------------------------------------------------------------
// types
// ---------------------------------------------------------------------------

/// One row, in TSV terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinRow {
    /// Relation name, e.g. `CALLS_RESOLVED`.
    pub predicate: String,
    /// Subject label.
    pub subject: String,
    /// Object label.
    pub object: String,
    /// Grade of the evidence.
    pub grade: String,
    /// `(file, start, end)`, or `None` when the row has no source site.
    pub site: Option<(String, usize, usize)>,
    /// Charon `FunDeclId` index of the function the row was found in; `None` is [`NO_ID`].
    pub fun: Option<u32>,
}

/// One provenance record. `None` is UNKNOWN and is distinct from every value,
/// including the string `"UNKNOWN"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prov {
    /// Charon version.
    pub charon_version: Option<String>,
    /// Charon commit.
    pub charon_commit: Option<String>,
    /// LLBC schema version.
    pub llbc_schema_version: Option<String>,
    /// Digest of the LLBC file.
    pub llbc_digest: Option<String>,
    /// rustc commit.
    pub rustc_commit: Option<String>,
    /// Target configuration.
    pub target_config: Option<String>,
    /// Revision of the analysed source.
    pub source_revision: Option<String>,
}

impl Prov {
    fn fields(&self) -> [&Option<String>; PROV_FIELDS] {
        [
            &self.charon_version,
            &self.charon_commit,
            &self.llbc_schema_version,
            &self.llbc_digest,
            &self.rustc_commit,
            &self.target_config,
            &self.source_revision,
        ]
    }
}

/// A file read back by [`read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The rows, in file order.
    pub rows: Vec<BinRow>,
    /// The partial flag from the header.
    pub partial: bool,
    /// The provenance records.
    pub prov: Vec<Prov>,
    /// The header digest, already verified against the body.
    pub digest: u64,
}

/// Why [`write`] refused its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// More symbols than a `u32` id can name.
    TooManySymbols,
    /// A span offset that does not fit below [`NO_ID`].
    SpanTooLarge {
        /// The offending offset.
        value: usize,
    },
    /// More rows than a `u32` row count can hold.
    TooManyRows,
    /// A codebook or blob that cannot be addressed.
    CodebookTooLarge {
        /// Which section.
        section: &'static str,
        /// Entry count, or byte length for a blob.
        len: usize,
    },
    /// No provenance record was given.
    NoProv,
    /// A function declaration index equal to [`NO_ID`].
    FunDeclTooLarge {
        /// The offending index.
        value: u32,
    },
}

/// Why [`read`] refused a file; one variant per refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Wrong magic.
    Magic,
    /// A major version this reader does not know.
    Major(u16),
    /// A minor version newer than this reader.
    Minor(u16),
    /// Non-zero reserved header bytes.
    Reserved,
    /// The file is shorter or longer than its header says.
    Truncated {
        /// Length recorded in the header (0 when the header itself is cut).
        header: u64,
        /// Actual byte length.
        actual: usize,
    },
    /// The body does not match the header digest.
    Digest {
        /// Digest recorded in the header.
        header: u64,
        /// Digest of the body.
        actual: u64,
    },
    /// The section directory does not fit in the file.
    Directory,
    /// A directory entry that is misaligned, out of order, overlapping, outside the file or duplicated.
    Section {
        /// The entry's tag.
        tag: u32,
    },
    /// An unknown section with the must-understand bit.
    UnknownRequired {
        /// The tag.
        tag: u32,
    },
    /// A required section is absent.
    MissingSection {
        /// The tag.
        tag: u32,
    },
    /// A section whose width, encoding or length is wrong.
    LaneLength {
        /// The tag.
        tag: u32,
    },
    /// An id at or past the end of the table it names.
    IdRange {
        /// The lane.
        tag: u32,
        /// The row (or symbol) index.
        row: usize,
    },
    /// A string that is not valid UTF-8, or an offset inside a character.
    Utf8 {
        /// The section.
        tag: u32,
    },
    /// A codeword that is not allowed in its lane.
    Codeword {
        /// The lane.
        tag: u32,
        /// The row.
        row: usize,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManySymbols => write!(f, "more symbols than a u32 id can name"),
            Self::SpanTooLarge { value } => write!(f, "span offset {value} does not fit a u32"),
            Self::TooManyRows => write!(f, "more rows than a u32 row count can hold"),
            Self::CodebookTooLarge { section, len } => {
                write!(f, "{section} is too large to address ({len})")
            }
            Self::NoProv => write!(f, "no provenance record"),
            Self::FunDeclTooLarge { value } => {
                write!(
                    f,
                    "function declaration index {value} collides with the absent id"
                )
            }
        }
    }
}

impl std::error::Error for WriteError {}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magic => write!(f, "bad magic"),
            Self::Major(v) => write!(f, "unsupported major version {v}"),
            Self::Minor(v) => write!(f, "unsupported minor version {v}"),
            Self::Reserved => write!(f, "reserved header bytes are not zero"),
            Self::Truncated { header, actual } => {
                write!(f, "file is {actual} bytes, header says {header}")
            }
            Self::Digest { header, actual } => {
                write!(f, "digest mismatch: header {header:#x}, body {actual:#x}")
            }
            Self::Directory => write!(f, "section directory does not fit in the file"),
            Self::Section { tag } => write!(f, "bad directory entry for section {tag:#x}"),
            Self::UnknownRequired { tag } => write!(f, "unknown required section {tag:#x}"),
            Self::MissingSection { tag } => write!(f, "missing section {tag:#x}"),
            Self::LaneLength { tag } => write!(f, "section {tag:#x} has the wrong shape"),
            Self::IdRange { tag, row } => write!(f, "id out of range in {tag:#x} at {row}"),
            Self::Utf8 { tag } => write!(f, "invalid UTF-8 in section {tag:#x}"),
            Self::Codeword { tag, row } => write!(f, "bad codeword in {tag:#x} at {row}"),
        }
    }
}

impl std::error::Error for ReadError {}

// ---------------------------------------------------------------------------
// writer
// ---------------------------------------------------------------------------

/// The dispatch target of a `MAY_DISPATCH` object, derived exactly as
/// lance-graph's `artifact::dispatch_target` does: the `Trait::method` part
/// before `" via "`, or the whole object when there is none.
fn dispatch_target(object: &str) -> &str {
    object.find(" via ").map_or(object, |i| &object[..i])
}

struct Symbols {
    labels: Vec<String>,
    index: HashMap<String, u32>,
}

impl Symbols {
    fn intern(&mut self, s: &str) -> Result<u32, WriteError> {
        if let Some(&id) = self.index.get(s) {
            return Ok(id);
        }
        let id = u32::try_from(self.labels.len())
            .ok()
            .filter(|&i| i < NO_ID)
            .ok_or(WriteError::TooManySymbols)?;
        self.labels.push(s.to_owned());
        self.index.insert(s.to_owned(), id);
        Ok(id)
    }
}

/// A first-seen string table; the entry's 0-based index plus one is its codeword.
#[derive(Default)]
struct Book {
    entries: Vec<String>,
    index: HashMap<String, usize>,
}

impl Book {
    fn id(&mut self, s: &str) -> usize {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.entries.len();
        self.entries.push(s.to_owned());
        self.index.insert(s.to_owned(), i);
        i
    }
}

/// A codec9 id lane: magnitude bytes plus a presence plane.
struct Lane9 {
    mag: Vec<u8>,
    pres: Vec<u64>,
}

impl Lane9 {
    fn new(n: usize) -> Self {
        Self {
            mag: vec![0; n],
            pres: vec![0; n.div_ceil(64)],
        }
    }

    /// Store codeword `c` (`0..=511`) at `row`. The caller guarantees `c <= 511`
    /// and `row` is in range.
    fn set(&mut self, row: usize, c: usize) {
        if let Some(m) = self.mag.get_mut(row) {
            *m = u8::try_from(c & 0xFF).unwrap_or(0);
        }
        if c >> 8 & 1 == 1
            && let Some(w) = self.pres.get_mut(row / 64)
        {
            *w |= 1 << (row % 64);
        }
    }
}

struct Section {
    tag: u32,
    width: u16,
    enc: u16,
    data: Vec<u8>,
}

fn pad8(v: &mut Vec<u8>) {
    while !v.len().is_multiple_of(8) {
        v.push(0);
    }
}

fn u32_lane(vals: &[u32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn u64_lane(vals: &[u64]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn too_large(section: &'static str, len: usize) -> WriteError {
    WriteError::CodebookTooLarge { section, len }
}

/// `u32 n`, `n + 1` offsets, the UTF-8 blob, zero-padded to 8.
fn kv_codebook(section: &'static str, entries: &[String]) -> Result<Vec<u8>, WriteError> {
    let n = u32::try_from(entries.len()).map_err(|_| too_large(section, entries.len()))?;
    let mut out = n.to_le_bytes().to_vec();
    let mut blob: Vec<u8> = Vec::new();
    out.extend_from_slice(&0u32.to_le_bytes());
    for e in entries {
        blob.extend_from_slice(e.as_bytes());
        let off = u32::try_from(blob.len()).map_err(|_| too_large(section, blob.len()))?;
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(&blob);
    pad8(&mut out);
    Ok(out)
}

fn prov_section(prov: &[Prov]) -> Result<Vec<u8>, WriteError> {
    let n = u32::try_from(prov.len()).map_err(|_| too_large("PROV", prov.len()))?;
    let mut out = n.to_le_bytes().to_vec();
    for rec in prov {
        for field in rec.fields() {
            match field {
                None => out.extend_from_slice(&NO_ID.to_le_bytes()),
                Some(s) => {
                    let len = u32::try_from(s.len())
                        .ok()
                        .filter(|&l| l < NO_ID)
                        .ok_or_else(|| too_large("PROV", s.len()))?;
                    out.extend_from_slice(&len.to_le_bytes());
                    out.extend_from_slice(s.as_bytes());
                }
            }
        }
    }
    pad8(&mut out);
    Ok(out)
}

fn span_u32(value: usize) -> Result<u32, WriteError> {
    u32::try_from(value)
        .ok()
        .filter(|&v| v < NO_ID)
        .ok_or(WriteError::SpanTooLarge { value })
}

fn lane9_sections(mag: u32, pres: u32, lane: &Lane9, out: &mut Vec<Section>) {
    out.push(Section {
        tag: mag,
        width: 1,
        enc: ENC_CODEC9,
        data: lane.mag.clone(),
    });
    out.push(Section {
        tag: pres,
        width: 8,
        enc: ENC_PRES,
        data: u64_lane(&lane.pres),
    });
}

fn raw_u32(tag: u32, vals: &[u32]) -> Section {
    Section {
        tag,
        width: 4,
        enc: ENC_RAW,
        data: u32_lane(vals),
    }
}

/// Encode `rows` as a `<crate>.rows.bin` file. The same input always gives the
/// same bytes: every table is built in first-seen order.
///
/// # Errors
/// A [`WriteError`] when a count, offset or codebook cannot be addressed, or
/// when `prov` is empty.
#[expect(clippy::too_many_lines, reason = "one pass that builds every lane")]
pub fn write(rows: &[BinRow], partial: bool, prov: &[Prov]) -> Result<Vec<u8>, WriteError> {
    if prov.is_empty() {
        return Err(WriteError::NoProv);
    }
    let n = rows.len();
    let n_rows = u32::try_from(n)
        .ok()
        .filter(|&v| v < NO_ID)
        .ok_or(WriteError::TooManyRows)?;

    let mut syms = Symbols {
        labels: Vec::new(),
        index: HashMap::new(),
    };
    let (mut preds, mut grades, mut files) = (Book::default(), Book::default(), Book::default());
    // Codebook ids per row; the file lane is built after the file count is known.
    let (mut pred_ids, mut grade_ids, mut file_ids) = (Vec::new(), Vec::new(), Vec::new());
    let (mut subj, mut obj, mut mayt) = (Vec::new(), Vec::new(), Vec::new());
    let (mut start, mut end, mut fundecl) = (Vec::new(), Vec::new(), Vec::new());

    for r in rows {
        pred_ids.push(preds.id(&r.predicate));
        grade_ids.push(grades.id(&r.grade));
        subj.push(syms.intern(&r.subject)?);
        obj.push(syms.intern(&r.object)?);
        mayt.push(if r.predicate == MAY_DISPATCH {
            syms.intern(dispatch_target(&r.object))?
        } else {
            NO_ID
        });
        if let Some((file, s, e)) = &r.site {
            file_ids.push(Some(files.id(file)));
            start.push(span_u32(*s)?);
            end.push(span_u32(*e)?);
        } else {
            file_ids.push(None);
            start.push(NO_ID);
            end.push(NO_ID);
        }
        fundecl.push(match r.fun {
            Some(v) if v == NO_ID => return Err(WriteError::FunDeclTooLarge { value: v }),
            Some(v) => v,
            None => NO_ID,
        });
    }

    // Owners are appended to the same table, so the loop runs to a fixpoint.
    let mut owners: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < syms.labels.len() {
        let prefix = syms.labels[i]
            .find("::")
            .map(|p| syms.labels[i][..p].to_owned());
        owners.push(match prefix {
            Some(p) => syms.intern(&p)?,
            None => NO_ID,
        });
        i += 1;
    }

    let n_symbols = u32::try_from(syms.labels.len()).map_err(|_| WriteError::TooManySymbols)?;
    let n_files = u32::try_from(files.entries.len())
        .map_err(|_| too_large("FILE_CB", files.entries.len()))?;
    for (name, book) in [("PRED_CB", &preds), ("GRADE_CB", &grades)] {
        if book.entries.len() > CODEC9_MAX {
            return Err(too_large(name, book.entries.len()));
        }
    }

    let mut sections: Vec<Section> = Vec::new();

    let (mut pl, mut gl) = (Lane9::new(n), Lane9::new(n));
    for row in 0..n {
        pl.set(row, pred_ids[row] + 1);
        gl.set(row, grade_ids[row] + 1);
    }
    lane9_sections(tag::PRED_MAG, tag::PRED_PRES, &pl, &mut sections);
    lane9_sections(tag::GRADE_MAG, tag::GRADE_PRES, &gl, &mut sections);
    sections.push(Section {
        tag: tag::PRED_CB,
        width: 1,
        enc: ENC_KV,
        data: kv_codebook("PRED_CB", &preds.entries)?,
    });
    sections.push(Section {
        tag: tag::GRADE_CB,
        width: 1,
        enc: ENC_KV,
        data: kv_codebook("GRADE_CB", &grades.entries)?,
    });

    if files.entries.len() <= CODEC9_MAX {
        let mut fl = Lane9::new(n);
        for (row, f) in file_ids.iter().enumerate() {
            fl.set(row, f.map_or(0, |id| id + 1));
        }
        lane9_sections(tag::FILE, tag::FILE_PRES, &fl, &mut sections);
    } else {
        let raw: Vec<u32> = file_ids
            .iter()
            .map(|f| f.and_then(|id| u32::try_from(id).ok()).unwrap_or(NO_ID))
            .collect();
        sections.push(raw_u32(tag::FILE, &raw));
    }
    sections.push(Section {
        tag: tag::FILE_CB,
        width: 1,
        enc: ENC_KV,
        data: kv_codebook("FILE_CB", &files.entries)?,
    });

    sections.push(raw_u32(tag::SUBJ, &subj));
    sections.push(raw_u32(tag::OBJ, &obj));
    sections.push(raw_u32(tag::MAYT, &mayt));
    sections.push(raw_u32(tag::START, &start));
    sections.push(raw_u32(tag::END, &end));

    let mut blob: Vec<u8> = Vec::new();
    let mut offs: Vec<u32> = vec![0];
    for l in &syms.labels {
        blob.extend_from_slice(l.as_bytes());
        offs.push(u32::try_from(blob.len()).map_err(|_| too_large("SYM_BLOB", blob.len()))?);
    }
    let hashes: Vec<u64> = syms.labels.iter().map(|l| digest(l.as_bytes())).collect();
    sections.push(raw_u32(tag::SYM_OFF, &offs));
    sections.push(Section {
        tag: tag::SYM_BLOB,
        width: 1,
        enc: ENC_RAW,
        data: blob,
    });
    sections.push(Section {
        tag: tag::SYM_HASH,
        width: 8,
        enc: ENC_RAW,
        data: u64_lane(&hashes),
    });
    sections.push(raw_u32(tag::SYM_OWNER, &owners));
    sections.push(raw_u32(tag::FUNDECL, &fundecl));

    // Every row of this chunk names a symbol; WORD and METHOD are reserved.
    let mut kinds = Lane9::new(n);
    for row in 0..n {
        kinds.set(row, KIND_SYMBOL);
    }
    lane9_sections(tag::OBJ_KIND_MAG, tag::OBJ_KIND_PRES, &kinds, &mut sections);
    sections.push(Section {
        tag: tag::PROV,
        width: 1,
        enc: ENC_KV,
        data: prov_section(prov)?,
    });

    Ok(assemble(
        [n_rows, n_symbols, n_files],
        u32::from(partial) * FLAG_PARTIAL,
        sections,
    ))
}

/// Lay out header, directory and sections (each starting on a multiple of 8).
fn assemble(counts: [u32; 3], flags: u32, mut sections: Vec<Section>) -> Vec<u8> {
    sections.sort_by_key(|s| s.tag);
    let n_sections = u32::try_from(sections.len()).unwrap_or(u32::MAX);
    let mut dir = Vec::with_capacity(sections.len() * DIR_ENTRY_LEN);
    let mut body: Vec<u8> = Vec::new();
    let base = HEADER_LEN + sections.len() * DIR_ENTRY_LEN;
    for s in &sections {
        pad8(&mut body);
        let offset = u32::try_from(base + body.len()).unwrap_or(u32::MAX);
        let len = u32::try_from(s.data.len()).unwrap_or(u32::MAX);
        dir.extend_from_slice(&s.tag.to_le_bytes());
        dir.extend_from_slice(&s.width.to_le_bytes());
        dir.extend_from_slice(&s.enc.to_le_bytes());
        dir.extend_from_slice(&offset.to_le_bytes());
        dir.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(&s.data);
    }
    pad8(&mut body);

    let mut file = vec![0u8; HEADER_LEN];
    file[0..8].copy_from_slice(&MAGIC);
    file[8..10].copy_from_slice(&MAJOR.to_le_bytes());
    file[10..12].copy_from_slice(&MINOR.to_le_bytes());
    file[12..16].copy_from_slice(&flags.to_le_bytes());
    file[24..32].copy_from_slice(&len_u64(base + body.len()).to_le_bytes());
    file[32..36].copy_from_slice(&counts[0].to_le_bytes());
    file[36..40].copy_from_slice(&counts[1].to_le_bytes());
    file[40..44].copy_from_slice(&counts[2].to_le_bytes());
    file[44..48].copy_from_slice(&n_sections.to_le_bytes());
    file.extend_from_slice(&dir);
    file.extend_from_slice(&body);
    let d = digest(&file[32..]);
    file[16..24].copy_from_slice(&d.to_le_bytes());
    file
}

// ---------------------------------------------------------------------------
// reader
// ---------------------------------------------------------------------------

fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .and_then(|s| s.first_chunk::<2>())
        .map_or(0, |c| u16::from_le_bytes(*c))
}

fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .and_then(|s| s.first_chunk::<4>())
        .map_or(0, |c| u32::from_le_bytes(*c))
}

fn le64(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8)
        .and_then(|s| s.first_chunk::<8>())
        .map_or(0, |c| u64::from_le_bytes(*c))
}

/// Check the magic and major version of a header and return its digest field.
/// `None` when either is wrong.
#[must_use]
pub fn header_digest(header: &[u8; HEADER_LEN]) -> Option<u64> {
    (header[0..8] == MAGIC && le16(header, 8) == MAJOR).then(|| le64(header, 16))
}

struct Sec<'a> {
    width: u16,
    enc: u16,
    data: &'a [u8],
}

type Secs<'a> = BTreeMap<u32, Sec<'a>>;

fn find<'s, 'a>(secs: &'s Secs<'a>, tag: u32) -> Result<&'s Sec<'a>, ReadError> {
    secs.get(&tag).ok_or(ReadError::MissingSection { tag })
}

/// A fixed-shape section: exact element width, encoding and byte length.
fn lane<'a>(
    secs: &Secs<'a>,
    tag: u32,
    width: u16,
    enc: u16,
    len: usize,
) -> Result<&'a [u8], ReadError> {
    let s = find(secs, tag)?;
    if s.width != width || s.enc != enc || s.data.len() != len {
        return Err(ReadError::LaneLength { tag });
    }
    Ok(s.data)
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn u64s(b: &[u8]) -> Vec<u64> {
    b.as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect()
}

fn lane_len(n: usize, width: usize, tag: u32) -> Result<usize, ReadError> {
    n.checked_mul(width).ok_or(ReadError::LaneLength { tag })
}

/// Only zero bytes, and fewer than 8 of them, may follow the last field.
fn only_padding(rest: &[u8], tag: u32) -> Result<(), ReadError> {
    if rest.len() < 8 && rest.iter().all(|&b| b == 0) {
        Ok(())
    } else {
        Err(ReadError::LaneLength { tag })
    }
}

fn decode_kv(tag: u32, data: &[u8]) -> Result<Vec<String>, ReadError> {
    let bad = ReadError::LaneLength { tag };
    if !data.len().is_multiple_of(8) {
        return Err(bad);
    }
    let n = usize::try_from(le32(data, 0)).map_err(|_| bad.clone())?;
    let table = n
        .checked_add(1)
        .and_then(|k| k.checked_mul(4))
        .and_then(|k| k.checked_add(4))
        .filter(|&k| k <= data.len())
        .ok_or_else(|| bad.clone())?;
    let offs = u32s(&data[4..table]);
    let blob = &data[table..];
    let blob_len = offs
        .last()
        .and_then(|&o| usize::try_from(o).ok())
        .ok_or_else(|| bad.clone())?;
    if offs.first() != Some(&0) || blob_len > blob.len() || offs.windows(2).any(|w| w[0] > w[1]) {
        return Err(bad);
    }
    only_padding(&blob[blob_len..], tag)?;
    let mut out = Vec::with_capacity(n);
    for w in offs.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        let s = std::str::from_utf8(&blob[a..b]).map_err(|_| ReadError::Utf8 { tag })?;
        out.push(s.to_owned());
    }
    Ok(out)
}

fn decode_prov(data: &[u8]) -> Result<Vec<Prov>, ReadError> {
    let tag = tag::PROV;
    let bad = ReadError::LaneLength { tag };
    if !data.len().is_multiple_of(8) || data.len() < 4 {
        return Err(bad);
    }
    let n = le32(data, 0);
    if n == 0 {
        return Err(bad);
    }
    let mut pos = 4usize;
    let mut out = Vec::new();
    for _ in 0..n {
        let mut f: Vec<Option<String>> = Vec::with_capacity(PROV_FIELDS);
        for _ in 0..PROV_FIELDS {
            let len = data
                .get(pos..pos + 4)
                .map(|_| le32(data, pos))
                .ok_or_else(|| bad.clone())?;
            pos += 4;
            if len == NO_ID {
                f.push(None);
                continue;
            }
            let end = usize::try_from(len)
                .ok()
                .and_then(|l| pos.checked_add(l))
                .filter(|&e| e <= data.len())
                .ok_or_else(|| bad.clone())?;
            let s = std::str::from_utf8(&data[pos..end]).map_err(|_| ReadError::Utf8 { tag })?;
            f.push(Some(s.to_owned()));
            pos = end;
        }
        let mut it = f.into_iter();
        let mut next = || it.next().flatten();
        out.push(Prov {
            charon_version: next(),
            charon_commit: next(),
            llbc_schema_version: next(),
            llbc_digest: next(),
            rustc_commit: next(),
            target_config: next(),
            source_revision: next(),
        });
    }
    only_padding(&data[pos..], tag)?;
    Ok(out)
}

fn codeword(mag: &[u8], pres: &[u64], row: usize) -> usize {
    let m = usize::from(mag.get(row).copied().unwrap_or(0));
    let p = pres.get(row / 64).map_or(0, |w| (w >> (row % 64)) & 1);
    m | (usize::try_from(p).unwrap_or(0) << 8)
}

/// Read a codec9 (magnitude, presence) lane pair of `n` rows.
fn lane9(secs: &Secs<'_>, mag: u32, pres: u32, n: usize) -> Result<(Vec<u8>, Vec<u64>), ReadError> {
    let m = lane(secs, mag, 1, ENC_CODEC9, n)?.to_vec();
    let p = u64s(lane(
        secs,
        pres,
        8,
        ENC_PRES,
        lane_len(n.div_ceil(64), 8, pres)?,
    )?);
    Ok((m, p))
}

/// Look up codeword `c` of a codebook lane: 0 is refused (`Codeword`), past the book is `IdRange`.
fn book_entry(book: &[String], c: usize, tag: u32, row: usize) -> Result<&str, ReadError> {
    if c == 0 {
        return Err(ReadError::Codeword { tag, row });
    }
    book.get(c - 1)
        .map(String::as_str)
        .ok_or(ReadError::IdRange { tag, row })
}

fn sym_ref(syms: &[&str], id: u32, tag: u32, row: usize) -> Result<String, ReadError> {
    usize::try_from(id)
        .ok()
        .and_then(|i| syms.get(i))
        .map(|s| (*s).to_owned())
        .ok_or(ReadError::IdRange { tag, row })
}

/// Parse the header and directory; returns the counts, flags and sections.
fn parse_directory(bytes: &[u8]) -> Result<(u64, u32, [usize; 3], Secs<'_>), ReadError> {
    let Some(h) = bytes.first_chunk::<HEADER_LEN>() else {
        return Err(ReadError::Truncated {
            header: 0,
            actual: bytes.len(),
        });
    };
    if h[0..8] != MAGIC {
        return Err(ReadError::Magic);
    }
    let major = le16(h, 8);
    if major != MAJOR {
        return Err(ReadError::Major(major));
    }
    let minor = le16(h, 10);
    if minor > MINOR {
        return Err(ReadError::Minor(minor));
    }
    if h[48..64].iter().any(|&b| b != 0) {
        return Err(ReadError::Reserved);
    }
    let file_len = le64(h, 24);
    if file_len != len_u64(bytes.len()) {
        return Err(ReadError::Truncated {
            header: file_len,
            actual: bytes.len(),
        });
    }
    let header_digest = le64(h, 16);
    let actual = digest(&bytes[32..]);
    if actual != header_digest {
        return Err(ReadError::Digest {
            header: header_digest,
            actual,
        });
    }

    let to_usize = |v: u32| usize::try_from(v).map_err(|_| ReadError::Directory);
    let n_sections = to_usize(le32(h, 44))?;
    let dir_end = n_sections
        .checked_mul(DIR_ENTRY_LEN)
        .and_then(|d| d.checked_add(HEADER_LEN))
        .filter(|&e| e <= bytes.len())
        .ok_or(ReadError::Directory)?;

    let mut secs: Secs<'_> = BTreeMap::new();
    let mut prev_end = dir_end;
    for i in 0..n_sections {
        let at = HEADER_LEN + i * DIR_ENTRY_LEN;
        let t = le32(bytes, at);
        let offset = to_usize(le32(bytes, at + 8))?;
        let len = to_usize(le32(bytes, at + 12))?;
        let end = offset.checked_add(len).filter(|&e| e <= bytes.len());
        let Some(end) = end.filter(|_| offset % 8 == 0 && offset >= prev_end) else {
            return Err(ReadError::Section { tag: t });
        };
        prev_end = end;
        let sec = Sec {
            width: le16(bytes, at + 4),
            enc: le16(bytes, at + 6),
            data: &bytes[offset..end],
        };
        if secs.insert(t, sec).is_some() {
            return Err(ReadError::Section { tag: t });
        }
    }
    for &t in secs.keys() {
        if t & 0x8000_0000 != 0 && !tag::ALL.contains(&t) {
            return Err(ReadError::UnknownRequired { tag: t });
        }
    }
    let counts = [le32(h, 32), le32(h, 36), le32(h, 40)];
    let counts = counts.map(|c| usize::try_from(c).unwrap_or(usize::MAX));
    Ok((header_digest, le32(h, 12), counts, secs))
}

/// Decode and verify a `<crate>.rows.bin` file. Checks run in a fixed order and
/// each refusal is its own [`ReadError`]: header, length, digest, directory,
/// section shapes, then the contents of every lane.
///
/// # Errors
/// The first [`ReadError`] found.
#[expect(clippy::too_many_lines, reason = "one pass that validates every lane")]
pub fn read(bytes: &[u8]) -> Result<Decoded, ReadError> {
    let (dig, flags, [n, n_syms, n_files], secs) = parse_directory(bytes)?;
    for t in tag::ALL {
        if t != tag::FILE_PRES && !secs.contains_key(&t) {
            return Err(ReadError::MissingSection { tag: t });
        }
    }

    let (pm, pp) = lane9(&secs, tag::PRED_MAG, tag::PRED_PRES, n)?;
    let (gm, gp) = lane9(&secs, tag::GRADE_MAG, tag::GRADE_PRES, n)?;
    let (km, kp) = lane9(&secs, tag::OBJ_KIND_MAG, tag::OBJ_KIND_PRES, n)?;
    let preds = decode_kv(tag::PRED_CB, find(&secs, tag::PRED_CB)?.data)?;
    let grades = decode_kv(tag::GRADE_CB, find(&secs, tag::GRADE_CB)?.data)?;
    let files = decode_kv(tag::FILE_CB, find(&secs, tag::FILE_CB)?.data)?;
    for (t, book) in [(tag::PRED_CB, &preds), (tag::GRADE_CB, &grades)] {
        if find(&secs, t)?.enc != ENC_KV || book.len() > CODEC9_MAX {
            return Err(ReadError::LaneLength { tag: t });
        }
    }
    if find(&secs, tag::FILE_CB)?.enc != ENC_KV || files.len() != n_files {
        return Err(ReadError::LaneLength { tag: tag::FILE_CB });
    }

    // Files: codec9 with a presence plane up to 511 entries, raw u32 beyond.
    let file9 = if n_files <= CODEC9_MAX {
        Some(lane9(&secs, tag::FILE, tag::FILE_PRES, n)?)
    } else {
        if secs.contains_key(&tag::FILE_PRES) {
            return Err(ReadError::LaneLength {
                tag: tag::FILE_PRES,
            });
        }
        None
    };
    let file_raw = match file9 {
        Some(_) => Vec::new(),
        None => u32s(lane(
            &secs,
            tag::FILE,
            4,
            ENC_RAW,
            lane_len(n, 4, tag::FILE)?,
        )?),
    };

    let u32_lane_of = |t: u32| -> Result<Vec<u32>, ReadError> {
        Ok(u32s(lane(&secs, t, 4, ENC_RAW, lane_len(n, 4, t)?)?))
    };
    let (subj, obj, mayt) = (
        u32_lane_of(tag::SUBJ)?,
        u32_lane_of(tag::OBJ)?,
        u32_lane_of(tag::MAYT)?,
    );
    let (start, end, fundecl) = (
        u32_lane_of(tag::START)?,
        u32_lane_of(tag::END)?,
        u32_lane_of(tag::FUNDECL)?,
    );

    let off_len = lane_len(
        n_syms
            .checked_add(1)
            .ok_or(ReadError::LaneLength { tag: tag::SYM_OFF })?,
        4,
        tag::SYM_OFF,
    )?;
    let offs = u32s(lane(&secs, tag::SYM_OFF, 4, ENC_RAW, off_len)?);
    let blob_sec = find(&secs, tag::SYM_BLOB)?;
    if blob_sec.width != 1 || blob_sec.enc != ENC_RAW {
        return Err(ReadError::LaneLength { tag: tag::SYM_BLOB });
    }
    let blob_str =
        std::str::from_utf8(blob_sec.data).map_err(|_| ReadError::Utf8 { tag: tag::SYM_BLOB })?;
    let ok_offsets = offs.first() == Some(&0)
        && offs.last().and_then(|&o| usize::try_from(o).ok()) == Some(blob_str.len())
        && offs.windows(2).all(|w| w[0] <= w[1]);
    if !ok_offsets {
        return Err(ReadError::LaneLength { tag: tag::SYM_OFF });
    }
    let mut labels: Vec<&str> = Vec::with_capacity(n_syms);
    for w in offs.windows(2) {
        let (a, b) = (w[0] as usize, w[1] as usize);
        if !blob_str.is_char_boundary(a) || !blob_str.is_char_boundary(b) {
            return Err(ReadError::Utf8 { tag: tag::SYM_BLOB });
        }
        labels.push(&blob_str[a..b]);
    }
    lane(
        &secs,
        tag::SYM_HASH,
        8,
        ENC_RAW,
        lane_len(n_syms, 8, tag::SYM_HASH)?,
    )?;
    let owners = u32s(lane(
        &secs,
        tag::SYM_OWNER,
        4,
        ENC_RAW,
        lane_len(n_syms, 4, tag::SYM_OWNER)?,
    )?);
    for (i, &o) in owners.iter().enumerate() {
        if o != NO_ID && usize::try_from(o).is_ok_and(|o| o >= n_syms) {
            return Err(ReadError::IdRange {
                tag: tag::SYM_OWNER,
                row: i,
            });
        }
    }
    let prov_sec = find(&secs, tag::PROV)?;
    if prov_sec.width != 1 || prov_sec.enc != ENC_KV {
        return Err(ReadError::LaneLength { tag: tag::PROV });
    }
    let prov = decode_prov(prov_sec.data)?;

    let mut rows = Vec::with_capacity(n);
    for row in 0..n {
        let kind = codeword(&km, &kp, row);
        if kind == 0 || kind > KIND_MAX {
            return Err(ReadError::Codeword {
                tag: tag::OBJ_KIND_MAG,
                row,
            });
        }
        let predicate = book_entry(&preds, codeword(&pm, &pp, row), tag::PRED_MAG, row)?.to_owned();
        let grade = book_entry(&grades, codeword(&gm, &gp, row), tag::GRADE_MAG, row)?.to_owned();
        let subject = sym_ref(&labels, subj[row], tag::SUBJ, row)?;
        let object = sym_ref(&labels, obj[row], tag::OBJ, row)?;
        if mayt[row] != NO_ID {
            sym_ref(&labels, mayt[row], tag::MAYT, row)?;
        }
        let file: Option<&str> = match &file9 {
            Some((m, p)) => match codeword(m, p, row) {
                0 => None,
                c => Some(book_entry(&files, c, tag::FILE, row)?),
            },
            None => match file_raw[row] {
                NO_ID => None,
                v => Some(
                    usize::try_from(v)
                        .ok()
                        .and_then(|i| files.get(i))
                        .map(String::as_str)
                        .ok_or(ReadError::IdRange {
                            tag: tag::FILE,
                            row,
                        })?,
                ),
            },
        };
        let site = match file {
            Some(f) => {
                let span = |v: u32, t: u32| {
                    if v == NO_ID {
                        Err(ReadError::IdRange { tag: t, row })
                    } else {
                        usize::try_from(v).map_err(|_| ReadError::IdRange { tag: t, row })
                    }
                };
                Some((
                    f.to_owned(),
                    span(start[row], tag::START)?,
                    span(end[row], tag::END)?,
                ))
            }
            None => {
                if start[row] != NO_ID {
                    return Err(ReadError::IdRange {
                        tag: tag::START,
                        row,
                    });
                }
                if end[row] != NO_ID {
                    return Err(ReadError::IdRange { tag: tag::END, row });
                }
                None
            }
        };
        rows.push(BinRow {
            predicate,
            subject,
            object,
            grade,
            site,
            fun: (fundecl[row] != NO_ID).then_some(fundecl[row]),
        });
    }
    Ok(Decoded {
        rows,
        partial: flags & FLAG_PARTIAL != 0,
        prov,
        digest: dig,
    })
}
