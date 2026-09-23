mod array;
mod attrs;
mod change;
mod delta;
mod doc;
mod error;
mod map;
mod mapchange;
mod text;
mod transaction;
mod undo;
mod subscription;

/// The binding's byte-taking entry points, callable without UniFFI: what the
/// cargo-fuzz targets under `fuzz/` and the property tier drive, so that a
/// crash found there is a crash in the code Swift calls and never in a
/// re-implementation of it. Compiled into tests, and into a normal build only
/// under the `fuzzing` feature the fuzz crate turns on.
#[cfg(any(test, feature = "fuzzing"))]
pub mod probe;

#[cfg(test)]
mod proptests;

use crate::doc::YrsCollectionPtr;
use crate::doc::YrsOrigin;
use crate::array::YrsArray;
use crate::array::YrsArrayEachDelegate;
use crate::array::YrsArrayObservationDelegate;
use crate::change::YrsChange;
use crate::delta::YrsDelta;
use crate::doc::YrsDoc;
use crate::error::CodingError;
use crate::error::YrsDocError;
use crate::map::YrsMap;
use crate::map::YrsMapIteratorDelegate;
use crate::map::YrsMapKVIteratorDelegate;
use crate::map::YrsMapObservationDelegate;
use crate::mapchange::YrsEntryChange;
use crate::mapchange::YrsMapChange;
use crate::text::YrsText;
use crate::text::YrsTextObservationDelegate;
use crate::transaction::YrsClientState;
use crate::transaction::YrsTransaction;
use crate::undo::YrsUndoManager;
use crate::undo::YrsUndoManagerObservationDelegate;
use crate::undo::YrsUndoError;
use crate::undo::YrsUndoEvent;
use crate::undo::YrsUndoEventKind;
use crate::subscription::YSubscription;

uniffi::include_scaffolding!("yniffi");
