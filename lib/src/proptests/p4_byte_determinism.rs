//! P4: byte determinism.
//!
//! The interop corpus pins update bytes produced by one core and replayed by
//! the other. That is only worth pinning if the bytes are a function of the
//! block set rather than of how the block set came about, so this property
//! checks the claim live: the same client id making the same edits encodes to
//! the same bytes, and a document that learned a state by applying an update
//! re-encodes that state to the bytes it arrived as.
//!
//! It also carries the two fixed vectors the cross-core comparison uses,
//! both pinned to their exact bytes because the other core has reproduced
//! them: the first is a fork-produced update a pycrdt peer replays, the second,
//! scenario S, was computed here and in pycrdt independently and compared
//! before it was pinned, so the constants record an agreement rather than
//! this core's opinion of itself.

use proptest::prelude::*;

use super::helpers::{doc_with, hex, plan, root_text, run, with_txn, Edit};
use crate::doc::YrsDoc;

const PINNED_CLIENT: u64 = 6_968_031_897_510_372;

/// The pinned cross-core vector: client 6968031897510372 inserting "hello"
/// into the root text "prompt" in one transaction, diffed against the empty
/// state vector. Base64 AQHkw6PQtaywDAAEAQZwcm9tcHQFaGVsbG8A, which is what
/// the other core is asked to produce byte for byte.
const PINNED_UPDATE_HEX: &str = "0101e4c3a3d0b5acb00c0004010670726f6d70740568656c6c6f00";

#[test]
fn the_pinned_cross_core_vector_still_encodes_to_its_bytes() {
    let doc = doc_with(PINNED_CLIENT, false);
    let text = root_text(&doc);
    let bytes = with_txn(&doc, |txn| {
        text.insert(txn, 0, "hello".to_string());
        doc.encode_diff_v1(txn, vec![]).unwrap()
    });
    assert_eq!(hex(&bytes), PINNED_UPDATE_HEX);
}

/// Scenario S, the canonical case the two cores are compared on. One client,
/// two transactions, an insert, a delete that splits the first block and an
/// insert at the head, then the four artefacts a peer could be handed: the
/// first update, the second update as a diff, the full state, and the state
/// vector. pycrdt (0.14.1 on yrs 0.27.2 and 0.14.4 on 0.27.4) produces the
/// same four byte for byte.
///
/// The full state is encoded in a transaction of its own, after the one that
/// deleted has committed. That is the one place the two cores first seemed to
/// disagree: an encode taken inside the still-open transaction still carries
/// the deleted "el" as string content, because garbage collection runs at
/// commit, and it matches what pycrdt produces with `skip_gc` instead. A
/// snapshot at rest is what the corpus pins, on both sides.
const SCENARIO_S_U1_HEX: &str = "0101e9f788889a84dc010004010670726f6d70740568656c6c6f00";
const SCENARIO_S_U2_HEX: &str =
    "0101e9f788889a84dc010544e9f788889a84dc0100014101e9f788889a84dc01010102";
const SCENARIO_S_FULL_HEX: &str = "0104e9f788889a84dc010004010670726f6d7074016881e9f788889a84dc01000284e9f788889a84dc0102026c6f44e9f788889a84dc0100014101e9f788889a84dc01010102";
const SCENARIO_S_SV_HEX: &str = "01e9f788889a84dc0106";

#[test]
fn scenario_s_bytes() {
    let doc = doc_with(967_714_667_641_833, false);
    let text = root_text(&doc);

    let (u1, sv_after_t1) = with_txn(&doc, |txn| {
        text.insert(txn, 0, "hello".to_string());
        (
            doc.encode_diff_v1(txn, vec![]).unwrap(),
            txn.transaction_state_vector(),
        )
    });

    let (u2, rendered) = with_txn(&doc, |txn| {
        text.remove_range(txn, 1, 2);
        text.insert(txn, 0, "A".to_string());
        (
            doc.encode_diff_v1(txn, sv_after_t1).unwrap(),
            text.get_string(txn),
        )
    });
    let (full, sv) = with_txn(&doc, |txn| {
        (
            txn.transaction_encode_state_as_update(),
            txn.transaction_state_vector(),
        )
    });

    // "hello" without the units at [1, 3) is "hlo"; "A" at the head makes
    // "Ahlo".
    assert_eq!(rendered, "Ahlo");
    assert_eq!(hex(&u1), SCENARIO_S_U1_HEX);
    assert_eq!(hex(&u2), SCENARIO_S_U2_HEX);
    assert_eq!(hex(&full), SCENARIO_S_FULL_HEX);
    assert_eq!(hex(&sv), SCENARIO_S_SV_HEX);

    // A peer that replays U1 then U2 reaches the same text as one that is
    // handed FULL, which is what makes the four artefacts one scenario.
    let replayed = YrsDoc::new();
    let replayed_text = root_text(&replayed);
    let a = with_txn(&replayed, |txn| {
        txn.transaction_apply_update(u1).unwrap();
        txn.transaction_apply_update(u2).unwrap();
        replayed_text.get_string(txn)
    });
    let whole = YrsDoc::new();
    let whole_text = root_text(&whole);
    let b = with_txn(&whole, |txn| {
        txn.transaction_apply_update(full).unwrap();
        whole_text.get_string(txn)
    });
    assert_eq!(a, "Ahlo");
    assert_eq!(b, "Ahlo");
}

/// `probe::seed_update` is built with `yrs` directly, because a fuzz target
/// needs a known good update before there is a document to make one with. That
/// is the only place in this tier where the binding is not the author, so the
/// bytes it produces are pinned to scenario S's first update, which the
/// binding does author. If the two ever part company, the probe module is
/// seeding the fuzzers with something the binding would not have written.
#[test]
fn the_probe_seed_is_scenario_s_first_update() {
    assert_eq!(hex(&crate::probe::seed_update()), SCENARIO_S_U1_HEX);
}

fn compose(client: u64, edits: &[Edit]) -> (Vec<u8>, Vec<u8>, String) {
    let doc = doc_with(client, false);
    let text = root_text(&doc);
    let mut model = String::new();
    for edit in edits {
        if let Some(planned) = plan(&mut model, edit) {
            with_txn(&doc, |txn| run(&text, txn, &planned));
        }
    }
    with_txn(&doc, |txn| {
        (
            doc.encode_diff_v1(txn, vec![]).unwrap(),
            txn.transaction_encode_state_as_update(),
            text.get_string(txn),
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    /// (i) The bytes are a function of the client id and the edits. Two
    /// documents that never met, given the same id and the same run of edits,
    /// encode to the same diff and the same full state.
    #[test]
    fn the_same_edits_under_the_same_client_id_encode_identically(
        client in super::helpers::client_id(),
        edits in super::helpers::mixed_edits(1..12),
    ) {
        let (diff_a, full_a, text_a) = compose(client, &edits);
        let (diff_b, full_b, text_b) = compose(client, &edits);
        prop_assert_eq!(text_a, text_b);
        prop_assert_eq!(hex(&diff_a), hex(&diff_b));
        prop_assert_eq!(hex(&full_a), hex(&full_b));
    }

    /// (ii) Re-encoding is stable across the wire: a fresh document handed A's
    /// full state encodes that state back to the same bytes. If this failed,
    /// the corpus would be pinning a peer's history rather than its state, and
    /// the cross-core comparison would be meaningless.
    #[test]
    fn a_document_re_encodes_the_full_state_it_was_given(
        client in super::helpers::client_id(),
        edits in super::helpers::mixed_edits(1..12),
    ) {
        let (_, full_a, text_a) = compose(client, &edits);

        let peer = YrsDoc::new();
        let peer_text = root_text(&peer);
        let (full_b, text_b) = with_txn(&peer, |txn| {
            txn.transaction_apply_update(full_a.clone()).unwrap();
            (txn.transaction_encode_state_as_update(), peer_text.get_string(txn))
        });

        prop_assert_eq!(text_a, text_b);
        prop_assert_eq!(hex(&full_a), hex(&full_b));
    }
}
