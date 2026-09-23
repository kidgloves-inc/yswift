#![no_main]

//! `YrsDoc.encode_diff_v1` against arbitrary state vector bytes.

use libfuzzer_sys::fuzz_target;
use uniffi_yniffi::probe;

fuzz_target!(|data: &[u8]| {
    let _ = probe::encode_diff_v1(data);
});
