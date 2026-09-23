//! Shared strategies and observers for the property tier.
//!
//! Nothing here reaches into `yrs` to do what the binding can do. There is
//! one deliberate exception, and this is it: `Update::decode_v1` describes an
//! update's clock ranges. The binding exposes no such view, and the
//! alternative would be a second decoder. (`has_missing_updates` is read
//! through the `ReadTxn` trait on the binding's own transaction type, which is
//! the same method the UDL's `transaction_has_missing_updates` returns.)

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use proptest::prelude::*;
use yrs::updates::decoder::Decode;
use yrs::{ReadTxn, Update};

use crate::doc::YrsDoc;
use crate::text::YrsText;
use crate::transaction::YrsTransaction;

/// The root text name every property uses.
pub(crate) const ROOT: &str = "prompt";

/// The client ids that have to be tried every run rather than waited for.
/// 2^31 is where the old Swift side used to clamp, 2^32 is where yrs 0.18's
/// `read_client` started truncating, and 2^53 - 1 is the top of the domain
/// `ClientID::new` admits.
pub(crate) const CORNER_CLIENT_IDS: [u64; 6] = [
    1,
    (1u64 << 31) - 1,
    1u64 << 31,
    (1u64 << 32) - 1,
    1u64 << 32,
    (1u64 << 53) - 1,
];

/// A client id: the corner set, weighted so it is drawn often, unioned with a
/// uniform draw over the whole 53 bit domain.
pub(crate) fn client_id() -> impl Strategy<Value = u64> {
    prop_oneof![
        2 => proptest::sample::select(CORNER_CLIENT_IDS.as_slice()),
        3 => 1u64..(1u64 << 53),
    ]
}

/// `n` distinct client ids, which is what peers in one document need.
pub(crate) fn distinct_client_ids(n: usize) -> impl Strategy<Value = Vec<u64>> {
    proptest::collection::vec(client_id(), n).prop_filter("client ids must be distinct", |ids| {
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len() == ids.len()
    })
}

/// ASCII only, for the properties whose subject is the channel rather than the
/// text: every index is then a UTF-16 boundary and the model stays obvious.
pub(crate) fn ascii_chunk() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        proptest::sample::select("abcdeFGHIJ0123 ".chars().collect::<Vec<_>>()),
        1..6,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

/// The alphabets P5 mixes: ASCII, accented Latin, Cyrillic, CJK, and non-BMP
/// emoji, the last of which is two UTF-16 code units per character.
pub(crate) fn mixed_chunk() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = "abZ9 "
        .chars()
        .chain("éàüñçö".chars())
        .chain("привет".chars())
        .chain("日本語漢字".chars())
        .chain("😀🎉🌍🧪".chars())
        .collect();
    proptest::collection::vec(proptest::sample::select(alphabet), 1..5)
        .prop_map(|cs| cs.into_iter().collect())
}

/// A document with a chosen client id, and optionally without garbage
/// collection of deleted blocks.
pub(crate) fn doc_with(client: u64, skip_gc: bool) -> YrsDoc {
    YrsDoc::with_client_id(client, skip_gc).expect("every id this tier draws is below 2^53")
}

/// The root text of a document. Always taken before a transaction is opened:
/// `get_or_insert_text` transacts internally.
pub(crate) fn root_text(doc: &YrsDoc) -> Arc<YrsText> {
    doc.get_text(ROOT.to_string())
}

/// Run `f` inside one transaction and close it. The binding's transactions are
/// explicit objects; leaving one open blocks the next `transact`.
pub(crate) fn with_txn<R>(doc: &YrsDoc, f: impl FnOnce(&YrsTransaction) -> R) -> R {
    let txn = doc.transact(None);
    let result = f(&txn);
    txn.free();
    result
}

/// A document's state vector, decoded, through the binding's own accessor.
pub(crate) fn state_map(txn: &YrsTransaction) -> BTreeMap<u64, u32> {
    txn.transaction_client_states()
        .into_iter()
        .map(|s| (s.client_id, s.clock))
        .collect()
}

/// What a peer looks like from outside: its text, its state vector, and
/// whether it is still waiting for something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observed {
    pub(crate) text: String,
    pub(crate) state: BTreeMap<u64, u32>,
    pub(crate) missing: bool,
}

pub(crate) fn observe(doc: &YrsDoc) -> Observed {
    let text = root_text(doc);
    with_txn(doc, |txn| Observed {
        text: text.get_string(txn),
        state: state_map(txn),
        missing: txn.has_missing_updates(),
    })
}

/// Does `sv` know everything `deps` names?
pub(crate) fn dominates(sv: &BTreeMap<u64, u32>, deps: &BTreeMap<u64, u32>) -> bool {
    deps.iter()
        .all(|(client, clock)| sv.get(client).copied().unwrap_or(0) >= *clock)
}

/// The clock ranges an encoded update carries, per client: what it inserts and
/// what it deletes. The binding has no view of this, so the update is decoded
/// with yrs's own reader and read through its public `insertions` and
/// `delete_set`.
#[derive(Debug, Clone, Default)]
pub(crate) struct UpdateSpan {
    pub(crate) inserts: BTreeMap<u64, Vec<Range<u32>>>,
    pub(crate) deletes: BTreeMap<u64, Vec<Range<u32>>>,
}

impl UpdateSpan {
    pub(crate) fn decode(bytes: &[u8]) -> Self {
        let update = Update::decode_v1(bytes).expect("an update this tier produced must decode");
        let mut span = UpdateSpan::default();
        for (client, ranges) in update.insertions(true).iter() {
            let entry = span.inserts.entry(client.get()).or_default();
            for range in ranges.iter() {
                entry.push(range.clone());
            }
        }
        for (client, ranges) in update.delete_set().iter() {
            let entry = span.deletes.entry(client.get()).or_default();
            for range in ranges.iter() {
                entry.push(range.clone());
            }
        }
        span
    }

    /// The lowest clock this update inserts for `client`, if any.
    pub(crate) fn first_insert_clock(&self, client: u64) -> Option<u32> {
        self.inserts
            .get(&client)
            .and_then(|ranges| ranges.iter().map(|r| r.start).min())
    }

    /// A client whose FIRST block in this update sits above what `sv` has
    /// seen: the receiver cannot reach the update's own starting point for
    /// that client, so nothing of it can be integrated.
    pub(crate) fn unreachable_client(&self, sv: &BTreeMap<u64, u32>) -> Option<u64> {
        self.inserts.iter().find_map(|(client, ranges)| {
            let first = ranges.iter().map(|r| r.start).min()?;
            (sv.get(client).copied().unwrap_or(0) < first).then_some(*client)
        })
    }

    /// A client whose blocks this update cannot lay down contiguously on top
    /// of `sv`: either it starts above what the receiver has, or it has a hole
    /// of its own.
    ///
    /// An update is not described by its author's state vector alone. A peer
    /// whose own store holds blocks past a Skip re-encodes them in its next
    /// diff while reporting the skip's start as its clock, so an update can
    /// carry a hole its composer's state vector does not mention.
    pub(crate) fn gapped_client(&self, sv: &BTreeMap<u64, u32>) -> Option<u64> {
        self.inserts.iter().find_map(|(client, ranges)| {
            let mut sorted: Vec<_> = ranges.clone();
            sorted.sort_by_key(|r| r.start);
            let mut reachable = sv.get(client).copied().unwrap_or(0);
            for range in sorted {
                if range.start > reachable {
                    return Some(*client);
                }
                reachable = reachable.max(range.end);
            }
            None
        })
    }

    /// Every id this update INSERTS is at or below the clock `sv` records for
    /// its client: the receiver holds the blocks, whatever it makes of the
    /// delete set. The delete set is kept out of this on purpose: a diff
    /// carries the composer's whole delete set, which on yrs 0.27.4 can name
    /// ids above the composer's own state vector, so a receiver that has
    /// integrated everything integrable still would not "cover" it.
    pub(crate) fn inserts_covered_by(&self, sv: &BTreeMap<u64, u32>) -> bool {
        let known = |client: &u64| sv.get(client).copied().unwrap_or(0);
        self.inserts
            .iter()
            .all(|(client, ranges)| ranges.iter().all(|r| r.end <= known(client)))
    }

    /// Every id this update names, insert or delete, is at or below the clock
    /// `sv` records for its client: the document has the update, it is not
    /// waiting for any part of it.
    pub(crate) fn covered_by(&self, sv: &BTreeMap<u64, u32>) -> bool {
        let known = |client: &u64| sv.get(client).copied().unwrap_or(0);
        self.inserts
            .iter()
            .chain(self.deletes.iter())
            .all(|(client, ranges)| ranges.iter().all(|r| r.end <= known(client)))
    }
}

/// One generated edit, in character positions. The positions are raw draws;
/// they are taken modulo the model's length when the edit is planned, so a
/// shrunk case stays meaningful whatever the text has become by then.
#[derive(Debug, Clone)]
pub(crate) enum Edit {
    Insert { pos: usize, chunk: String },
    Delete { pos: usize, len: usize },
}

/// The same edit in the units the binding speaks: UTF-16 code units, because
/// `YrsDoc::new` builds a document with `OffsetKind::Utf16`.
#[derive(Debug, Clone)]
pub(crate) enum Planned {
    Insert { offset: u32, chunk: String },
    Delete { offset: u32, len: u32 },
}

fn utf16_len(chars: &[char]) -> u32 {
    chars.iter().map(|c| c.len_utf16() as u32).sum()
}

/// Plan an edit against a plain string model, mutating the model and returning
/// what to hand the binding. `None` means the edit had nothing to work on (a
/// delete against empty text) and should be skipped.
pub(crate) fn plan(model: &mut String, edit: &Edit) -> Option<Planned> {
    let chars: Vec<char> = model.chars().collect();
    match edit {
        Edit::Insert { pos, chunk } => {
            let idx = pos % (chars.len() + 1);
            let offset = utf16_len(&chars[..idx]);
            let byte_idx: usize = chars[..idx].iter().map(|c| c.len_utf8()).sum();
            model.insert_str(byte_idx, chunk);
            Some(Planned::Insert {
                offset,
                chunk: chunk.clone(),
            })
        }
        Edit::Delete { pos, len } => {
            if chars.is_empty() {
                return None;
            }
            let start = pos % chars.len();
            let count = (len % (chars.len() - start)) + 1;
            let offset = utf16_len(&chars[..start]);
            let length = utf16_len(&chars[start..start + count]);
            let mut next: String = chars[..start].iter().collect();
            next.extend(chars[start + count..].iter());
            *model = next;
            Some(Planned::Delete {
                offset,
                len: length,
            })
        }
    }
}

/// Plan an edit against the text a document is actually holding, which is what
/// a caller does: Swift computes an offset from the string it just rendered.
/// A peer that has taken remote edits no longer matches any private model, and
/// planning against one would generate offsets that fall inside a character.
pub(crate) fn plan_for(current: &str, edit: &Edit) -> Option<Planned> {
    let mut text = current.to_string();
    plan(&mut text, edit)
}

/// The number of UTF-16 code units the first character of `text` occupies, or
/// zero for empty text. Used to delete a whole character rather than half of
/// one.
pub(crate) fn first_char_units(text: &str) -> u32 {
    text.chars()
        .next()
        .map(|c| c.len_utf16() as u32)
        .unwrap_or(0)
}

/// Run a planned edit through the binding, inside an already open transaction.
pub(crate) fn run(text: &YrsText, txn: &YrsTransaction, planned: &Planned) {
    match planned {
        Planned::Insert { offset, chunk } => text.insert(txn, *offset, chunk.clone()),
        Planned::Delete { offset, len } => text.remove_range(txn, *offset, *len),
    }
}

fn edits(
    chunk: impl Strategy<Value = String> + 'static,
    count: std::ops::Range<usize>,
) -> impl Strategy<Value = Vec<Edit>> {
    let edit = prop_oneof![
        3 => (0usize..64, chunk).prop_map(|(pos, chunk)| Edit::Insert { pos, chunk }),
        1 => (0usize..64, 0usize..8).prop_map(|(pos, len)| Edit::Delete { pos, len }),
    ];
    proptest::collection::vec(edit, count)
}

/// A run of edits over the ASCII alphabet.
pub(crate) fn ascii_edits(count: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Edit>> {
    edits(ascii_chunk(), count)
}

/// A run of edits over the mixed alphabets, which is where UTF-16 offsets stop
/// being the same number as character offsets.
pub(crate) fn mixed_edits(count: std::ops::Range<usize>) -> impl Strategy<Value = Vec<Edit>> {
    edits(mixed_chunk(), count)
}

/// Hex, for the byte level assertions and for the vectors printed for the
/// other core to compare against.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
