//! P5: offsets on real text.
//!
//! `YrsDoc::new` builds its document with `OffsetKind::Utf16`, so every index
//! and length crossing the FFI is a count of UTF-16 code units, which is what
//! Swift's `String.utf16` gives and what the Swift wrapper hands in. This
//! property holds the binding to that against a plain string model over text
//! that mixes ASCII, accented Latin, Cyrillic, CJK and non-BMP emoji, the last
//! of which costs two units per character.
//!
//! It stands guard over the 0.27.1 skip-item slicing underflow and the 0.27.3
//! missing right neighbour on an insert at index 0: both were failures of
//! where an edit lands, and both would show here as a model mismatch rather
//! than as an error.

use proptest::prelude::*;

use super::helpers::{observe, plan, root_text, run, with_txn};
use crate::doc::YrsDoc;

fn utf16_units(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// After every edit the document's text is the model's text and its length
    /// is the model's length in UTF-16 code units, and a peer fed each update
    /// as it was made ends at the same text.
    #[test]
    fn text_and_length_track_a_plain_string_model(edits in super::helpers::mixed_edits(1..16)) {
        let doc = YrsDoc::new();
        let text = root_text(&doc);
        let peer = YrsDoc::new();
        let peer_text = root_text(&peer);

        let mut model = String::new();
        let mut seen = with_txn(&doc, |txn| txn.transaction_state_vector());

        for edit in &edits {
            let Some(planned) = plan(&mut model, edit) else { continue };
            let (rendered, length, update) = with_txn(&doc, |txn| {
                run(&text, txn, &planned);
                (
                    text.get_string(txn),
                    text.length(txn),
                    doc.encode_diff_v1(txn, seen.clone()).unwrap(),
                )
            });

            prop_assert_eq!(&rendered, &model);
            prop_assert_eq!(length, utf16_units(&model));

            seen = with_txn(&doc, |txn| txn.transaction_state_vector());
            with_txn(&peer, |txn| txn.transaction_apply_update(update).unwrap());
        }

        let (peer_rendered, peer_length) =
            with_txn(&peer, |txn| (peer_text.get_string(txn), peer_text.length(txn)));
        prop_assert_eq!(&peer_rendered, &model);
        prop_assert_eq!(peer_length, utf16_units(&model));
        prop_assert!(!observe(&peer).missing);
    }

    /// The same run of edits reaching a peer as one full state rather than as a
    /// stream: the same text, so nothing in the per-edit path depends on having
    /// seen the edits separately.
    #[test]
    fn a_peer_given_the_full_state_reaches_the_same_text(
        edits in super::helpers::mixed_edits(1..16)
    ) {
        let doc = YrsDoc::new();
        let text = root_text(&doc);
        let mut model = String::new();
        for edit in &edits {
            if let Some(planned) = plan(&mut model, edit) {
                with_txn(&doc, |txn| run(&text, txn, &planned));
            }
        }
        let full = with_txn(&doc, |txn| txn.transaction_encode_state_as_update());

        let peer = YrsDoc::new();
        let peer_text = root_text(&peer);
        let (rendered, length) = with_txn(&peer, |txn| {
            txn.transaction_apply_update(full).unwrap();
            (peer_text.get_string(txn), peer_text.length(txn))
        });
        prop_assert_eq!(&rendered, &model);
        prop_assert_eq!(length, utf16_units(&model));
    }
}

/// An offset that falls inside a surrogate pair is not a position in the text,
/// and the binding neither rejects it nor panics: it rounds up to the end of
/// the pair, so an insert at 1 into a document holding one emoji lands after
/// the emoji, exactly where an insert at 2 lands. This test pins what the
/// binding does TODAY rather than what it should do; it is here so that a
/// change in the core's clamping is visible instead of silent, and a caller
/// that wants a different answer has to say so.
#[test]
fn an_offset_inside_a_surrogate_pair_rounds_up_to_the_pair_end() {
    let landed = |offset: u32| {
        let doc = YrsDoc::new();
        let text = root_text(&doc);
        with_txn(&doc, |txn| text.insert(txn, 0, "\u{1F600}".to_string()));
        with_txn(&doc, |txn| {
            text.insert(txn, offset, "x".to_string());
            (text.get_string(txn), text.length(txn))
        })
    };

    assert_eq!(landed(0), ("x\u{1F600}".to_string(), 3));
    // Offset 1 is the middle of the pair. It does not split it, and it does not
    // fail: it behaves as offset 2, the position after it.
    assert_eq!(landed(1), ("\u{1F600}x".to_string(), 3));
    assert_eq!(landed(2), ("\u{1F600}x".to_string(), 3));
}

/// A delete whose range cuts a surrogate pair in half leaves the document's
/// own clock one unit ahead of what its encoded state carries, so a peer
/// restored from that state reports a permanently lower state vector for the
/// same text. This test pins the DEFECT, not the contract: what ought to
/// happen is that the two state vectors agree.
///
/// Minimal reproducer, and it is the core rather than the binding: a document
/// with `OffsetKind::Utf16` holding "a\u{1F600}b" and asked to
/// `remove_range(1, 1)` renders "ab" and reports clock 4, while its own full
/// state re-integrates as clock 3. Reproduced against stock yrs 0.27.4 with no
/// binding in the way. The text still converges, so this is a bookkeeping
/// divergence rather than data loss, but two peers holding identical content
/// disagree about what they have seen.
///
/// The properties above never generate such an offset: they plan every edit
/// against the text the document is actually holding, which is what a caller
/// does. This is the one place the tier says what happens when a caller does
/// not.
#[test]
fn a_delete_that_cuts_a_surrogate_pair_desynchronises_the_state_vector() {
    use super::helpers::state_map;

    let author = YrsDoc::new();
    let text = root_text(&author);
    with_txn(&author, |txn| {
        text.insert(txn, 0, "a\u{1F600}b".to_string())
    });
    // The unit at offset 1 is the high surrogate of the emoji. Deleting it
    // takes the whole character with it.
    with_txn(&author, |txn| text.remove_range(txn, 1, 1));

    let (rendered, author_state) = with_txn(&author, |txn| (text.get_string(txn), state_map(txn)));
    let full = with_txn(&author, |txn| txn.transaction_encode_state_as_update());

    let peer = YrsDoc::new();
    let peer_text = root_text(&peer);
    let (peer_rendered, peer_state) = with_txn(&peer, |txn| {
        txn.transaction_apply_update(full).unwrap();
        (peer_text.get_string(txn), state_map(txn))
    });

    assert_eq!(rendered, "ab");
    assert_eq!(peer_rendered, rendered, "the text does converge");
    let client = *author_state.keys().next().unwrap();
    assert_eq!(author_state[&client], 4, "the author counts four units");
    assert_eq!(
        peer_state[&client], 3,
        "and the peer restored from its state counts three"
    );
}
