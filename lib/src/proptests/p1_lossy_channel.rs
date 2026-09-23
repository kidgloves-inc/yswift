//! P1: convergence under a lossy channel.
//!
//! A generated program over two or three peers, interpreted by a channel that
//! reorders, duplicates and drops. This is the channel a forwarding server
//! actually is: updates arrive out of order, twice, or not at all until a
//! later resync. What is asserted, in order of how much it
//! says: an update the receiver is ready for (it holds everything the composer
//! held, and the update lays down contiguously on its clocks) is integrated at
//! once; an update starting above anything the receiver was ever handed is
//! not; a peer that has only ever been handed updates it was ready for reports
//! nothing missing; and at quiescence every peer holds the same text and the
//! same state vector.
//!
//! `has_missing_updates` is weaker on yrs 0.27.4 than its name suggests, in
//! both directions, and each direction was established by probing the core
//! rather than by reading it. Each also has a committed reproducer, so neither
//! claim rests on this comment.
//!
//! It reads FALSE while a peer is behind. A hole in one client's own sequence
//! is recorded as a Skip block in the block store, not as a pending update;
//! only an unresolved dependency (a missing origin, or a delete naming an
//! unknown id) sets the flag. P3's
//! `a_diff_against_a_partial_state_vector_repairs_the_peer` exercises exactly
//! that shape, which is why it asserts the flag in one direction only.
//!
//! It reads TRUE after a peer is fully caught up. A pending record whose
//! `missing` clock the store has already reached is never retried and never
//! cleared, because `apply_update` retries only when that clock is strictly
//! below the store's clock for the client (`transaction.rs`, step 3). Such a
//! peer agrees with its neighbours on text and on state vector and still
//! answers `true`. `a_resynced_peer_reports_nothing_missing` below is that
//! case, `#[ignore]`d because it fails on this core.
//!
//! Quiescence therefore means a resync, not just a redelivery: yrs 0.27.4 does
//! not always re-integrate a pending update when the one it was waiting for
//! arrives (`reordered_same_author_updates_are_all_integrated`, also ignored),
//! so the channel ends with what the real protocol does, each peer asking the
//! others for the difference against its state vector. The property tolerates
//! a peer still reporting something missing after plain redelivery, which is
//! exactly why it does not catch those two core bugs itself.

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::helpers::{
    ascii_edits, distinct_client_ids, doc_with, dominates, mixed_chunk, observe, plan_for,
    root_text, run, state_map, with_txn, Edit, UpdateSpan,
};
use crate::doc::YrsDoc;
use crate::text::YrsText;
use std::sync::Arc;
use yrs::ReadTxn;

/// One instruction of a generated program. Peer and inbox indices are raw
/// draws, taken modulo the live counts when interpreted.
#[derive(Debug, Clone)]
enum Op {
    /// A peer edits its own copy, which puts one update in every other peer's
    /// inbox.
    Edit { peer: usize, edit: Edit },
    /// A peer takes one update out of its inbox and applies it.
    Deliver { peer: usize, index: usize },
    /// The channel delivers the same update twice.
    Duplicate { peer: usize, index: usize },
    /// The channel loses an update. It is remembered, and offered again at the
    /// next quiescence: a lost update is late, not gone.
    Drop { peer: usize, index: usize },
    /// Everything settles: what was dropped is offered again, the inboxes are
    /// drained, and the peers resync.
    Quiesce,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0usize..3, ascii_edits(1..2))
            .prop_map(|(peer, mut edits)| Op::Edit { peer, edit: edits.remove(0) }),
        6 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Deliver { peer, index }),
        1 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Duplicate { peer, index }),
        2 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Drop { peer, index }),
        1 => Just(Op::Quiesce),
    ]
}

/// An update in flight, with what its composer HELD when it made it.
///
/// `deps` is not the composer's state vector. A peer whose own store has a
/// hole reports the hole's start as its clock for that client while still
/// holding, and being able to reference, blocks above it, so its state vector
/// understates what an update it composes can depend on. What is recorded here
/// is every clock range the composer has ever been handed or authored, which
/// is an upper bound on what the update can reference and therefore the honest
/// precondition for "the receiver was ready for this".
#[derive(Debug, Clone)]
struct Envelope {
    update: Vec<u8>,
    deps: BTreeMap<u64, u32>,
    author: u64,
    span: UpdateSpan,
}

struct Peer {
    doc: YrsDoc,
    text: Arc<YrsText>,
    client: u64,
    /// The state vector the peer had when it last composed, so its next update
    /// is a diff rather than its whole state.
    seen: Vec<u8>,
}

struct Channel {
    peers: Vec<Peer>,
    inboxes: Vec<Vec<Envelope>>,
    dropped: Vec<(usize, Envelope)>,
    /// Everything each peer has actually been handed.
    delivered: Vec<Vec<Envelope>>,
    /// Whether a peer has only ever been handed updates whose dependencies it
    /// already held. Such a peer has never had a reason to pend anything.
    in_order: Vec<bool>,
    /// The highest clock per client each peer has ever been handed or
    /// authored, integrated or not. See [`Envelope::deps`].
    holdings: Vec<BTreeMap<u64, u32>>,
}

/// Raise every client's clock in `into` to the highest one `span` names, on
/// either side of the insert and delete split.
fn absorb(into: &mut BTreeMap<u64, u32>, span: &UpdateSpan) {
    for (client, ranges) in span.inserts.iter().chain(span.deletes.iter()) {
        if let Some(end) = ranges.iter().map(|range| range.end).max() {
            let clock = into.entry(*client).or_insert(0);
            *clock = (*clock).max(end);
        }
    }
}

impl Channel {
    fn new(clients: &[u64]) -> Self {
        let peers: Vec<Peer> = clients
            .iter()
            .map(|client| {
                let doc = doc_with(*client, false);
                let text = root_text(&doc);
                let seen = with_txn(&doc, |txn| txn.transaction_state_vector());
                Peer {
                    doc,
                    text,
                    client: *client,
                    seen,
                }
            })
            .collect();
        let count = peers.len();
        Channel {
            peers,
            inboxes: vec![Vec::new(); count],
            dropped: Vec::new(),
            delivered: vec![Vec::new(); count],
            in_order: vec![true; count],
            holdings: vec![BTreeMap::new(); count],
        }
    }

    fn edit(&mut self, peer: usize, edit: &Edit) {
        let peer = peer % self.peers.len();
        let (update, deps) = {
            let p = &self.peers[peer];
            let deps = self.holdings[peer].clone();
            let update = with_txn(&p.doc, |txn| {
                match plan_for(&p.text.get_string(txn), edit) {
                    Some(planned) => {
                        run(&p.text, txn, &planned);
                        Some(p.doc.encode_diff_v1(txn, p.seen.clone()).unwrap())
                    }
                    None => None,
                }
            });
            (update, deps)
        };
        let Some(update) = update else { return };
        let author = self.peers[peer].client;
        self.peers[peer].seen =
            with_txn(&self.peers[peer].doc, |txn| txn.transaction_state_vector());
        let envelope = Envelope {
            span: UpdateSpan::decode(&update),
            update,
            deps,
            author,
        };
        absorb(&mut self.holdings[peer], &envelope.span);
        for other in 0..self.peers.len() {
            if other != peer {
                self.inboxes[other].push(envelope.clone());
            }
        }
    }

    /// The highest clock per client the peer could hold given what it has been
    /// handed: its own state vector extended by every delivered update whose
    /// blocks join onto it, to a fixpoint. A peer that was never given the
    /// bytes for a range cannot integrate anything above that range, whatever
    /// it is holding pending.
    fn reachable(before: &BTreeMap<u64, u32>, delivered: &[Envelope]) -> BTreeMap<u64, u32> {
        let mut reach = before.clone();
        loop {
            let mut changed = false;
            for envelope in delivered {
                for (client, ranges) in &envelope.span.inserts {
                    let clock = reach.entry(*client).or_insert(0);
                    for range in ranges {
                        if range.start <= *clock && range.end > *clock {
                            *clock = range.end;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                return reach;
            }
        }
    }

    /// Apply one envelope and check what the receiver did with it.
    fn apply(&mut self, peer: usize, envelope: Envelope) -> Result<(), TestCaseError> {
        let p = &self.peers[peer];
        let before = with_txn(&p.doc, |txn| state_map(txn));
        with_txn(&p.doc, |txn| {
            txn.transaction_apply_update(envelope.update.clone())
                .unwrap()
        });
        let after = with_txn(&p.doc, |txn| state_map(txn));

        // Ready means two things: the receiver has integrated everything the
        // composer HELD when it composed (not merely everything the composer's
        // state vector admitted to), and the update starts where the
        // receiver's own clocks are, with no hole in any client's sequence.
        let gap = envelope.span.gapped_client(&before);
        let reachable = Self::reachable(&before, &self.delivered[peer]);
        let in_order = dominates(&before, &envelope.deps) && gap.is_none();
        if in_order {
            prop_assert!(
                envelope.span.inserts_covered_by(&after),
                "the receiver was ready for this update, so it had to integrate: \
                 peer {} author {} span {:?} deps {:?} before {:?} after {:?}",
                peer,
                envelope.author,
                envelope.span,
                envelope.deps,
                before,
                after
            );
        } else if envelope.span.unreachable_client(&reachable).is_some() {
            prop_assert!(
                !envelope.span.inserts_covered_by(&after),
                "an update starting above anything the receiver was ever handed \
                 must not integrate: peer {} span {:?} before {:?} reachable {:?} after {:?}",
                peer,
                envelope.span,
                before,
                reachable,
                after
            );
        }

        absorb(&mut self.holdings[peer], &envelope.span);
        self.delivered[peer].push(envelope);
        self.in_order[peer] &= in_order;

        // A peer that has only ever been handed updates it was ready for has
        // nothing to wait for. This is the one direction of
        // `has_missing_updates` that holds on yrs 0.27.4; see the module note.
        if self.in_order[peer] {
            prop_assert!(
                !observe(&self.peers[peer].doc).missing,
                "a peer handed only updates it was ready for reports one missing"
            );
        }
        Ok(())
    }

    fn deliver(&mut self, peer: usize, index: usize) -> Result<(), TestCaseError> {
        let peer = peer % self.peers.len();
        if self.inboxes[peer].is_empty() {
            return Ok(());
        }
        let index = index % self.inboxes[peer].len();
        let envelope = self.inboxes[peer].remove(index);
        self.apply(peer, envelope)
    }

    fn duplicate(&mut self, peer: usize, index: usize) {
        let peer = peer % self.peers.len();
        if self.inboxes[peer].is_empty() {
            return;
        }
        let index = index % self.inboxes[peer].len();
        let copy = self.inboxes[peer][index].clone();
        self.inboxes[peer].push(copy);
    }

    fn drop_one(&mut self, peer: usize, index: usize) {
        let peer = peer % self.peers.len();
        if self.inboxes[peer].is_empty() {
            return;
        }
        let index = index % self.inboxes[peer].len();
        let envelope = self.inboxes[peer].remove(index);
        self.dropped.push((peer, envelope));
    }

    /// Offer everything again, drain every inbox, then resync.
    fn quiesce(&mut self) -> Result<(), TestCaseError> {
        for (peer, envelope) in std::mem::take(&mut self.dropped) {
            self.inboxes[peer].push(envelope);
        }
        for peer in 0..self.peers.len() {
            while !self.inboxes[peer].is_empty() {
                let envelope = self.inboxes[peer].remove(0);
                self.apply(peer, envelope)?;
            }
        }

        // Every peer that believes it is up to date must agree with every
        // other such peer. A peer still reporting something missing is behind,
        // and the resync below is what a real client does about it.
        let settled: Vec<_> = (0..self.peers.len())
            .map(|i| observe(&self.peers[i].doc))
            .collect();
        for i in 0..settled.len() {
            for j in (i + 1)..settled.len() {
                if !settled[i].missing && !settled[j].missing {
                    prop_assert_eq!(
                        &settled[i].text,
                        &settled[j].text,
                        "two peers holding nothing outstanding disagree"
                    );
                    prop_assert_eq!(&settled[i].state, &settled[j].state);
                }
            }
        }

        // The resync: every peer asks every other for the difference against
        // what it has. Two rounds, which is one more than convergence needs.
        for _ in 0..2 {
            for receiver in 0..self.peers.len() {
                for sender in 0..self.peers.len() {
                    if sender == receiver {
                        continue;
                    }
                    let sv = with_txn(&self.peers[receiver].doc, |txn| {
                        txn.transaction_state_vector()
                    });
                    let diff = with_txn(&self.peers[sender].doc, |txn| {
                        self.peers[sender].doc.encode_diff_v1(txn, sv).unwrap()
                    });
                    with_txn(&self.peers[receiver].doc, |txn| {
                        txn.transaction_apply_update(diff).unwrap()
                    });
                }
            }
        }

        let views: Vec<_> = (0..self.peers.len())
            .map(|i| observe(&self.peers[i].doc))
            .collect();
        for i in 1..views.len() {
            prop_assert_eq!(
                &views[i].text,
                &views[0].text,
                "peers disagree after a resync"
            );
            prop_assert_eq!(&views[i].state, &views[0].state);
        }
        // What is deliberately NOT asserted here: that no peer reports
        // anything missing. It is false on yrs 0.27.4, and
        // `a_resynced_peer_reports_nothing_missing` below is the reproducer,
        // ignored for that reason.
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// The channel property.
    #[test]
    fn peers_converge_under_a_lossy_channel(
        clients in distinct_client_ids(3),
        peer_count in 2usize..=3,
        ops in proptest::collection::vec(op(), 4..40),
    ) {
        let mut channel = Channel::new(&clients[..peer_count]);
        for op in &ops {
            match op {
                Op::Edit { peer, edit } => channel.edit(*peer, edit),
                Op::Deliver { peer, index } => channel.deliver(*peer, *index)?,
                Op::Duplicate { peer, index } => channel.duplicate(*peer, *index),
                Op::Drop { peer, index } => channel.drop_one(*peer, *index),
                Op::Quiesce => channel.quiesce()?,
            }
        }
        channel.quiesce()?;
    }

    /// yrs 0.26.0 through 0.27.2 placed an insert at index 0 by comparing
    /// client ids instead of anchoring it to the start of the type, so a peer
    /// with the LARGER client id prepending to text it had already received
    /// landed at index 1 on every peer (y-crdt #636, fixed by ed78a05 in
    /// 0.27.3). The fix was in `Branch::insert_at`, on the XML path, so a root
    /// text on 0.27.4 has to pass this; the shape is cheap and specific enough
    /// to be worth stating separately from the channel above.
    ///
    /// Both id orderings are run for every case, because the bug was a
    /// tiebreak: whichever peer holds the larger id is the one that used to
    /// land in the wrong place.
    #[test]
    fn sequential_prepend_keeps_index_0(
        pair in distinct_client_ids(2),
        peer_count in 2usize..=3,
        third in super::helpers::client_id(),
        first in mixed_chunk(),
        second in mixed_chunk(),
    ) {
        prop_assume!(!pair.contains(&third));
        for order in [[pair[0], pair[1]], [pair[1], pair[0]]] {
            let mut clients = order.to_vec();
            if peer_count == 3 {
                clients.push(third);
            }
            for at_end in [false, true] {
                let peers: Vec<YrsDoc> =
                    clients.iter().map(|c| doc_with(*c, false)).collect();
                let texts: Vec<_> = peers.iter().map(root_text).collect();

                // A inserts, and everyone receives it.
                let from_a = with_txn(&peers[0], |txn| {
                    texts[0].insert(txn, 0, first.clone());
                    peers[0].encode_diff_v1(txn, vec![]).unwrap()
                });
                for i in 1..peers.len() {
                    with_txn(&peers[i], |txn| {
                        txn.transaction_apply_update(from_a.clone()).unwrap()
                    });
                }

                // B then edits the text it has already received: sequential,
                // not concurrent. At index 0, or at the very end.
                let seen_b = with_txn(&peers[1], |txn| txn.transaction_state_vector());
                let from_b = with_txn(&peers[1], |txn| {
                    let offset = if at_end { texts[1].length(txn) } else { 0 };
                    texts[1].insert(txn, offset, second.clone());
                    peers[1].encode_diff_v1(txn, seen_b).unwrap()
                });
                for i in 0..peers.len() {
                    if i != 1 {
                        with_txn(&peers[i], |txn| {
                            txn.transaction_apply_update(from_b.clone()).unwrap()
                        });
                    }
                }

                for i in 0..peers.len() {
                    let rendered = with_txn(&peers[i], |txn| texts[i].get_string(txn));
                    if at_end {
                        prop_assert!(
                            rendered.ends_with(&second),
                            "peer {} rendered {:?}, which does not end with {:?}",
                            i, rendered, second
                        );
                    } else {
                        prop_assert!(
                            rendered.starts_with(&second),
                            "peer {} rendered {:?}, which does not start with {:?}",
                            i, rendered, second
                        );
                    }
                    prop_assert_eq!(rendered.chars().count(), first.chars().count() + second.chars().count());
                }
            }
        }
    }
}

/// Four sequential updates from ONE author, delivered with the second one
/// last. yrs 0.27.4 does not re-integrate the update that was waiting on it:
/// the document ends holding "ZXZ" at clock 3 with `has_missing_updates()`
/// true, where every one of the four updates has been delivered and the text
/// should be "ZXZZ" at clock 4. Delivering the stuck update a second time
/// repairs it, and so does an ordinary sync round (a diff against the peer's
/// state vector), which is why the channel property above ends with one and
/// does not catch this.
///
/// IGNORED because it fails on the core this fork pins. It is here as the
/// reproducer, not as a gate. The same sequence loses the update outright on
/// pycrdt 0.14.1 and 0.14.4, where it is not even reported as missing.
#[test]
#[ignore = "fails on yrs 0.27.4: a pending update is not re-integrated when its dependency arrives"]
fn reordered_same_author_updates_are_all_integrated() {
    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    // Client 2147483647 on root text "prompt": insert(0,"X"), insert(1,"Z"),
    // insert(0,"Z"), insert(3,"Z"), which renders "ZXZZ".
    let updates = [
        unhex("0101ffffffff070004010670726f6d7074015800"),
        unhex("0101ffffffff070184ffffffff0700015a00"),
        unhex("0101ffffffff070244ffffffff0700015a00"),
        unhex("0101ffffffff070384ffffffff0701015a00"),
    ];

    for order in [[0, 2, 3, 1], [0, 3, 2, 1]] {
        let doc = YrsDoc::new();
        let text = root_text(&doc);
        let (rendered, state, missing) = with_txn(&doc, |txn| {
            for index in order {
                txn.transaction_apply_update(updates[index].clone())
                    .unwrap();
            }
            (
                text.get_string(txn),
                state_map(txn),
                txn.has_missing_updates(),
            )
        });
        assert_eq!(rendered, "ZXZZ", "order {order:?} lost an update");
        assert_eq!(state, BTreeMap::from([(2_147_483_647u64, 4u32)]));
        assert!(!missing);
    }
}

/// A peer that has been handed every update, redelivered what was dropped and
/// then resynced against its neighbour, holding the same text and the same
/// state vector as that neighbour, still answers `has_missing_updates()` with
/// true. It is holding a `PendingUpdate` whose `missing` clock its own store
/// has already reached, which `apply_update` never retries and never clears:
/// step 3 of that function retries only when the missing clock is strictly
/// below `store.blocks.get_clock(client)`, so equality means never.
///
/// The program below is the shrunk case, replayed through the same channel the
/// property above uses. Two peers, one of them (client 1) doing all the
/// composing, one update dropped in flight and offered again at quiescence.
/// At the end both peers render the same text and report the same state
/// vector, and the observed store on the peer that reports something missing
/// holds:
///
/// ```text
/// pending: PendingUpdate {
///     update: { ClientID(1): [(<1#6> len 2, origin-r <1#2>), (<1#8> len 1,
///               origin-r <1#6>), (<1#9> len 4, origin-l <1#8>, origin-r <1#6>)] },
///     missing: StateVector({ClientID(1): 14}),
/// }
/// ```
///
/// IGNORED because it fails on the core this fork pins. It is here as the
/// reproducer, not as a gate. It also matters beyond tidiness: with
/// `has_missing_updates` on the UDL, a caller that treats it as
/// "am I behind" would resync forever on a document that is already complete.
#[test]
#[ignore = "fails on yrs 0.27.4: a pending record whose missing clock the store has reached is never cleared"]
fn a_resynced_peer_reports_nothing_missing() {
    // Peer 0 composes everything; peer 1 loses one update in flight and is
    // offered it again at quiescence.
    let insert = |pos: usize, chunk: &str| Op::Edit {
        peer: 0,
        edit: Edit::Insert {
            pos,
            chunk: chunk.to_string(),
        },
    };
    let delete = |pos: usize, len: usize| Op::Edit {
        peer: 0,
        edit: Edit::Delete { pos, len },
    };
    let program = [
        insert(0, "aa"),
        insert(0, "aaaa"),
        insert(0, "aa"),
        Op::Drop { peer: 1, index: 1 },
        insert(0, "a"),
        delete(0, 0),
        insert(0, "aaaa"),
        delete(0, 2),
        insert(8, "a"),
    ];

    let mut channel = Channel::new(&[1, 2_147_483_647]);
    for op in &program {
        match op {
            Op::Edit { peer, edit } => channel.edit(*peer, edit),
            Op::Deliver { peer, index } => channel.deliver(*peer, *index).unwrap(),
            Op::Duplicate { peer, index } => channel.duplicate(*peer, *index),
            Op::Drop { peer, index } => channel.drop_one(*peer, *index),
            Op::Quiesce => channel.quiesce().unwrap(),
        }
    }
    // Redelivery, drain, resync. This asserts convergence of text and state
    // vector, and it passes: the peers do agree.
    channel.quiesce().expect("the peers converge");

    for (i, peer) in channel.peers.iter().enumerate() {
        let view = observe(&peer.doc);
        assert!(
            !view.missing,
            "peer {i} agrees with the others on {:?} and {:?} and still reports a missing update",
            view.text, view.state
        );
    }
}
