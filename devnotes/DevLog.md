# Decision log

## 2026-08-26

### kidgloves-inc fork: yrs 0.27.4, 53-bit client ids

This tree is `kidgloves-inc/yswift`, forked from `y-crdt/yswift` at `d22bde0`
(the 0.2.1 release, July 2024). It exists because upstream is dormant — no
commit since 2024-07-20, and issue #53 / PR #54 (May 2026, a production user
offering to contribute) have had no maintainer response — while the core it
pins, `yrs 0.18.2`, is on the wrong side of a wire-format break.

`yrs` 0.18 declared `ClientID = u64` and *encoded* it as a 64-bit varint, but
`DecoderV1::read_client` decoded it into a `u32`. Every peer on `yrs` ≥ 0.26
(y-crdt PR #612 "Client 53bit", merged 2026-05-04) or Yjs ≥ 14 generates
53-bit ids by default, so an update from such a peer applied through yswift
0.2.1 lands under a silently truncated author. No error, no warning: the
document forks and the fork surfaces days later. kidgloves hit this in
production (fourteen boards) with `pycrdt` on the other side; another
production user, `zshannon/yswift`, hit the same bug and patched a git fork of
`yrs` 0.25 (`fix/client-id-u64-truncation`) rather than crossing the 0.26
line. That fork was read as reference here — its `sync`-feature shapes were
useful — but nothing was taken from it without checking against stock 0.27.4
signatures.

What changed to get onto `yrs = { version = "0.27.4", features = ["sync"] }`:

- `yrs::types::Value` became private; the crate exports `yrs::Out`, imported
  here as `Out as Value` so the match arms stay readable.
- `TransactionMut::apply_update` returns `Result<(), UpdateError>` instead of
  panicking; it maps to `CodingError::DecodingError`.
- `UndoManager::new()` takes no document, and `expand_scope` takes `&Doc` —
  undo managers span documents now — so `YrsUndoManager` holds a `Doc` clone.
  `undo`/`redo` are `async`; the `_blocking` forms are used, and they wait for
  exclusive store access rather than failing, so `YrsUndoError::PendingTransaction`
  has no source any more and `undo`/`redo`/`clear` stop throwing on the Swift
  side. The cost is documented on `YUndoManager.undo()`: an undo issued from
  inside a transaction on the same document deadlocks instead of throwing.
  `clear` is `clear_all`. Observers take `&mut self`.
- `AsRef<Branch>` is implemented for several types now, so the `'static`
  branch transmutes name the impl explicitly.
- Dropping a yrs `Subscription` no longer removes the callback; it queues the
  removal for the observer's next trigger, so a cancelled closure stayed
  alive until the next event and three leak tests went red. The shared types
  now observe under a key (`observe_with`) and `YSubscription` unobserves by
  that key on drop, which removes the callback at once. The undo manager's
  observers still use yrs's own `Subscription`.
  That unobserve walks the branch through a raw `BranchPtr`, and a
  subscription can be the last thing standing — a view model releases its
  document, its text and its subscription in an order nobody chose — so the
  first cut segfaulted in `Branch::unobserve` on every `WhiteboardDocument`
  deinit in the app while the package's own tests stayed green. Each shared
  type wrapper (`YrsText`, `YrsArray`, `YrsMap`) now carries a clone of its
  `Doc`, and the unobserve closure captures it, so the store outlives anything
  that can reach into it. `YSubscriptionTests.test_cancelling_after_the_document_is_gone_is_safe`
  pins it, and crashed with signal 11 before the change.
- One addition to the UDL, `YrsTransaction.transaction_client_states()`: the
  state vector as `(client_id, clock)` pairs. `transaction_state_vector` hands
  back the same thing encoded; this is what lets a caller check WHICH client
  a document credits an update to, which is the whole question here.
- `Cargo.lock` is committed: `scripts/build-xcframework.sh` builds with
  `--locked`, and a release must be reproducible from the tree.
- Upstream PR #54 (`diff(from: [])` treated as "full state" instead of a
  decoder panic, by Mike / `appymichael`) is folded in with its tests.

Deliberately not done here: `small-client` (the 32-bit compatibility flag —
the opposite of the goal), the `uniffi` 0.29+ move (removes
`UniffiCustomTypeConverter`, used in `doc.rs`; an unforced bump in a
migration hides its real cost), and `thiserror` 2.

A `#[test]` in `lib/src/doc.rs` applies an update authored by the incident's
53-bit id (`967714667641833`) and asserts the peer credits exactly that id,
not its `u32` truncation.

Release: `scripts/release.sh <version>` builds the XCFramework, rewrites
`Package.swift`'s binary target to this repo's release URL and checksum,
commits, tags and publishes the GitHub release with the zip attached. The
repository must stay public: SwiftPM, rules_swift_package_manager and CI all
fetch `binaryTarget` URLs unauthenticated.

## 2023-01-19

### Passing complex types as `String`s through Uniffi bridging

At the moment of writing, Uniffi didn't support passing complex types through Uniffi bridge.  
(See [related issue #1](https://github.com/mozilla/uniffi-rs/issues/411), [#2](https://github.com/mozilla/uniffi-rs/issues/348)
and type mapping [table from docs](https://mozilla.github.io/uniffi-rs/udl/builtin_types.html)).

There were some attempts to implement passing any non-primitive type by the means of JSON serialization & deserialization.  
See [related PR](https://github.com/mozilla/uniffi-rs/pull/440). But it wasn't merged due to the reasons outlined in the comments.

To work around this limitation – manual JSON serialization & deserialization happens when passing complex types back and forth
between Rust and Swift code. This process leverages `lib0-serde` feature.

Few further improvements that can be made here: 
- Use `lib0` binary encoding/decoding to pass data as binary buffers rather than JSON strings.
- Pass raw pointers (e.g. `BranchPtr` from `yrs`) through the bridge and consume them using `Unmanaged` features of Swift.

## 2023-01-10

### Monorepo for Kotlin & Swift bindings development

Both Kotlin and Swift bindings need to access common `.udl` (interface definition file),
few options as git submodules were considered, but decision was made to go with
monorepo setup for the active development phase of the bindings as it was the simplest-to-use
and lowest overhead approach.

After language bindings reach their stable release states, we might consider to split the repo
into three parts: Uniffi, Kotlin and Swift, where Uniffi part will contain `.udl` file and
wrapping Rust library that will eventually publish corresponding artifacts 
(e.g. SPM package for Swift and Gradle module for Kotlin)

## 2022-12-20

### Uniffi as bindgen foundation

[UniFFI](https://mozilla.github.io/uniffi-rs/) was chosen as binding generation solution
for Kotlin & Swift language bindings due to good documentation, active maintenance state and overall
use case suitability.

Alternatives considered: [swift-bridge](https://github.com/chinedufn/swift-bridge) and [Yrs C FFI](https://github.com/y-crdt/y-crdt/tree/main/yffi)
