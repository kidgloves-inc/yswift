//! P8: a third writer class behind a forwarding server's client-id gate.
//!
//! The first two writer classes are peers of each other: each core peer mints
//! its own client id and the wire takes its word for it. A writer whose client
//! id is assigned by the server rather than drawn by its own core is not one.
//! The server draws its id, records it for the session, and refuses any update
//! whose struct section names a different author — so a writer that minted
//! its own id could not fork a document the way the retired decoder's
//! arithmetic did with the u32 client-id truncation in yrs 0.18's V1 decoder.
//!
//! The positive is that the gate costs a well-bound session nothing: it never
//! refuses one of its updates, all three peers converge under a lossy
//! schedule, and no peer ends up holding a block under an id nobody issued.
//! The negative is the truncation's shape arriving by a second road — a
//! session whose binding mangled its id — and it says the mangled writer is
//! stopped at the server rather than in the two core peers' documents.
//!
//! Quiescence here is a replay of the server's LOG, not the peer-to-peer diff
//! round P1 settles with, and the difference is this module's whole subject: a
//! session's bytes reach another peer only by passing the gate, so a diff
//! composed by the session and applied directly to a peer would be the test
//! walking around the thing it is testing. The log is every update the server
//! admitted, in admission order, which is what a server replays to a peer on
//! attach — and replaying it is also what repairs the pending update this core
//! does not re-integrate when its dependency arrives late (P1's `reordered_…`).
//!
//! The gate is read here with yrs's own decoder, for the reason `helpers.rs`
//! gives for `UpdateSpan`: the binding exposes no view of an update's authors,
//! and the alternative is a second decoder. The same rule is stated over the
//! same bytes against pycrdt as well.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use yrs::updates::decoder::Decode;
use yrs::Update;

use super::helpers::{
    ascii_chunk, ascii_edits, distinct_client_ids, doc_with, observe, plan_for, root_text, run,
    with_txn, Edit, CORNER_CLIENT_IDS,
};
use super::p6_retired_decoder::modelled;
use crate::doc::YrsDoc;
use crate::text::YrsText;

/// A forwarding server's client-id gate (it admits an update only when every
/// block in its struct section is filed under the client id the server
/// assigned to the sender): true iff every client the update's struct section
/// names is `client`.
///
/// Two shapes pass and are worth naming. An update whose struct section names
/// nobody is admitted, and a delete-only update is the ordinary one: a delete
/// set names the AUTHORS of the text being removed, not the author of the
/// removal, so refusing on it would refuse a session for deleting text it can
/// see. And bytes that will not decode are refused rather than admitted — an
/// update nobody can read cannot be shown to be this session's.
///
/// This statement decodes with yrs's own `Update::decode_v1`, whose Any
/// reader recurses without a bound, so a value nested thousands deep
/// overflows the stack here rather than answering. A server enforcing this
/// gate over untrusted bytes has to bound the nesting depth (at 64, say)
/// before any core sees a session's bytes; this test statement only
/// ever sees updates its own peers composed.
fn update_is_only_from(update: &[u8], client: u64) -> bool {
    let Ok(decoded) = Update::decode_v1(update) else {
        return false;
    };
    decoded
        .insertions(true)
        .iter()
        .all(|(author, _)| author.get() == client)
}

/// An update nobody can read cannot be shown to be this session's, so the
/// gate's answer to one is no. The pycrdt statement of the gate refuses the
/// same two inputs.
#[test]
fn the_gate_refuses_bytes_it_cannot_decode() {
    assert!(!update_is_only_from(b"\x01\x02\x03garbage", 1));
    assert!(!update_is_only_from(&[], 1));
}

/// A session that inserts and deletes in the same breath still wrote the
/// blocks it deleted, and the gate still has to see them.
///
/// A state encoded at rest carries a deleted run in place of the string it
/// replaced: a block with a client id and no live content — but that shape
/// holds whichever way `Update::insertions`'s "include deleted" argument is
/// passed, since a `remove_range` leaves the ContentDeleted run either way.
/// It is `a_garbage_collected_block_still_names_its_author` below that pins
/// the flag: only a fully garbage-collected block disappears from
/// `insertions(false)`.
#[test]
fn the_gate_reads_an_author_whose_every_block_is_deleted() {
    let author = doc_with(7, false);
    let text = root_text(&author);
    with_txn(&author, |txn| text.insert(txn, 0, "hello".to_string()));
    with_txn(&author, |txn| text.remove_range(txn, 0, 5));
    let update = with_txn(&author, |txn| author.encode_diff_v1(txn, vec![]).unwrap());

    assert!(
        update_is_only_from(&update, 7),
        "update {}",
        super::helpers::hex(&update)
    );
    assert!(!update_is_only_from(&update, 8));
}

/// A one-block update whose block is a garbage-collected run: one client (7),
/// one struct at clock 0, `info` byte 0 for GC, a length, and an empty delete
/// set. Written by hand because neither core reaches this shape from a text
/// document — yrs replaces a deleted item's content with a Deleted run and
/// only collects the block outright when the item's own parent type was
/// deleted too — and the server decodes whatever a session sends rather than
/// whatever the document is supposed to hold. The pycrdt statement of the gate
/// pins the same seven bytes.
const GC_BLOCK_UPDATE: [u8; 7] = [0x01, 0x01, 0x07, 0x00, 0x00, 0x05, 0x00];

/// A collected block names the client that wrote it, and the gate reads it.
///
/// `Update::insertions` reports garbage-collected blocks only when asked to
/// include deleted ones, so a gate that asked it the other way round would
/// read this update as naming nobody and admit it from any session at all.
/// The mutation pass found exactly that; this is what it costs to close.
#[test]
fn a_garbage_collected_block_still_names_its_author() {
    assert!(update_is_only_from(&GC_BLOCK_UPDATE, 7));
    assert!(!update_is_only_from(&GC_BLOCK_UPDATE, 8));
}

/// A one-block update whose block is a Skip: one client (11), one struct at
/// clock 0, an `info` byte of 0x0A for Skip, a length, and an empty delete
/// set. The pycrdt statement of the gate pins the same seven bytes.
const SKIP_UPDATE: [u8; 7] = [0x01, 0x01, 0x0B, 0x00, 0x0A, 0x04, 0x00];

/// A Skip declares a hole in someone else's clock range and supplies no
/// content, so it names nobody — which means the gate passes it against ANY
/// client id, correctly: refusing it would refuse a session for a hole it
/// did not create.
#[test]
fn a_skip_names_nobody() {
    assert!(update_is_only_from(&SKIP_UPDATE, 7));
    assert!(update_is_only_from(&SKIP_UPDATE, 11));
}

/// The gate's hardest case: an update filed under the assigned id AND
/// somebody else's. Every other test here offers the gate one author at a time, so a
/// gate that answered from the first client it decoded would pass all of them
/// — and this is the update it would have to pass for a block to land under a
/// client the server never issued.
///
/// It is asked about BOTH authors, because answering from the first decoded
/// client is right about one of them by luck and the block order is not this
/// test's to fix. The shape is not exotic either: it is what a writer sending
/// its whole state rather than its own transaction's update would put on the
/// wire.
#[test]
fn the_gate_refuses_an_update_that_carries_a_second_author() {
    let assigned = CORNER_CLIENT_IDS[5];
    let other = CORNER_CLIENT_IDS[0];
    let session = doc_with(assigned, false);
    let outsider = doc_with(other, false);
    let session_text = root_text(&session);
    let outsider_text = root_text(&outsider);

    let theirs = with_txn(&outsider, |txn| {
        outsider_text.insert(txn, 0, "theirs".to_string());
        outsider.encode_diff_v1(txn, vec![]).unwrap()
    });
    let whole = with_txn(&session, |txn| {
        session_text.insert(txn, 0, "mine".to_string());
        txn.transaction_apply_update(theirs).unwrap();
        session.encode_diff_v1(txn, vec![]).unwrap()
    });

    let authors: Vec<u64> = Update::decode_v1(&whole)
        .unwrap()
        .insertions(true)
        .iter()
        .map(|(author, _)| author.get())
        .collect();
    assert_eq!(authors.len(), 2, "authors {authors:?}");
    assert!(!update_is_only_from(&whole, assigned));
    assert!(!update_is_only_from(&whole, other));
}

/// One instruction of a generated program, P1's vocabulary: a peer edits, and
/// a channel that delivers out of order, duplicates and drops. Peer and inbox
/// indices are raw draws, taken modulo the live counts when interpreted.
#[derive(Debug, Clone)]
enum Op {
    Edit { peer: usize, edit: Edit },
    Deliver { peer: usize, index: usize },
    Duplicate { peer: usize, index: usize },
    Drop { peer: usize, index: usize },
}

fn op(edit: BoxedStrategy<Edit>) -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0usize..3, edit).prop_map(|(peer, edit)| Op::Edit { peer, edit }),
        6 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Deliver { peer, index }),
        1 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Duplicate { peer, index }),
        2 => (0usize..3, 0usize..8).prop_map(|(peer, index)| Op::Drop { peer, index }),
    ]
}

/// Inserts and deletes, which is what the positive runs.
fn any_edit() -> BoxedStrategy<Edit> {
    ascii_edits(1..2)
        .prop_map(|mut edits| edits.remove(0))
        .boxed()
}

/// Inserts only, which is what the negative runs — and not for weight: a
/// delete-only update names no author in its struct section, so the gate
/// admits it by design (see `update_is_only_from`) and "every update refused"
/// would be false for a reason that has nothing to do with the mangled id.
fn insert_edit() -> BoxedStrategy<Edit> {
    (0usize..64, ascii_chunk())
        .prop_map(|(pos, chunk)| Edit::Insert { pos, chunk })
        .boxed()
}

struct Peer {
    doc: YrsDoc,
    text: Arc<YrsText>,
}

/// Three peers on a lossy channel, with the gate on everything the session
/// peer sends.
struct Server {
    peers: Vec<Peer>,
    inboxes: Vec<Vec<Vec<u8>>>,
    dropped: Vec<(usize, Vec<u8>)>,
    /// Every update the server admitted, in admission order.
    log: Vec<Vec<u8>>,
    session: usize,
    assigned: u64,
    admitted: usize,
    refused: usize,
}

impl Server {
    fn new(clients: &[u64], session: usize, assigned: u64) -> Self {
        let peers: Vec<Peer> = clients
            .iter()
            .map(|client| {
                let doc = doc_with(*client, false);
                let text = root_text(&doc);
                Peer { doc, text }
            })
            .collect();
        let count = peers.len();
        Server {
            peers,
            inboxes: vec![Vec::new(); count],
            dropped: Vec::new(),
            log: Vec::new(),
            session,
            assigned,
            admitted: 0,
            refused: 0,
        }
    }

    fn send(&mut self, peer: usize, update: Vec<u8>) {
        if update.is_empty() {
            return;
        }
        if peer == self.session {
            if !update_is_only_from(&update, self.assigned) {
                self.refused += 1;
                return;
            }
            self.admitted += 1;
        }
        for other in 0..self.peers.len() {
            if other != peer {
                self.inboxes[other].push(update.clone());
            }
        }
        self.log.push(update);
    }

    /// A peer edits and sends what that edit produced.
    ///
    /// The diff is taken against the peer's state vector as it is at the
    /// moment of the edit, not against the one it last composed from, and P1's
    /// channel differs here on purpose. A peer's outbound bytes on that
    /// channel may legitimately forward another peer's blocks; a session's may
    /// not, because the server's gate is about authorship and the session
    /// sends what its own transaction produced and nothing else. Composing against a
    /// stale vector would put a core peer's blocks in the session's update and
    /// have the gate refuse it for the test's bookkeeping rather than for the
    /// session's id. The pycrdt mirror of this channel composes the same way.
    fn edit(&mut self, peer: usize, edit: &Edit) {
        let peer = peer % self.peers.len();
        let update = {
            let p = &self.peers[peer];
            with_txn(&p.doc, |txn| {
                let before = txn.transaction_state_vector();
                plan_for(&p.text.get_string(txn), edit).map(|planned| {
                    run(&p.text, txn, &planned);
                    p.doc.encode_diff_v1(txn, before).unwrap()
                })
            })
        };
        let Some(update) = update else { return };
        self.send(peer, update);
    }

    fn deliver(&mut self, peer: usize, index: usize) {
        let peer = peer % self.peers.len();
        if self.inboxes[peer].is_empty() {
            return;
        }
        let index = index % self.inboxes[peer].len();
        let update = self.inboxes[peer].remove(index);
        self.apply(peer, update);
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
        let update = self.inboxes[peer].remove(index);
        self.dropped.push((peer, update));
    }

    fn apply(&mut self, peer: usize, update: Vec<u8>) {
        with_txn(&self.peers[peer].doc, |txn| {
            txn.transaction_apply_update(update).unwrap()
        });
    }

    /// Offer everything again, drain every inbox, then replay the server's log.
    fn settle(&mut self) {
        for (peer, update) in std::mem::take(&mut self.dropped) {
            self.inboxes[peer].push(update);
        }
        for peer in 0..self.peers.len() {
            let mut queue = std::mem::take(&mut self.inboxes[peer]);
            queue.extend(self.log.iter().cloned());
            for update in queue {
                self.apply(peer, update);
            }
        }
    }

    fn run(&mut self, program: &[Op]) {
        for op in program {
            match op {
                Op::Edit { peer, edit } => self.edit(*peer, edit),
                Op::Deliver { peer, index } => self.deliver(*peer, *index),
                Op::Duplicate { peer, index } => self.duplicate(*peer, *index),
                Op::Drop { peer, index } => self.drop_one(*peer, *index),
            }
        }
        self.settle();
    }

    /// The text and state vector of each peer in `which`.
    fn views(&self, which: std::ops::Range<usize>) -> Vec<(String, BTreeMap<u64, u32>)> {
        which
            .map(|i| {
                let view = observe(&self.peers[i].doc);
                (view.text, view.state)
            })
            .collect()
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// A writer on the id the server assigned it is an ordinary
    /// third peer: the gate never refuses it, the three converge under a lossy
    /// schedule, and nobody ends up holding a block under an unissued id.
    ///
    /// `assigned` is drawn over the domain a server's id draw issues — uniform
    /// below 2^53, zero included, because a mask that makes the draw uniform
    /// admits it. The issued set the last assertion checks
    /// against is the three ids this case drew, never anything read back out
    /// of the documents: a peer that invented an id would otherwise be asked
    /// to confirm its own invention.
    ///
    /// The opening edit is the session's, so `admitted` is never zero and "the
    /// gate refuses nothing" has something to be true of.
    #[test]
    fn a_session_on_its_assigned_id_converges_with_the_two_core_peers(
        cores in distinct_client_ids(2),
        assigned in 0u64..(1u64 << 53),
        opening in ascii_chunk(),
        program in proptest::collection::vec(op(any_edit()), 4..40),
    ) {
        prop_assume!(!cores.contains(&assigned));
        let mut server = Server::new(&[cores[0], cores[1], assigned], 2, assigned);
        server.edit(2, &Edit::Insert { pos: 0, chunk: opening });
        server.run(&program);

        prop_assert_eq!(
            server.refused, 0,
            "the server refused an update the session composed under the id it was assigned ({})",
            assigned
        );
        prop_assert!(server.admitted > 0, "the session's opening edit never reached the gate");

        let views = server.views(0..3);
        for i in 1..views.len() {
            prop_assert_eq!(&views[i].0, &views[0].0, "peers disagree once the server has replayed");
            prop_assert_eq!(&views[i].1, &views[0].1, "peers agree on text but not on state vector");
        }
        let issued = [cores[0], cores[1], assigned];
        for (i, view) in views.iter().enumerate() {
            for client in view.1.keys() {
                prop_assert!(
                    issued.contains(client),
                    "peer {} holds blocks under {}, which nobody issued; the three ids are {:?}",
                    i, client, issued
                );
            }
        }
    }

    /// The u32 client-id truncation arriving by a second road, and the gate
    /// standing in it.
    ///
    /// The session was assigned an id and writes under a different one — here
    /// the image the retired yswift decoder produced for it, which is a
    /// mangling that really happened and therefore a better negative than an
    /// arbitrary wrong number. Every update it composes names that image, so
    /// every one is refused, and the two core peers converge holding no client
    /// id but their own two: the fork does not happen in their documents, it
    /// does not happen at all.
    ///
    /// `assigned` is drawn at or above 2^32 because below it the retired
    /// decoder carried an id whole (P6's table), so there would be nothing
    /// mangled and the gate would rightly admit the session. The image is also
    /// required to differ from both core ids — one that collided with a core
    /// peer's id would file the session's blocks under an issued client and
    /// the last assertion would pass while the fork happened.
    #[test]
    fn a_session_whose_binding_mangled_its_id_never_reaches_the_core_peers(
        cores in distinct_client_ids(2),
        assigned in (1u64 << 32)..(1u64 << 53),
        opening in ascii_chunk(),
        program in proptest::collection::vec(op(insert_edit()), 4..40),
    ) {
        let mangled = modelled(assigned);
        prop_assume!(!cores.contains(&assigned) && !cores.contains(&mangled));
        let mut server = Server::new(&[cores[0], cores[1], mangled], 2, assigned);
        server.edit(2, &Edit::Insert { pos: 0, chunk: opening });
        server.run(&program);

        prop_assert_eq!(
            server.admitted, 0,
            "the server admitted an update filed under {} from a session assigned {}",
            mangled, assigned
        );
        prop_assert!(server.refused > 0, "the session's opening edit never reached the gate");

        let views = server.views(0..2);
        prop_assert_eq!(&views[1].0, &views[0].0, "the two core peers disagree");
        prop_assert_eq!(&views[1].1, &views[0].1, "the two core peers agree on text but not on state vector");
        for (i, view) in views.iter().enumerate() {
            for client in view.1.keys() {
                prop_assert!(
                    cores.contains(client),
                    "core peer {} holds blocks under {}; the session was assigned {} and \
                     writes under {}",
                    i, client, assigned, mangled
                );
            }
        }
    }
}
