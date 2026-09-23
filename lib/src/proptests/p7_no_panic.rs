//! P7: no panic across the FFI.
//!
//! Every `[Throws]` entry in the UDL that takes bytes has to answer arbitrary
//! bytes with an error. A panic here is not a failed call: UniFFI's generated
//! scaffolding cannot carry an unwind across the language boundary, and the
//! release profile builds this crate with `panic = "abort"`, so a malformed
//! update from a peer would take the host process down. yrs 0.27.4's decoder hardening
//! is the precedent for this property existing, and `fuzz/` carries the same
//! three entry points to a fuzzer that can search harder than a shrinker can.
//!
//! The second half of the property is that a rejected update leaves nothing
//! behind: the document a caller was holding renders the same text and reports
//! the same state vector on both sides of the failed call.

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Once;

use proptest::prelude::*;

use crate::probe;

/// The one panic this tier tolerates, and only because it cannot ship: yrs
/// 0.27.4 decodes a client id straight into `ClientID::new`, which carries
/// `debug_assert!(value & Self::MASK == 0)`, so a client id at or above 2^53
/// read off the wire aborts a debug build. The release profile this crate
/// ships under has debug assertions off, where the same bytes are accepted and
/// the id is silently truncated to its low 53 bits instead. Both answers are
/// wrong (the decoder owes a `DecodingError`), and both are the core's, not
/// the binding's. Any OTHER panic fails the property.
const TOLERATED_PANIC: &str = "value & Self::MASK == 0";

thread_local! {
    static SILENCED: Cell<bool> = const { Cell::new(false) };
}

/// Install a panic hook that stays quiet inside [`caught`] and behaves
/// normally everywhere else, so a tolerated panic does not print a backtrace
/// per generated case while a real failure still does.
fn install_quiet_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !SILENCED.with(|flag| flag.get()) {
                previous(info);
            }
        }));
    });
}

/// Run a probe, returning the panic message if it unwound.
fn caught<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    install_quiet_hook();
    SILENCED.with(|flag| flag.set(true));
    let outcome = catch_unwind(AssertUnwindSafe(f));
    SILENCED.with(|flag| flag.set(false));
    outcome.map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic carrying no message".to_string())
    })
}

/// Assert that a probe either answered or hit the one tolerated core defect.
fn answered<R>(what: &str, bytes: &[u8], f: impl FnOnce() -> R) -> Result<(), TestCaseError> {
    match caught(f) {
        Ok(_) => Ok(()),
        Err(message) if message.contains(TOLERATED_PANIC) => Ok(()),
        Err(message) => Err(TestCaseError::fail(format!(
            "{what} panicked on {} bytes: {message}",
            bytes.len()
        ))),
    }
}

/// The ways a valid encoding is broken here. Arbitrary bytes almost never
/// reach past the first length prefix; these keep the shape of something real
/// and damage it.
///
/// Only `Truncate` is offered to `apply_update`, and the reason is the finding
/// above: any mutation that CHANGES a byte can rewrite a string, or the root
/// type's name, into bytes that decode to a non-character, and yrs 0.27.4
/// reads both without validating. Truncation can only shorten, so a string
/// read either completes on unmodified bytes or fails on a short buffer; it
/// never fabricates one. The state vector decoder reads nothing but varints,
/// so every mutation is safe there.
#[derive(Debug, Clone)]
enum Mutation {
    /// Flip one bit.
    BitFlip { index: usize, bit: u8 },
    /// Cut the encoding short.
    Truncate { at: usize },
    /// Leave the encoding intact and append rubbish.
    Append { junk: Vec<u8> },
    /// Replace the leading count with the largest one this property will
    /// generate, so the decoder is told to expect far more than it is given.
    /// The value is bounded for the reason [`MAX_DECLARED_CLIENTS`] gives; a
    /// count with no bound at all is the fuzzers' business.
    OverstatedCount,
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        (0usize..256, 0u8..8).prop_map(|(index, bit)| Mutation::BitFlip { index, bit }),
        (0usize..256).prop_map(|at| Mutation::Truncate { at }),
        proptest::collection::vec(any::<u8>(), 0..32).prop_map(|junk| Mutation::Append { junk }),
        Just(Mutation::OverstatedCount),
    ]
}

fn mutate(base: &[u8], mutation: &Mutation) -> Vec<u8> {
    let mut bytes = base.to_vec();
    match mutation {
        Mutation::BitFlip { index, bit } => {
            if !bytes.is_empty() {
                let index = index % bytes.len();
                bytes[index] ^= 1 << bit;
            }
        }
        Mutation::Truncate { at } => {
            let at = at % (bytes.len() + 1);
            bytes.truncate(at);
        }
        Mutation::Append { junk } => bytes.extend_from_slice(junk),
        Mutation::OverstatedCount => {
            // Replace the leading count, whatever its width, with 1024: the
            // decoder is told to read that many clients out of an encoding
            // that holds one.
            let width = leading_varint_width(&bytes).unwrap_or(0);
            let mut rewritten = vec![0x80, 0x08]; // the varint for 1024
            rewritten.extend_from_slice(&bytes[width..]);
            bytes = rewritten;
        }
    }
    bytes
}

/// The number of clients a state vector's leading varint declares, read the
/// way `StateVector::decode_v1` reads it: 7 bit pieces accumulated into a u32.
/// `None` when the varint runs off the end of the input, which the decoder
/// answers with an error before it allocates anything.
fn leading_varint_width(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .position(|byte| byte & 0x80 == 0)
        .map(|i| i + 1)
}

fn declared_client_count(bytes: &[u8]) -> Option<u32> {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    for &byte in bytes {
        result |= ((byte & 0x7f) as u32) << (shift % 32);
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
    }
    None
}

/// How many clients a generated state vector may claim to hold.
///
/// The bound is not squeamishness, it is what keeps this property honest about
/// what it proves. `StateVector::decode_v1` reads that count and reserves for
/// it, and yrs 0.27.4 does use a fallible `try_reserve` there, so an absurd
/// count comes back as a decode error. A count in the plausible band does not:
/// a few bytes ask for gigabytes, the reservation SUCCEEDS on a Linux host
/// because the kernel overcommits, and the property passes for a reason that
/// has nothing to do with the binding. On a phone the same input is a jetsam
/// kill. Roughly one draw in ten over arbitrary bytes lands there.
///
/// So the allocation bomb is not asserted about here; it is left to
/// `fuzz/`, which drives these entry points with no bound at all and reports
/// it as the out-of-memory it is (see `fuzz/README.md`). What is left below is
/// the question this property can actually answer: does the decoder return.
const MAX_DECLARED_CLIENTS: u32 = 1024;

fn within_the_allocation_bound(bytes: &[u8]) -> bool {
    declared_client_count(bytes).is_none_or(|count| count <= MAX_DECLARED_CLIENTS)
}

/// A mutation of `base`, kept only if the bytes it produces stay inside the
/// allocation bound. Filtering in the strategy rather than in the test body
/// keeps a rejected draw from spending the runner's global reject budget: a
/// bit flip on a leading count byte is rare, but it is exactly what turns a
/// one client state vector into a claim of millions.
fn damaged(base: fn() -> Vec<u8>) -> impl Strategy<Value = (Mutation, Vec<u8>)> {
    mutation()
        .prop_map(move |mutation| {
            let bytes = mutate(&base(), &mutation);
            (mutation, bytes)
        })
        .prop_filter(
            "declares more clients than the bound allows",
            |(_, bytes)| within_the_allocation_bound(bytes),
        )
}

/// A valid encoded state vector, to be mutated.
fn valid_state_vector() -> Vec<u8> {
    probe::encode_state_from_sv(&[0]).expect("the empty state vector encodes");
    // The state vector of a document holding the seed update.
    let outcome = probe::apply_update_to_seeded(&probe::seed_update(), &[]);
    outcome.state_vector_before
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Arbitrary bytes into the two encoders: an answer, either way. Both
    /// decode a state vector, which is client ids and clocks and no content.
    ///
    /// `apply_update` is NOT driven with arbitrary bytes here, and that is a
    /// finding rather than an oversight: yrs 0.27.4 reads a string out of an
    /// update with `std::str::from_utf8_unchecked` (encoding/read.rs) and
    /// never validates it, so arbitrary bytes reach `char::from_u32_unchecked`
    /// with a value that is not a character. That is undefined behaviour, and
    /// it aborts rather than unwinding under the standard library's checks, so
    /// no property could catch it and no test could survive it. It is reached
    /// in seconds by `fuzz/fuzz_targets/apply_update.rs`, which is where it
    /// belongs. What is driven below instead is damage to a real update, which
    /// is the shape a corrupted or truncated update, as a server would store or
    /// forward it, actually has.
    #[test]
    fn arbitrary_bytes_are_answered_not_unwound(
        bytes in proptest::collection::vec(any::<u8>(), 0..1024)
            .prop_filter("declares more clients than the bound allows", |bytes| {
                within_the_allocation_bound(bytes)
            })
    ) {
        answered("encode_diff_v1", &bytes, || probe::encode_diff_v1(&bytes))?;
        answered("encode_state_from_sv", &bytes, || probe::encode_state_from_sv(&bytes))?;
    }

    /// A real update cut short at an arbitrary point — what a truncated update,
    /// as a server would store or forward it, or a half-written file looks
    /// like — into `apply_update`: an answer, and on an error a document that
    /// has not moved.
    #[test]
    fn a_truncated_update_is_answered_and_leaves_the_document_alone(
        at in 0usize..=probe::seed_update().len()
    ) {
        let bytes = mutate(&probe::seed_update(), &Mutation::Truncate { at });
        answered("apply_update", &bytes, || probe::apply_update(&bytes))?;

        let seed = probe::seed_update();
        let outcome = caught(|| probe::apply_update_to_seeded(&seed, &bytes));
        match outcome {
            // Whatever the answer, a rejected update leaves the document as it
            // was: the same text and the same state vector.
            Ok(outcome) => {
                if outcome.result.is_err() {
                    prop_assert!(
                        outcome.unchanged(),
                        "a rejected update changed the document: {:?}",
                        outcome
                    );
                }
            }
            Err(message) => prop_assert!(
                message.contains(TOLERATED_PANIC),
                "apply_update_to_seeded panicked: {}",
                message
            ),
        }
    }

    /// Damaged versions of a real state vector, into both encoders.
    #[test]
    fn a_damaged_state_vector_is_answered_not_unwound(
        (_mutation, bytes) in damaged(valid_state_vector)
    ) {
        answered("encode_diff_v1", &bytes, || probe::encode_diff_v1(&bytes))?;
        answered("encode_state_from_sv", &bytes, || probe::encode_state_from_sv(&bytes))?;
    }

    /// The same two entry points, given bytes that are a valid update rather
    /// than a valid state vector: a caller crossing the two up must get an
    /// error or a diff, never an unwind.
    #[test]
    fn an_update_offered_where_a_state_vector_belongs_is_answered(
        (_mutation, bytes) in damaged(probe::seed_update)
    ) {
        answered("encode_diff_v1", &bytes, || probe::encode_diff_v1(&bytes))?;
        answered("encode_state_from_sv", &bytes, || probe::encode_state_from_sv(&bytes))?;
    }
}
