//! P6: the retired decoder as a negative.
//!
//! This property does not test the binding as it is. It tests that the harness
//! can still see the bug the fork exists for: yrs 0.18's
//! `DecoderV1::read_client` read a client id into a `u32`, so every id at or
//! above 2^32 arrived under a different, scrambled author and the document
//! forked silently (fixed by the 53 bit client ids in yrs 0.26).
//!
//! The negative half runs the retired core itself. `yrs 0.18.2` is a
//! dev-dependency beside the pinned 0.27.4, so what decodes the update here is
//! the decoder that shipped in yswift 0.2.1, not a description of it. A model
//! of the accumulation is kept as well and checked against that decoder on
//! every case, which is what makes the model evidence rather than an
//! assumption.
//!
//! Note where the truncation lived: in the UPDATE decoder. 0.18.2's
//! `StateVector::decode_v1` reads a client with a full width varint and gets a
//! 53 bit id right, so a state vector round trip proves nothing here. The
//! property therefore builds a real update through the binding and asks the
//! old core who wrote it.
//!
//! The constructive half requires the binding, on the core it actually pins,
//! to credit an update to exactly the id that authored it. The corner ids are
//! enumerated rather than drawn: 2^32 is the boundary the old decoder broke
//! at, and a uniform draw over 53 bits would hit it about once in two million.

use std::collections::BTreeMap;

use proptest::prelude::*;
use yrs_legacy::updates::decoder::Decode as LegacyDecode;

use super::helpers::{doc_with, observe, root_text, state_map, with_txn, CORNER_CLIENT_IDS};

/// Every id this tier pins, and the author yrs 0.18.2 credits an update from
/// it to. Measured with 0.18.2 itself, not derived.
///
/// The same table is mirrored row for row, in this order, against pycrdt: it
/// is the shared statement of what the retired decoder did, and a row that
/// disagrees across the two cores is the finding. The first five rows and the tenth are the corner set; the last
/// four are ids observed in the wild, two of which forked a real document.
const RETIRED_DECODER_TABLE: [(u64, u64); 14] = [
    (1, 1),
    (2_147_483_647, 2_147_483_647),
    (2_147_483_648, 2_147_483_648),
    (4_294_967_295, 4_294_967_295),
    (4_294_967_296, 0),
    (4_294_967_297, 1),
    (8_589_934_592, 0),
    (1_099_511_627_776, 256),
    (281_474_976_710_656, 65_536),
    (9_007_199_254_740_991, 4_294_967_295),
    (967_714_667_641_833, 2_701_360_105),
    (4_792_597_679_421_530, 2_587_606_746),
    (6_968_031_897_510_372, 1_511_580_132),
    (6_009_215_146_349_235, 1_849_515_003),
];

/// lib0 V1 variable length encoding of a u64, which is how a client id goes on
/// the wire, then and now.
fn write_var_u64(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// A model of yrs 0.18's `read_client`: a u32 accumulator fed 7 bit pieces,
/// each shifted by `shift % 32` and masked back to 32 bits. Every piece above
/// the 32nd bit wraps around and lands on top of bits already written, which is
/// why the result is not merely truncated but scrambled.
///
/// The model exists to say what the old decoder did in a form a reader can
/// check by eye. It is never trusted on its own: every property below compares
/// it against what 0.18.2 actually returns for the same id.
fn legacy_read_client(bytes: &[u8]) -> u32 {
    let mut result: u32 = 0;
    let mut shift: u32 = 0;
    for &byte in bytes {
        let piece = (byte & 0x7f) as u32;
        result = (result | ((piece << (shift % 32)) & 0xFFFF_FFFF)) & 0xFFFF_FFFF;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    result
}

/// P8's negative builds its session peer under this, rather than deriving the
/// same arithmetic a second time: the model is pinned against yrs 0.18.2 on
/// every case below, and a second copy could be "fixed" into a wrong model of
/// the bug (a plain `% 2**32`) without that pin noticing. The pycrdt mirror
/// shares a single copy of its truncation model for the same reason.
pub(super) fn modelled(id: u64) -> u64 {
    legacy_read_client(&write_var_u64(id)) as u64
}

/// Author an update under `id` through the binding, hand it to yrs 0.18.2, and
/// report who that core says wrote it.
fn credited_by_the_retired_decoder(id: u64) -> u64 {
    let author = doc_with(id, false);
    let text = root_text(&author);
    let update = with_txn(&author, |txn| {
        text.insert(txn, 0, "hello".to_string());
        author.encode_diff_v1(txn, vec![]).unwrap()
    });

    let decoded = yrs_legacy::Update::decode_v1(&update)
        .expect("yrs 0.18.2 parses an update this binding produces");
    let clients: Vec<u64> = decoded
        .state_vector()
        .iter()
        .map(|(client, _)| *client)
        .collect();
    assert_eq!(clients.len(), 1, "the update has exactly one author");
    clients[0]
}

/// The table, row by row, against the core it was measured with. This is the
/// row the pycrdt mirror compares against, so a disagreement here is a
/// disagreement between the two harnesses and not a detail of one.
#[test]
fn the_retired_decoder_credits_exactly_the_table() {
    for (id, expected) in RETIRED_DECODER_TABLE {
        assert_eq!(
            credited_by_the_retired_decoder(id),
            expected,
            "yrs 0.18.2 credited an update from {id} to something other than {expected}"
        );
        assert_eq!(
            modelled(id),
            expected,
            "the model disagrees with 0.18.2 on {id}"
        );
    }
}

/// Below 2^32 the old decoder is exact, which is why the bug hid for as long as
/// it did: every id yrs 0.18 itself generated survived it.
#[test]
fn the_corner_ids_split_at_2_to_the_32() {
    for id in CORNER_CLIENT_IDS {
        let credited = credited_by_the_retired_decoder(id);
        if id < (1u64 << 32) {
            assert_eq!(credited, id, "{id} is inside the old decoder's range");
        } else {
            assert_ne!(credited, id, "{id} must not survive the old decoder");
        }
    }
}

/// The top of the domain is closed: 2^53 is the first id the wire cannot carry
/// (the client-id contract names 2^53 an explicit invalid input), and the
/// constructor refuses it rather than let `ClientID::new` mask it to 0 — an id
/// nobody issued, which is the u32 truncation's shape by a third road.
#[test]
fn an_id_at_2_to_the_53_is_refused_by_the_constructor() {
    assert!(matches!(
        crate::doc::YrsDoc::with_client_id(1u64 << 53, false),
        Err(crate::error::YrsDocError::ClientIdOutOfRange)
    ));
    assert!(crate::doc::YrsDoc::with_client_id((1u64 << 53) - 1, false).is_ok());
}

/// The constructive half, over the corners: on the core the fork pins, a
/// document authoring under a corner id has that exact id credited on a fresh
/// peer. This is the property the unclamp opens up.
#[test]
fn every_corner_id_is_credited_unchanged_on_a_fresh_peer() {
    for id in CORNER_CLIENT_IDS {
        let author = doc_with(id, false);
        let text = root_text(&author);
        let update = with_txn(&author, |txn| {
            text.insert(txn, 0, "hello".to_string());
            author.encode_diff_v1(txn, vec![]).unwrap()
        });

        let peer = crate::doc::YrsDoc::new();
        let peer_text = root_text(&peer);
        let credited = with_txn(&peer, |txn| {
            txn.transaction_apply_update(update).unwrap();
            (state_map(txn), peer_text.get_string(txn))
        });

        assert_eq!(
            credited.0,
            BTreeMap::from([(id, 5u32)]),
            "the peer must credit the update to {id} itself"
        );
        assert_eq!(credited.1, "hello");
        assert!(!observe(&peer).missing);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Every id in the half of the domain the unclamp opened up is mangled by
    /// the retired decoder, and mangled in the way the model says. If this ever
    /// passed for some id, that id would be one a 0.2.1 device could have
    /// handled, and the harness would be blind there.
    #[test]
    fn no_id_at_or_above_2_to_the_32_survives_the_retired_decoder(
        id in (1u64 << 32)..(1u64 << 53)
    ) {
        let credited = credited_by_the_retired_decoder(id);
        prop_assert_ne!(credited, id);
        prop_assert_eq!(credited, modelled(id));
    }

    /// And the same decoder is exact below 2^32, so a failure above it is about
    /// the width of the accumulator and not about the encoding.
    #[test]
    fn every_id_below_2_to_the_32_survives_the_retired_decoder(id in 1u64..(1u64 << 32)) {
        let credited = credited_by_the_retired_decoder(id);
        prop_assert_eq!(credited, id);
        prop_assert_eq!(credited, modelled(id));
    }
}
