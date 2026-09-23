//! The property tier: eight properties over the binding, run by `proptest`.
//!
//! Every property drives `YrsDoc`, `YrsTransaction` and `YrsText`, the types
//! Swift calls, rather than `yrs` directly, so that a failure is a statement
//! about this library and not about the core underneath it.
//!
//! One thing the tier uses is not on the surface Swift sees, and it is named
//! here so that nobody has to discover it by reading the imports: P6 links
//! `yrs 0.18.2` beside the pinned core, to run the retired decoder rather than
//! to describe it. Test-only; the XCFramework does not carry it. (P1, P2, P3,
//! P4, P6 and P8 choose a client id through `YrsDoc::with_client_id`, which
//! IS on the UDL — the Kotlin mirror needs it from Kotlin, where there is no
//! `#[cfg(test)]` — and `has_missing_updates` is the UDL's
//! `transaction_has_missing_updates`, read here through the `ReadTxn` trait.)
//!
//! P1 convergence under a lossy channel (`p1_lossy_channel`)
//! P2 deletes, GC and snapshots (`p2_deletes_gc_snapshots`)
//! P3 partial state vectors (`p3_partial_state_vectors`)
//! P4 byte determinism (`p4_byte_determinism`)
//! P5 offsets on real text (`p5_offsets`)
//! P6 the retired decoder as a negative (`p6_retired_decoder`)
//! P7 no panic across the FFI (`p7_no_panic`)
//! P8 a third writer class behind a server's client-id gate (`p8_third_writer`)
//!
//! The same eight properties exist on the other side of the wire, against
//! pycrdt under `hypothesis`, and on the Kotlin side of this same crate, in
//! kidgloves-inc/ykt under kotest. They are written to be compared: a
//! property that fails on one core and holds on the other is itself the
//! finding, which is why the numbering is shared and why the statements are
//! kept in the same words.
//!
//! Several properties stand guard over a specific fix in the core this fork
//! pins. 0.27.1 fixed a swallowed pending update and a skip-item slicing
//! underflow, 0.27.2 fixed a delete set lost after `apply_update`, 0.27.3 fixed
//! a missing right neighbour on an insert at index 0, and 0.27.4 hardened the
//! decoder and removed a `find_index` panic. Each property's doc comment names
//! the one it would catch.

mod helpers;

mod p1_lossy_channel;
mod p2_deletes_gc_snapshots;
mod p3_partial_state_vectors;
mod p4_byte_determinism;
mod p5_offsets;
mod p6_retired_decoder;
mod p7_no_panic;
mod p8_third_writer;
