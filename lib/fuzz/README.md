# yniffi-fuzz

`cargo-fuzz` targets for the binding's `[Throws]` entry points that take bytes.
Each one calls `yniffi`'s `probe` module, which calls `YrsDoc`,
`YrsTransaction` and `YrsText` exactly as the generated Swift does, so a crash
here is a crash an app would take: UniFFI cannot carry an unwind across the
language boundary, and the shipping profile builds with `panic = "abort"`.

| Target | Entry point |
|---|---|
| `apply_update` | `YrsTransaction.transaction_apply_update` on a fresh document |
| `apply_update_seeded` | the same, on a document that already holds blocks, asserting that a rejected update leaves it unchanged |
| `encode_diff_v1` | `YrsDoc.encode_diff_v1` against arbitrary state vector bytes |
| `encode_state_from_sv` | `YrsTransaction.transaction_encode_state_as_update_from_sv` |

## Running

Needs a nightly toolchain and `cargo-fuzz`; the sanitizer instrumentation
libFuzzer relies on is nightly only.

```sh
cargo install cargo-fuzz
rustup toolchain install nightly

cd lib/fuzz
cargo +nightly fuzz run apply_update
cargo +nightly fuzz run apply_update -- -max_total_time=60   # time boxed
cargo +nightly fuzz list                                     # the four targets
```

A finding lands in `fuzz/artifacts/<target>/`; reproduce it with
`cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>`. Corpora and
artifacts are not committed.

CI does not run the fuzzers: the `rust` job type-checks them
(`cargo check --locked --manifest-path lib/fuzz/Cargo.toml`) so that a change
to the binding cannot leave a target that no longer compiles, and the
property tests in `lib/src/proptests/` carry the same questions in a form that
finishes.

## Known crashes

Every one of the four targets dies within seconds, and every failure so far is
the core's rather than the binding's. Until they are fixed upstream (or the
binding validates before it forwards) the fuzzers cannot get past them, so a
long run is not yet worth starting.

**Out of memory on four bytes, all four targets.** A length prefix is read and
allocated against without a bound. The smallest inputs found, as hex:

| Target | Input | Allocation |
|---|---|---|
| `apply_update` | `979c9763` | out of memory |
| `apply_update_seeded` | `cdcdac2c` | `malloc(5502926864)` |
| `encode_diff_v1` | `fff6c325` | `malloc(2281701392)` |
| `encode_state_from_sv` | `d0d0d92a` | `malloc(2281701392)` |

A length far beyond the domain is rejected cleanly; it is the plausible looking
one in between that allocates. On a phone this is a jetsam kill from a single
malformed row.

**Undefined behaviour on a malformed string, `apply_update`.** yrs 0.27.4 reads
a string out of an update with `std::str::from_utf8_unchecked`
(`encoding/read.rs`) and never validates it, so malformed bytes reach
`char::from_u32_unchecked` with a value that is not a character. Under the
standard library's undefined behaviour checks that aborts; without them it
proceeds on an invalid `str`. It is why the property tier drives `apply_update`
with truncations rather than with arbitrary bytes.
