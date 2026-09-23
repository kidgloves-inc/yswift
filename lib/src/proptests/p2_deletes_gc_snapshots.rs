//! P2: deletes, garbage collection and snapshots.
//!
//! This is a compacting server's daily path: a document is served as a
//! snapshot of the whole document plus the updates that came after it, and a
//! peer joining that way has to end up where a peer that watched every update
//! go by ended up. Deletes are what makes it interesting. With garbage
//! collection on (the default) a deleted run stops being an item and becomes a
//! tombstone, so the snapshot a late joiner gets is not the same block set the
//! early peer assembled, and the delete set has to carry the difference. yrs
//! 0.27.2 fixed a delete set that was lost after `apply_update`, which is
//! exactly the failure this property would show: the late joiner's text would
//! carry back text the early peer had deleted.
//!
//! The property is run with the default garbage collection and with
//! `skip_gc = true`, because the two produce different block sets from the
//! same history and both have to converge.

use proptest::prelude::*;

use super::helpers::{
    doc_with, first_char_units, observe, plan, plan_for, root_text, run, state_map, with_txn,
};
use crate::doc::YrsDoc;

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// A late joiner given a snapshot plus the tail reaches the same document
    /// as the peer that produced them, and as a peer that was fed every update
    /// one at a time.
    #[test]
    fn a_snapshot_plus_the_tail_equals_the_whole_history(
        clients in super::helpers::distinct_client_ids(2),
        skip_gc in any::<bool>(),
        edits_a in super::helpers::mixed_edits(2..12),
        edits_b in super::helpers::mixed_edits(0..6),
        cut in 0usize..24,
    ) {
        let a = doc_with(clients[0], skip_gc);
        let a_text = root_text(&a);
        let b = doc_with(clients[1], false);
        let b_text = root_text(&b);

        // Every update A emits, in order: its own edits and what it learned
        // from B, each a diff against A's state before that step.
        let mut stream: Vec<Vec<u8>> = Vec::new();
        let mut seen = with_txn(&a, |txn| txn.transaction_state_vector());
        let mut seen_b = with_txn(&b, |txn| txn.transaction_state_vector());

        let steps = edits_a.len().max(edits_b.len());
        for i in 0..steps {
            let mut mutated = false;
            if let Some(edit) = edits_a.get(i) {
                mutated |= with_txn(&a, |txn| {
                    match plan_for(&a_text.get_string(txn), edit) {
                        Some(planned) => {
                            run(&a_text, txn, &planned);
                            true
                        }
                        None => false,
                    }
                });
            }
            // B edits without having seen A, so its blocks arrive concurrent.
            if let Some(edit) = edits_b.get(i) {
                let update = with_txn(&b, |txn| {
                    plan_for(&b_text.get_string(txn), edit).map(|planned| {
                        run(&b_text, txn, &planned);
                        b.encode_diff_v1(txn, seen_b.clone()).unwrap()
                    })
                });
                if let Some(update) = update {
                    seen_b = with_txn(&b, |txn| txn.transaction_state_vector());
                    with_txn(&a, |txn| txn.transaction_apply_update(update).unwrap());
                    mutated = true;
                }
            }
            if mutated {
                let update = with_txn(&a, |txn| a.encode_diff_v1(txn, seen.clone()).unwrap());
                seen = with_txn(&a, |txn| txn.transaction_state_vector());
                stream.push(update);
            }
        }
        prop_assume!(!stream.is_empty());

        // The peer that watched every update go by.
        let d = YrsDoc::new();
        let d_text = root_text(&d);
        for update in &stream {
            with_txn(&d, |txn| txn.transaction_apply_update(update.clone()).unwrap());
        }

        // The snapshot is taken part way through, which means replaying the
        // stream from the start up to the cut and then asking A's earlier self
        // for its full state. A only exists once, so the earlier self is a
        // peer built from the prefix.
        let cut = cut % stream.len();
        let snapshot_peer = YrsDoc::new();
        for update in &stream[..=cut] {
            with_txn(&snapshot_peer, |txn| {
                txn.transaction_apply_update(update.clone()).unwrap()
            });
        }
        let snapshot =
            with_txn(&snapshot_peer, |txn| txn.transaction_encode_state_as_update());

        // The late joiner: snapshot first, then the tail.
        let c = YrsDoc::new();
        let c_text = root_text(&c);
        with_txn(&c, |txn| txn.transaction_apply_update(snapshot).unwrap());
        for update in &stream[cut + 1..] {
            with_txn(&c, |txn| txn.transaction_apply_update(update.clone()).unwrap());
        }

        let view_a = with_txn(&a, |txn| (a_text.get_string(txn), state_map(txn)));
        let view_c = with_txn(&c, |txn| (c_text.get_string(txn), state_map(txn)));
        let view_d = with_txn(&d, |txn| (d_text.get_string(txn), state_map(txn)));

        prop_assert_eq!(&view_c.0, &view_a.0, "late joiner text");
        prop_assert_eq!(&view_c.1, &view_a.1, "late joiner state vector");
        prop_assert_eq!(&view_d.0, &view_c.0, "streamed peer text");
        prop_assert_eq!(&view_d.1, &view_c.1, "streamed peer state vector");
        prop_assert!(!observe(&a).missing);
        prop_assert!(!observe(&c).missing);
        prop_assert!(!observe(&d).missing);
    }

    /// Deleted text does not come back. A run of deletes, then a snapshot, then
    /// a fresh peer from that snapshot alone: what the peer renders is what the
    /// author renders, tombstones and all. With garbage collection on, the
    /// deleted items are gone from the snapshot entirely and only the delete
    /// set says they ever existed.
    #[test]
    fn a_snapshot_carries_the_deletes(
        client in super::helpers::client_id(),
        skip_gc in any::<bool>(),
        edits in super::helpers::mixed_edits(2..14),
    ) {
        let a = doc_with(client, skip_gc);
        let text = root_text(&a);
        let mut model = String::new();
        for edit in &edits {
            if let Some(planned) = plan(&mut model, edit) {
                with_txn(&a, |txn| run(&text, txn, &planned));
            }
        }
        // One more delete, to be sure at least one exists.
        let deleted = with_txn(&a, |txn| {
            // A whole character, never half of a surrogate pair.
            let units = first_char_units(&text.get_string(txn));
            if units > 0 {
                text.remove_range(txn, 0, units);
            }
            text.get_string(txn)
        });
        let snapshot = with_txn(&a, |txn| txn.transaction_encode_state_as_update());

        let peer = YrsDoc::new();
        let peer_text = root_text(&peer);
        let view = with_txn(&peer, |txn| {
            txn.transaction_apply_update(snapshot).unwrap();
            (peer_text.get_string(txn), state_map(txn))
        });
        let author = with_txn(&a, |txn| (text.get_string(txn), state_map(txn)));
        prop_assert_eq!(&view.0, &deleted);
        prop_assert_eq!(&view.0, &author.0);
        prop_assert_eq!(&view.1, &author.1);
        prop_assert!(!observe(&peer).missing);
    }
}
