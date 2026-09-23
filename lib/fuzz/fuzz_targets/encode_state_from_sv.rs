#![no_main]

//! `YrsTransaction.transaction_encode_state_as_update_from_sv` against
//! arbitrary state vector bytes.

use libfuzzer_sys::fuzz_target;
use uniffi_yniffi::probe;

fuzz_target!(|data: &[u8]| {
    let _ = probe::encode_state_from_sv(data);
});
