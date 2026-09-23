#![no_main]

//! The same entry point against a document that already holds blocks, which is
//! the state a real one is in. Integration paths that need an existing block
//! store are only reachable this way.

use libfuzzer_sys::fuzz_target;
use uniffi_yniffi::probe;

fuzz_target!(|data: &[u8]| {
    let outcome = probe::apply_update_to_seeded(&probe::seed_update(), data);
    if outcome.result.is_err() {
        assert!(
            outcome.unchanged(),
            "a rejected update left the document changed"
        );
    }
});
