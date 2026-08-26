use crate::array::YrsArray;
use crate::error::CodingError;
use crate::map::YrsMap;
use crate::text::YrsText;
use crate::transaction::YrsTransaction;
use std::sync::Arc;
use std::{borrow::Borrow, cell::RefCell};
use yrs::{updates::decoder::Decode, ArrayRef, Doc, OffsetKind, Options, StateVector, Transact, Origin};
use yrs::{MapRef, ReadTxn};
use yrs::branch::Branch;
use crate::undo::YrsUndoManager;
use crate::UniffiCustomTypeConverter;

pub(crate) struct YrsDoc(RefCell<Doc>);

unsafe impl Send for YrsDoc {}
unsafe impl Sync for YrsDoc {}

impl YrsDoc {
    pub(crate) fn new() -> Self {
        let mut options = Options::default();
        options.offset_kind = OffsetKind::Utf16;
        let doc = yrs::Doc::with_options(options);

        Self(RefCell::from(doc))
    }

    pub(crate) fn encode_diff_v1(
        &self,
        transaction: &YrsTransaction,
        state_vector: Vec<u8>,
    ) -> Result<Vec<u8>, CodingError> {
        let mut tx = transaction.transaction();
        let tx = tx.as_mut().unwrap();

        StateVector::decode_v1(state_vector.borrow())
            .map_err(|_e| CodingError::DecodingError)
            .map(|sv| tx.encode_diff_v1(&sv))
    }

    pub(crate) fn get_text(&self, name: String) -> Arc<YrsText> {
        let text_ref = self.0.borrow().get_or_insert_text(name.as_str());
        Arc::from(YrsText::from(text_ref))
    }

    pub(crate) fn get_array(&self, name: String) -> Arc<YrsArray> {
        let array_ref: ArrayRef = self.0.borrow().get_or_insert_array(name.as_str()).into();
        Arc::from(YrsArray::from(array_ref))
    }

    pub(crate) fn get_map(&self, name: String) -> Arc<YrsMap> {
        let map_ref: MapRef = self.0.borrow().get_or_insert_map(name.as_str()).into();
        Arc::from(YrsMap::from(map_ref))
    }

    pub(crate) fn transact<'doc>(&self, origin: Option<YrsOrigin>) -> Arc<YrsTransaction> {
        let tx = self.0.borrow();
        let tx = if let Some(origin) = origin {
            tx.transact_mut_with(origin)
        } else {
            tx.transact_mut()
        };
        Arc::from(YrsTransaction::from(tx))
    }

    pub(crate) fn undo_manager(&self, tracked_refs: Vec<YrsCollectionPtr>) -> Arc<YrsUndoManager> {
        let doc = self.0.borrow().clone();
        let mut undo_manager = yrs::undo::UndoManager::new();
        for tracked in tracked_refs {
            undo_manager.expand_scope(&doc, &tracked);
        }
        Arc::new(YrsUndoManager::new(doc, undo_manager))
    }
}

#[derive(Clone)]
pub(crate) struct YrsOrigin(Arc<[u8]>);

impl From<Origin> for YrsOrigin {
    fn from(value: Origin) -> Self {
        YrsOrigin(Arc::from(value.as_ref()))
    }
}

impl Into<Origin> for YrsOrigin {
    fn into(self) -> Origin {
        Origin::from(self.0.as_ref())
    }
}

impl UniffiCustomTypeConverter for YrsOrigin {
    type Builtin = Vec<u8>;

    fn into_custom(val: Self::Builtin) -> uniffi::Result<Self> where Self: Sized {
        Ok(YrsOrigin(val.into()))
    }

    fn from_custom(obj: Self) -> Self::Builtin {
        obj.0.to_vec()
    }
}

#[derive(Copy, Clone)]
#[repr(transparent)]
pub(crate) struct YrsCollectionPtr(*const Branch);

unsafe impl Send for YrsCollectionPtr { }
unsafe impl Sync for YrsCollectionPtr { }

impl AsRef<Branch> for YrsCollectionPtr {
    #[inline]
    fn as_ref(&self) -> &Branch {
        unsafe { self.0.as_ref() }.unwrap()
    }
}

impl<'a> From<&'a Branch> for YrsCollectionPtr {
    #[inline]
    fn from(value: &'a Branch) -> Self {
        let ptr = value as *const Branch;
        YrsCollectionPtr(ptr)
    }
}

impl UniffiCustomTypeConverter for YrsCollectionPtr {
    type Builtin = u64;

    fn into_custom(val: Self::Builtin) -> uniffi::Result<Self> where Self: Sized {
        let ptr = val as usize as *const Branch;
        Ok(YrsCollectionPtr(ptr))
    }

    fn from_custom(obj: Self) -> Self::Builtin {
        obj.0 as usize as u64
    }
}
#[cfg(test)]
mod tests {
    use yrs::updates::decoder::Decode;
    use yrs::updates::encoder::Encode;
    use yrs::{ClientID, Doc, GetString, Options, ReadTxn, StateVector, Text, Transact, Update};

    /// A client id above 2^32 must land in a peer's state vector unchanged. yrs 0.18's
    /// V1 decoder read the id as a u32, which is how a 53-bit author became a
    /// different, truncated author on the receiving side and forked the document.
    #[test]
    fn a_53_bit_client_id_survives_the_v1_round_trip() {
        // The pycrdt-authored id from the incident this fork exists for; well above 2^32
        // and inside yrs's 53-bit range (ClientID::new masks above bit 53).
        let author: u64 = 967_714_667_641_833;
        assert!(author > u32::MAX as u64);
        assert!(author < (1u64 << 53));

        let mut options = Options::default();
        options.client_id = ClientID::new(author);
        let source = Doc::with_options(options);
        let text = source.get_or_insert_text("prompt");
        let update = {
            let mut txn = source.transact_mut();
            text.insert(&mut txn, 0, "hello");
            txn.encode_state_as_update_v1(&StateVector::default())
        };

        let peer = Doc::new();
        let peer_text = peer.get_or_insert_text("prompt");
        {
            let mut txn = peer.transact_mut();
            txn.apply_update(Update::decode_v1(&update).unwrap()).unwrap();
        }
        let txn = peer.transact();
        assert_eq!(peer_text.get_string(&txn), "hello");
        let sv = txn.state_vector();
        assert_eq!(sv.get(&ClientID::new(author)), 5, "the block is credited to the 53-bit author");
        assert_eq!(sv.get(&ClientID::new(author & u32::MAX as u64)), 0, "and not to its u32 truncation");
        // Re-encoding must carry the same id back out.
        let sv_bytes = sv.encode_v1();
        let decoded = StateVector::decode_v1(&sv_bytes).unwrap();
        assert_eq!(decoded.get(&ClientID::new(author)), 5);
    }
}
