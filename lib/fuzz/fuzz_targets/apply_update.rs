#![no_main]

//! `YrsTransaction.transaction_apply_update` on a fresh document: the entry
//! point every remote update arrives through.

use libfuzzer_sys::fuzz_target;
use uniffi_yniffi::probe;

fuzz_target!(|data: &[u8]| {
    let _ = probe::apply_update(data);
});
