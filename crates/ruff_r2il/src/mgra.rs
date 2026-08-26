//! MGRA v3 encoder — the graph-ABI wire the `a2ui-graph` field renderer reads.
//!
//! This module is the WRITE side of a wire whose only normative statement lives in the
//! consumer: `AdaWorldAPI/a2ui-rs`, `crates/a2ui-graph/src/abi.rs`. Nothing here re-derives
//! the layout from prose — the offsets below are transcribed from that file, and the
//! round-trip test decodes with that crate's own parser rather than with a second reader
//! written here. Diffing an encoder against your own re-read of your own encoder proves
//! only that you were consistent.
//!
//! # Why this exists at all
//!
//! The reachability probe measures a partition — `CODE [29.9 .. 59.9]` reached against
//! `DATA [0.0 .. 0.9]` — and prints it as a table. The partition is a *graph*: addresses
//! are nodes, calls and data references are edges, and the reached/unreached split is a
//! per-node colour. `medcare-rs` already emits this wire for a different substrate
//! (`crates/medcare-server/src/views/graph_abi.rs`) and already renders it through
//! `a2ui-graph`, so the display tier is not being built here — only the emitter that
//! lets a 6502 image use it.
//!
//! # The lane layout (v3), transcribed
//!
//! ```text
//! header 16 B : magic "MGRA" | version u16 | flags u16 | node_count u32 | edge_count u32
//! node   16 B : classid u32 @0 | identity u32 @4 | vocab u8 @8 | role u8 @9
//!               | flags u8 @10 | (reserved @11) | domain u8 @12 | evidence u8 @13
//!               | reserved u16 @14
//! edge   12 B : from u32 @0 | to u32 @4 | kind u8 @8 | role u8 @9
//!               | flags u8 @10 | predicate u8 @11
//! tail        : "MGL1" labels — 4 B magic, then per node: u16 len | len bytes UTF-8
//! ```
//!
//! Header flags stay **0**: labels are unconditional, and titles (bit 1), rails (bit 2) and
//! curies (bit 3) are lanes this emitter does not write. Announcing a lane that is not on
//! the wire is not a cosmetic error — the reader dispatches tails by magic and would fail
//! on whatever byte followed.
//!
//! # What the bytes mean here
//!
//! `classid` is **0** for every node, and that is the canon-sanctioned value rather than a
//! placeholder. The zero-fallback ladder reads a zero classid as *default class, no prefix
//! routing (dormant)*, which leaves `identity` alone as the discriminator — exactly true of
//! a 6502 image, where an address IS the identity and no concept has been minted for
//! "function at $1d00". Minting one to fill the field would be inventing vocabulary to
//! satisfy a struct.
//!
//! `domain` and `vocab` are opaque palette indices by the consumer's own charter — it
//! "colors by the byte and never interprets it". [`Palette`] is therefore this emitter's
//! private codebook, meaningful to whoever reads the legend and to nothing else.
//!
//! `evidence` carries in-degree, clamped at 255: how many call sites point at a node. That
//! is a measurement the walk already produces, and it is the evidence that an address is a
//! function rather than a byte someone jumped into once.

use std::collections::BTreeMap;
use std::fmt;

/// The 4-byte stream magic.
pub const MAGIC: [u8; 4] = *b"MGRA";
/// The one wire version the consumer accepts. It refuses every other value rather than
/// reinterpreting offsets, so emitting anything else is emitting garbage loudly.
pub const WIRE_VERSION: u16 = 3;
/// Header length in bytes.
pub const HEADER_LEN: usize = 16;
/// Node record length in bytes.
pub const NODE_LEN: usize = 16;
/// Edge record length in bytes.
pub const EDGE_LEN: usize = 12;

const LABEL_MAGIC: [u8; 4] = *b"MGL1";

/// This emitter's private palette, written into the node `domain` byte.
///
/// The renderer colours by the raw byte and attaches no meaning to it, so these values are
/// a legend for humans reading the field, not a shared vocabulary.
///
/// There are exactly two, because a 6502 image without a symbol table supports exactly two
/// claims: an address something CALLS, and an address something names as a constant operand
/// of a load or a store. A variant for "reached" is deliberately absent — reachedness is a
/// property of a byte, and a node per decoded instruction is an instruction dump, not a call
/// graph. The byte-level reached percentage stays what it already is: a statistic
/// `reachability_probe` prints.
///
/// The ordering is load-bearing: [`GraphBuilder::observe`] keeps the MAXIMUM seen, so an
/// address that is both referenced and called resolves to [`Palette::Entry`]. Jump tables
/// make that a real case rather than a hypothetical one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Palette {
    /// A `Load`/`Store` names this address as a constant, and nothing calls it.
    ///
    /// On the Elite images every such address lands OUTSIDE the code image — zero page,
    /// the stack page, or a hardware register. Which of those it is depends on the C64's
    /// bank switching, which this walk does not model, so the node is not labelled with a
    /// guess about it.
    Referenced = 0,
    /// The target of at least one `Call` — a discovered entry point.
    Entry = 1,
}

impl Palette {
    /// The byte written to the wire.
    #[must_use]
    pub fn byte(self) -> u8 {
        self as u8
    }
}

/// This emitter's edge `kind` codebook. Same status as [`Palette`]: a legend, not a
/// contract with the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EdgeKind {
    /// A `Call` op whose target resolved inside the image.
    Call = 0,
    /// A `Load`/`Store` naming a constant address.
    AbsRef = 1,
}

impl EdgeKind {
    /// The byte written to the wire.
    #[must_use]
    pub fn byte(self) -> u8 {
        self as u8
    }
}

/// One node record plus its label.
#[derive(Debug, Clone)]
pub struct Node {
    /// Zero for every node this emitter writes — see the module docs on the fallback ladder.
    pub classid: u32,
    /// The 6502 address. The whole discriminator while `classid` is zero.
    pub identity: u32,
    /// Opaque codebook index; unassigned here.
    pub vocab: u8,
    /// Opaque role index; unassigned here.
    pub role: u8,
    /// Per-node flags; unassigned here.
    pub flags: u8,
    /// The [`Palette`] byte.
    pub domain: u8,
    /// In-degree, clamped at 255.
    pub evidence: u8,
    /// The label lane entry. Written for every node — the lane is unconditional and the
    /// reader expects exactly `node_count` entries.
    pub label: String,
}

/// One edge record. `from` and `to` are **node indices**, never addresses: the renderer
/// uses them to index its own instance buffers directly.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    /// Index into the node lane.
    pub from: u32,
    /// Index into the node lane.
    pub to: u32,
    /// The renderer's line style, read by the consumer's `edge_kind()`.
    pub kind: u8,
    /// Opaque role index; unassigned here.
    pub role: u8,
    /// Per-edge flags; unassigned here.
    pub flags: u8,
    /// The semantic relation, read by the consumer's `edge()`.
    ///
    /// Distinct from `kind` in the consumer — "the edge's renderer class (line style),
    /// distinct from its predicate" — even though this emitter derives both from the same
    /// [`EdgeKind`]. A call and a data reference differ in what they MEAN and in how they
    /// should be drawn, so both fields carry it rather than one being left zero.
    pub predicate: u8,
}

/// Why a graph was refused rather than encoded.
///
/// These are conditions worth refusing at the point of writing rather than discovering at
/// the point of drawing. The consumer is not defenceless — its `edge_pairs()` drops any
/// edge whose ordinal passes `node_count`, precisely so a ghost cannot index a GPU buffer
/// out of bounds — but a dropped edge is silent data loss: the field renders, slightly
/// wrong, and nothing says so. Refusing the stream names the offending edge instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The node or edge count does not fit the header's `u32`.
    TooManyRecords {
        /// Which lane overflowed.
        lane: &'static str,
        /// How many records were offered.
        count: usize,
    },
    /// An edge names a node index the node lane does not contain.
    EdgeEndpointOutOfRange {
        /// Position of the offending edge.
        edge: usize,
        /// The index it named.
        endpoint: u32,
        /// How many nodes exist.
        node_count: usize,
    },
    /// A label's UTF-8 length does not fit the lane's `u16` prefix.
    LabelTooLong {
        /// Position of the offending node.
        node: usize,
        /// The label's length in bytes.
        len: usize,
    },
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::TooManyRecords { lane, count } => {
                write!(f, "{count} {lane} records overflow the header u32")
            }
            EncodeError::EdgeEndpointOutOfRange {
                edge,
                endpoint,
                node_count,
            } => write!(
                f,
                "edge {edge} names node {endpoint}, but the graph has {node_count} nodes"
            ),
            EncodeError::LabelTooLong { node, len } => {
                write!(
                    f,
                    "label for node {node} is {len} bytes, over the u16 prefix"
                )
            }
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode nodes and edges into one MGRA v3 byte stream.
///
/// # Errors
///
/// Returns [`EncodeError`] when a count overflows the header, an edge names a node index
/// outside the node lane, or a label overruns its `u16` length prefix. Each of those would
/// otherwise produce a stream the consumer accepts and then misreads.
pub fn encode(nodes: &[Node], edges: &[Edge]) -> Result<Vec<u8>, EncodeError> {
    let node_count = u32::try_from(nodes.len()).map_err(|_| EncodeError::TooManyRecords {
        lane: "node",
        count: nodes.len(),
    })?;
    let edge_count = u32::try_from(edges.len()).map_err(|_| EncodeError::TooManyRecords {
        lane: "edge",
        count: edges.len(),
    })?;

    for (i, e) in edges.iter().enumerate() {
        for endpoint in [e.from, e.to] {
            if endpoint >= node_count {
                return Err(EncodeError::EdgeEndpointOutOfRange {
                    edge: i,
                    endpoint,
                    node_count: nodes.len(),
                });
            }
        }
    }
    for (i, n) in nodes.iter().enumerate() {
        if u16::try_from(n.label.len()).is_err() {
            return Err(EncodeError::LabelTooLong {
                node: i,
                len: n.label.len(),
            });
        }
    }

    let labels_len: usize = 4 + nodes.iter().map(|n| 2 + n.label.len()).sum::<usize>();
    let mut b = Vec::with_capacity(
        HEADER_LEN + nodes.len() * NODE_LEN + edges.len() * EDGE_LEN + labels_len,
    );

    b.extend_from_slice(&MAGIC);
    b.extend_from_slice(&WIRE_VERSION.to_le_bytes());
    // Labels only. Bits 1-3 would announce tails this emitter does not write.
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&node_count.to_le_bytes());
    b.extend_from_slice(&edge_count.to_le_bytes());

    for n in nodes {
        b.extend_from_slice(&n.classid.to_le_bytes());
        b.extend_from_slice(&n.identity.to_le_bytes());
        b.push(n.vocab);
        b.push(n.role);
        b.push(n.flags);
        b.push(0); // reserved @11
        b.push(n.domain);
        b.push(n.evidence);
        b.extend_from_slice(&0u16.to_le_bytes()); // reserved @14
    }

    for e in edges {
        b.extend_from_slice(&e.from.to_le_bytes());
        b.extend_from_slice(&e.to.to_le_bytes());
        b.push(e.kind);
        b.push(e.role);
        b.push(e.flags);
        b.push(e.predicate);
    }

    b.extend_from_slice(&LABEL_MAGIC);
    for n in nodes {
        // The length was proven to fit above; this cast cannot truncate.
        let len = n.label.len() as u16;
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(n.label.as_bytes());
    }

    Ok(b)
}

/// A graph under construction, addressed by 6502 address rather than by node index.
///
/// The renderer wants indices and the walk produces addresses, so something has to hold
/// the mapping. Doing it here keeps every caller from re-deriving it — and from the
/// off-by-one that an edge referring to an address the node lane never received would
/// otherwise become.
#[derive(Debug, Default)]
pub struct GraphBuilder {
    order: Vec<u64>,
    index: BTreeMap<u64, u32>,
    palette: BTreeMap<u64, u8>,
    in_degree: BTreeMap<u64, u32>,
    edges: Vec<(u64, u64, EdgeKind)>,
}

impl GraphBuilder {
    /// A builder with no nodes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an address, or raise its palette if this sighting is stronger evidence.
    ///
    /// Monotonic by design: the same address can be seen as reached and later as a call
    /// target, and the stronger reading must win regardless of arrival order. Without that,
    /// a node's colour would depend on walk order, which is not a property of the program.
    pub fn observe(&mut self, addr: u64, palette: Palette) {
        if !self.index.contains_key(&addr) {
            // A `u32` index is what the wire carries; the guard in `encode` catches an
            // overflow before it can be written, so a saturating cast here is not a place
            // a defect could hide.
            let idx = u32::try_from(self.order.len()).unwrap_or(u32::MAX);
            self.index.insert(addr, idx);
            self.order.push(addr);
        }
        let slot = self.palette.entry(addr).or_insert(0);
        *slot = (*slot).max(palette.byte());
    }

    /// Record an edge, observing both endpoints first so an edge can never name an address
    /// the node lane lacks.
    pub fn edge(&mut self, from: u64, to: u64, kind: EdgeKind) {
        let to_palette = match kind {
            EdgeKind::Call => Palette::Entry,
            EdgeKind::AbsRef => Palette::Referenced,
        };
        // Anything an edge leaves FROM is an entry by construction: the walk attributes
        // every call and reference to the entry its descent started at.
        self.observe(from, Palette::Entry);
        self.observe(to, to_palette);
        if kind == EdgeKind::Call {
            *self.in_degree.entry(to).or_insert(0) += 1;
        }
        self.edges.push((from, to, kind));
    }

    /// How many distinct addresses have been observed.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.order.len()
    }

    /// How many edges have been recorded.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Freeze into the wire's node and edge lanes, labelling each node with its address.
    #[must_use]
    pub fn finish(&self) -> (Vec<Node>, Vec<Edge>) {
        let nodes = self
            .order
            .iter()
            .map(|&addr| {
                let domain = self.palette.get(&addr).copied().unwrap_or(0);
                let deg = self.in_degree.get(&addr).copied().unwrap_or(0);
                Node {
                    classid: 0,
                    identity: u32::try_from(addr).unwrap_or(u32::MAX),
                    vocab: 0,
                    role: 0,
                    flags: 0,
                    domain,
                    evidence: u8::try_from(deg).unwrap_or(u8::MAX),
                    label: format!("${addr:04x}"),
                }
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .filter_map(|&(from, to, kind)| {
                Some(Edge {
                    from: *self.index.get(&from)?,
                    to: *self.index.get(&to)?,
                    kind: kind.byte(),
                    role: 0,
                    flags: 0,
                    predicate: kind.byte(),
                })
            })
            .collect();
        (nodes, edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2ui_graph::abi::{AbiError, GraphAbi};

    /// Three nodes, two edges, one of each kind — small enough to assert every field by
    /// hand, wide enough that a transposed lane or a dropped byte moves something.
    fn sample() -> (Vec<Node>, Vec<Edge>) {
        let mut g = GraphBuilder::new();
        g.observe(0x1d00, Palette::Entry);
        g.edge(0x1d00, 0x1f2a, EdgeKind::Call);
        g.edge(0x1d00, 0xd020, EdgeKind::AbsRef);
        g.finish()
    }

    #[test]
    fn the_consumers_own_parser_reads_back_every_field() {
        let (nodes, edges) = sample();
        let buf = encode(&nodes, &edges).expect("encodes");
        // Decoded by `a2ui-graph`, the crate that actually renders this wire. A second
        // reader written here would only prove this module agrees with itself.
        let g = GraphAbi::parse(&buf).expect("the real consumer parses it");

        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 2);
        assert_eq!(g.flags(), 0, "no optional tail is announced");

        assert_eq!(g.address(0), (0, 0x1d00));
        assert_eq!(g.address(1), (0, 0x1f2a));
        assert_eq!(g.address(2), (0, 0xd020));

        assert_eq!(g.domain(0), Palette::Entry.byte());
        assert_eq!(g.domain(1), Palette::Entry.byte());
        assert_eq!(g.domain(2), Palette::Referenced.byte());

        assert_eq!(g.evidence(1), 1, "one call site points at $1f2a");
        assert_eq!(g.evidence(2), 0, "an absolute reference is not a call");

        assert_eq!(g.label(0), "$1d00");
        assert_eq!(g.label(2), "$d020");

        // `edge()`'s third element is the PREDICATE at @11; the line style is a separate
        // byte at @8 behind `edge_kind()`. Asserting both is what caught them being
        // conflated when this test was first written.
        assert_eq!(g.edge(0), (0, 1, EdgeKind::Call.byte()));
        assert_eq!(g.edge(1), (0, 2, EdgeKind::AbsRef.byte()));
        assert_eq!(g.edge_kind(0), EdgeKind::Call.byte());
        assert_eq!(g.edge_kind(1), EdgeKind::AbsRef.byte());
    }

    #[test]
    fn an_edge_naming_a_node_the_lane_lacks_is_refused() {
        let (nodes, mut edges) = sample();
        // The graph is valid until this line, so the assertion below cannot pass for the
        // trivial reason that the fixture was broken all along.
        assert!(encode(&nodes, &edges).is_ok(), "the fixture starts valid");

        edges[0].to = 99;
        let err = encode(&nodes, &edges).expect_err("an out-of-range endpoint must be refused");
        assert_eq!(
            err,
            EncodeError::EdgeEndpointOutOfRange {
                edge: 0,
                endpoint: 99,
                node_count: 3,
            }
        );
    }

    #[test]
    fn a_label_over_the_length_prefix_is_refused() {
        let (mut nodes, edges) = sample();
        nodes[0].label = "x".repeat(usize::from(u16::MAX));
        assert!(
            encode(&nodes, &edges).is_ok(),
            "exactly u16::MAX still fits the prefix"
        );

        nodes[0].label.push('x');
        let err = encode(&nodes, &edges).expect_err("one byte past the prefix must be refused");
        assert_eq!(
            err,
            EncodeError::LabelTooLong {
                node: 0,
                len: usize::from(u16::MAX) + 1,
            }
        );
    }

    #[test]
    fn the_palette_rises_with_evidence_and_never_falls() {
        // Walk order is not a property of the program, so the colour must not depend on it.
        // A jump table's slot is loaded as data AND called; whichever the walk sees first,
        // the node must come out an entry.
        let mut a = GraphBuilder::new();
        a.observe(0x1d00, Palette::Referenced);
        a.observe(0x1d00, Palette::Entry);

        let mut b = GraphBuilder::new();
        b.observe(0x1d00, Palette::Entry);
        b.observe(0x1d00, Palette::Referenced);

        let (na, _) = a.finish();
        let (nb, _) = b.finish();
        assert_eq!(na[0].domain, Palette::Entry.byte());
        assert_eq!(
            nb[0].domain, na[0].domain,
            "order must not decide the colour"
        );
        assert_eq!(a.node_count(), 1, "one address is one node");
    }

    #[test]
    fn edges_carry_lane_indices_not_addresses() {
        let (_, edges) = sample();
        // $1f2a would be 7978 as an index; the node lane has three entries. Emitting the
        // address here is the mistake this asserts against, and it would sail past the
        // consumer's parser into an out-of-bounds instance lookup.
        assert!(
            edges.iter().all(|e| e.from < 3 && e.to < 3),
            "endpoints must index the node lane"
        );
        assert_eq!(edges[0].to, 1);
    }

    #[test]
    fn a_wrong_version_byte_is_refused_by_the_consumer() {
        // Guards the one constant that decides whether every offset above means anything.
        let (nodes, edges) = sample();
        let mut buf = encode(&nodes, &edges).expect("encodes");
        assert!(GraphAbi::parse(&buf).is_ok(), "v3 parses");

        buf[4] = 2;
        match GraphAbi::parse(&buf) {
            Err(AbiError::WrongVersion(v)) => assert_eq!(v, 2),
            Err(other) => panic!("wrong refusal: {other}"),
            Ok(_) => panic!("a v2 stream must not parse under a v3 reader"),
        }
    }
}
