//! Entry points the fuzz targets and the property tests drive the binding
//! through.
//!
//! Every entry point below goes through `YrsDoc`, `YrsTransaction` and the
//! binding's own `[Throws]` methods. That is the point of the module: a fuzz
//! target that re-implemented decoding would prove nothing about what Swift
//! actually calls. The functions are deliberately small and total, so a target
//! is one line and a failure names the entry point.
//!
//! `seed_update` is the exception, and it is not an entry point: it is input.
//! It is built with `yrs` directly, because a fuzz target needs a known good
//! update before there is a document to make one with. The bytes it produces
//! are pinned to what the BINDING writes for the same script, scenario S's
//! first update, by `SCENARIO_S_U1_HEX` in
//! `proptests::p4_byte_determinism::the_probe_seed_is_scenario_s_first_update`.
//!
//! Compiled under `cfg(test)` for the property tier and under the `fuzzing`
//! feature for `fuzz/`, so a release build of the XCFramework carries none of
//! it.

use crate::doc::YrsDoc;

pub use crate::error::CodingError;

/// The root text every probe uses. The name is the root name every property
/// uses, so a corpus entry recorded against that root is meaningful here.
const ROOT: &str = "prompt";

/// A known good update: client 967714667641833 inserting "hello" into the root
/// text, encoded V1. The seed of `apply_update_to_seeded`, and the base a
/// fuzzer mutates from. Built with `yrs` rather than with the binding, and
/// checked byte for byte against the binding's own output for the same script
/// (see the module note).
pub fn seed_update() -> Vec<u8> {
    use yrs::{ClientID, Doc, OffsetKind, Options, ReadTxn, Text, Transact};

    let mut options = Options::default();
    options.client_id = ClientID::new(967_714_667_641_833);
    options.offset_kind = OffsetKind::Utf16;
    let doc = Doc::with_options(options);
    let text = doc.get_or_insert_text(ROOT);
    let mut txn = doc.transact_mut();
    text.insert(&mut txn, 0, "hello");
    txn.encode_state_as_update_v1(&yrs::StateVector::default())
}

/// `YrsTransaction.transaction_apply_update` on a fresh document.
pub fn apply_update(bytes: &[u8]) -> Result<(), CodingError> {
    let doc = YrsDoc::new();
    let txn = doc.transact(None);
    let result = txn.transaction_apply_update(bytes.to_vec());
    txn.free();
    result
}

/// `YrsDoc.encode_diff_v1` against arbitrary state vector bytes. The document
/// holds a little text first, so the encoder walks real blocks rather than
/// returning an empty diff whatever it is handed.
pub fn encode_diff_v1(state_vector: &[u8]) -> Result<Vec<u8>, CodingError> {
    let doc = YrsDoc::new();
    let text = doc.get_text(ROOT.to_string());
    let txn = doc.transact(None);
    text.insert(&txn, 0, "hello".to_string());
    let result = doc.encode_diff_v1(&txn, state_vector.to_vec());
    txn.free();
    result
}

/// `YrsTransaction.transaction_encode_state_as_update_from_sv` against
/// arbitrary state vector bytes.
pub fn encode_state_from_sv(state_vector: &[u8]) -> Result<Vec<u8>, CodingError> {
    let doc = YrsDoc::new();
    let text = doc.get_text(ROOT.to_string());
    let txn = doc.transact(None);
    text.insert(&txn, 0, "hello".to_string());
    let result = txn.transaction_encode_state_as_update_from_sv(state_vector.to_vec());
    txn.free();
    result
}

/// What a document looked like before and after a candidate update was applied
/// to it. `text` is the root text, `state_vector` its encoded state vector.
#[derive(Debug)]
pub struct SeededOutcome {
    pub result: Result<(), CodingError>,
    pub text_before: String,
    pub text_after: String,
    pub state_vector_before: Vec<u8>,
    pub state_vector_after: Vec<u8>,
}

impl SeededOutcome {
    /// True when the candidate left no trace, which is what a rejected update
    /// owes the caller.
    pub fn unchanged(&self) -> bool {
        self.text_before == self.text_after && self.state_vector_before == self.state_vector_after
    }
}

/// Apply a known good update, then a candidate one, reporting the document's
/// observables on both sides of the candidate. A document that already holds
/// blocks reaches integration paths an empty one cannot.
pub fn apply_update_to_seeded(seed_update: &[u8], bytes: &[u8]) -> SeededOutcome {
    let doc = YrsDoc::new();
    let text = doc.get_text(ROOT.to_string());
    let txn = doc.transact(None);
    // A malformed seed would make the probe meaningless, so it is applied and
    // its result kept out of the outcome: callers pass `seed_update()`.
    let _ = txn.transaction_apply_update(seed_update.to_vec());
    let text_before = text.get_string(&txn);
    let state_vector_before = txn.transaction_state_vector();

    let result = txn.transaction_apply_update(bytes.to_vec());

    let text_after = text.get_string(&txn);
    let state_vector_after = txn.transaction_state_vector();
    txn.free();

    SeededOutcome {
        result,
        text_before,
        text_after,
        state_vector_before,
        state_vector_after,
    }
}
