//! P3: partial state vectors.
//!
//! A peer that is behind says so with a state vector, and the answer it gets
//! back has to be exactly what it lacks: applying the diff must leave it
//! holding the same text as the peer that produced it and waiting for nothing.
//! The hard case is a state vector with a hole in the middle of a client's
//! sequence, which is the shape the skip-item paths handle and where yrs
//! 0.27.1 fixed a slicing underflow and a swallowed pending update. This
//! property builds that hole on purpose: a peer takes a prefix of a run of
//! updates, then a strictly later one, and only then asks for the difference.

use proptest::prelude::*;

use super::helpers::{observe, plan, root_text, run, state_map, with_txn, Edit, UpdateSpan};
use crate::doc::YrsDoc;

/// Author a run of updates, each the diff since the author's state before it.
fn author_run(client: u64, edits: &[Edit]) -> (YrsDoc, Vec<Vec<u8>>) {
    let doc = super::helpers::doc_with(client, false);
    let text = root_text(&doc);
    let mut model = String::new();
    let mut seen = with_txn(&doc, |txn| txn.transaction_state_vector());
    let mut updates = Vec::new();
    for edit in edits {
        let Some(planned) = plan(&mut model, edit) else {
            continue;
        };
        let update = with_txn(&doc, |txn| {
            run(&text, txn, &planned);
            doc.encode_diff_v1(txn, seen.clone()).unwrap()
        });
        seen = with_txn(&doc, |txn| txn.transaction_state_vector());
        updates.push(update);
    }
    (doc, updates)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// A peer holding a prefix, handed a strictly later update first, is
    /// exactly repaired by a diff against the state vector it can honestly
    /// report.
    #[test]
    fn a_diff_against_a_partial_state_vector_repairs_the_peer(
        client in super::helpers::client_id(),
        edits in super::helpers::mixed_edits(3..14),
        prefix in 0usize..12,
        skip in 0usize..12,
    ) {
        let (author, updates) = author_run(client, &edits);
        prop_assume!(updates.len() >= 3);

        // The prefix the peer already has, and a strictly later update that
        // leaves a hole behind it.
        let k = prefix % (updates.len() - 2);
        let j = k + 2 + (skip % (updates.len() - k - 2));

        let peer = YrsDoc::new();
        let peer_text = root_text(&peer);
        for update in &updates[..k] {
            with_txn(&peer, |txn| txn.transaction_apply_update(update.clone()).unwrap());
        }

        // The later update leaves a hole. What the peer must NOT do is
        // integrate it: its blocks stay outside the peer's state vector until
        // the hole is filled.
        let state_before = with_txn(&peer, |txn| state_map(txn));
        let span = UpdateSpan::decode(&updates[j]);
        let covered = span.covered_by(&state_before);
        with_txn(&peer, |txn| txn.transaction_apply_update(updates[j].clone()).unwrap());
        let state_after = with_txn(&peer, |txn| state_map(txn));
        let hole = match span.first_insert_clock(client) {
            Some(first) => state_before.get(&client).copied().unwrap_or(0) < first,
            None => false,
        };
        if hole {
            prop_assert!(
                !span.covered_by(&state_after),
                "an update past a hole in its author's own sequence must not be integrated"
            );
        }
        // `has_missing_updates` may only be true if something really is
        // outstanding. The converse does not hold on this core: a hole in one
        // client's own sequence is recorded as a Skip block in the store, not
        // as a pending update, so the flag stays false while the peer is
        // demonstrably behind. See the module note in `p1_lossy_channel`.
        if covered {
            prop_assert!(!observe(&peer).missing);
        }

        // What the peer can honestly say it has seen, which does not include
        // anything it is holding pending.
        let sv = with_txn(&peer, |txn| txn.transaction_state_vector());
        let diff = with_txn(&author, |txn| author.encode_diff_v1(txn, sv).unwrap());
        with_txn(&peer, |txn| txn.transaction_apply_update(diff).unwrap());

        let author_text = root_text(&author);
        let author_view = with_txn(&author, |txn| (author_text.get_string(txn), state_map(txn)));
        let peer_view = with_txn(&peer, |txn| (peer_text.get_string(txn), state_map(txn)));
        prop_assert_eq!(&peer_view.0, &author_view.0);
        prop_assert_eq!(&peer_view.1, &author_view.1);
        prop_assert!(!observe(&peer).missing);
    }

    /// A document's own diff against its own state vector adds nothing to it.
    /// The delete set still travels, so the update is not empty; applying it
    /// has to be a no-op all the same.
    #[test]
    fn a_documents_diff_against_itself_changes_nothing(
        client in super::helpers::client_id(),
        edits in super::helpers::mixed_edits(1..12),
    ) {
        let (doc, _) = author_run(client, &edits);
        let text = root_text(&doc);
        let before = with_txn(&doc, |txn| (text.get_string(txn), state_map(txn)));
        let sv = with_txn(&doc, |txn| txn.transaction_state_vector());
        let diff = with_txn(&doc, |txn| doc.encode_diff_v1(txn, sv).unwrap());
        with_txn(&doc, |txn| txn.transaction_apply_update(diff).unwrap());
        let after = with_txn(&doc, |txn| (text.get_string(txn), state_map(txn)));
        prop_assert_eq!(before, after);
        prop_assert!(!observe(&doc).missing);
    }

    /// The empty slice means "seen nothing" whatever the document holds. This
    /// is a unit test in `doc.rs` for one document; here it is a property over
    /// documents, because the equivalence is what lets the Swift side pass an
    /// empty array straight through.
    #[test]
    fn an_empty_slice_is_the_encoded_empty_state_vector(
        client in super::helpers::client_id(),
        edits in super::helpers::mixed_edits(1..12),
    ) {
        let (doc, _) = author_run(client, &edits);
        let (whole, from_zero) = with_txn(&doc, |txn| {
            (
                doc.encode_diff_v1(txn, vec![]).unwrap(),
                doc.encode_diff_v1(txn, vec![0]).unwrap(),
            )
        });
        prop_assert_eq!(whole, from_zero);
    }
}
