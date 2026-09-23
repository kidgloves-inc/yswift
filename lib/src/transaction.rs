use crate::array::YrsArray;
use crate::error::CodingError;
use crate::map::YrsMap;
use crate::text::YrsText;
use std::borrow::Borrow;
use std::cell::{RefCell, RefMut};
use std::sync::Arc;
use yrs::{
    updates::decoder::Decode, updates::encoder::Encode, ReadTxn, StateVector, TransactionMut,
    Update,
};
use yrs::{Store, WriteTxn};
use crate::doc::YrsOrigin;

pub(crate) struct YrsClientState {
    pub(crate) client_id: u64,
    pub(crate) clock: u32,
}

pub(crate) struct YrsTransaction(pub(crate) RefCell<Option<TransactionMut<'static>>>);

unsafe impl Send for YrsTransaction {}
unsafe impl Sync for YrsTransaction {}

impl YrsTransaction {}

impl ReadTxn for YrsTransaction {
    fn store(&self) -> &Store {
        let mut tx = self.transaction();
        let tx = tx.as_mut().unwrap();

        // Use transmute to cast the mutable reference to the `Store` to a reference with a shorter lifetime
        unsafe { std::mem::transmute::<&mut Store, &'static Store>(tx.store_mut()) }
    }
}

impl<'doc> From<TransactionMut<'doc>> for YrsTransaction {
    fn from(txn: TransactionMut<'doc>) -> Self {
        let txn: TransactionMut<'static> = unsafe { std::mem::transmute(txn) };
        YrsTransaction(RefCell::from(Some(txn)))
    }
}

impl YrsTransaction {
    pub(crate) fn transaction(&self) -> RefMut<'_, Option<TransactionMut<'static>>> {
        self.0.borrow_mut()
    }

    pub(crate) fn origin(&self) -> Option<YrsOrigin> {
        let txn = self.0.borrow();
        txn.as_ref()?.origin().cloned().map(YrsOrigin::from)
    }

    pub(crate) fn transaction_encode_update(&self) -> Vec<u8> {
        self.transaction().as_ref().unwrap().encode_update_v1()
    }

    pub(crate) fn transaction_encode_state_as_update_from_sv(
        &self,
        state_vector: Vec<u8>,
    ) -> Result<Vec<u8>, CodingError> {
        let mut tx = self.transaction();
        let tx = tx.as_mut().unwrap();

        StateVector::decode_v1(state_vector.borrow())
            .map_err(|_e| CodingError::DecodingError)
            .map(|sv: StateVector| tx.encode_state_as_update_v1(&sv))
    }

    pub(crate) fn transaction_encode_state_as_update(&self) -> Vec<u8> {
        let mut tx = self.transaction();
        let tx = tx.as_mut().unwrap();
        tx.encode_state_as_update_v1(&StateVector::default())
    }

    pub(crate) fn transaction_state_vector(&self) -> Vec<u8> {
        self.transaction()
            .as_ref()
            .unwrap()
            .state_vector()
            .encode_v1()
    }

    pub(crate) fn transaction_client_states(&self) -> Vec<YrsClientState> {
        self.transaction()
            .as_ref()
            .unwrap()
            .state_vector()
            .iter()
            .map(|(client, clock)| YrsClientState {
                client_id: client.get(),
                clock: *clock,
            })
            .collect()
    }

    // True while the store holds updates whose dependencies have not arrived; a
    // document in this state still edits and renders, so nothing else reports it.
    // See tests::a_withheld_dependency_leaves_the_document_missing_updates.
    pub(crate) fn transaction_has_missing_updates(&self) -> bool {
        self.transaction().as_ref().unwrap().has_missing_updates()
    }

    pub(crate) fn transaction_apply_update(&self, update: Vec<u8>) -> Result<(), CodingError> {
        let update = Update::decode_v1(update.as_slice()).map_err(|_e| CodingError::DecodingError)?;
        // yrs >= 0.27 reports integration failures instead of panicking; they
        // are not decoding failures, and a caller may want to tell them apart.
        self.transaction()
            .as_mut()
            .unwrap()
            .apply_update(update)
            .map_err(|_e| CodingError::ApplyError)
    }

    pub(crate) fn transaction_get_text(&self, name: String) -> Option<Arc<YrsText>> {
        self.transaction()
            .as_ref()
            .unwrap()
            .get_text(name.as_str())
            .map(YrsText::from)
            .map(Arc::from)
    }

    pub(crate) fn transaction_get_array(&self, name: String) -> Option<Arc<YrsArray>> {
        self.transaction()
            .as_ref()
            .unwrap()
            .get_array(name.as_str())
            .map(YrsArray::from)
            .map(Arc::from)
    }

    pub(crate) fn transaction_get_map(&self, name: String) -> Option<Arc<YrsMap>> {
        self.transaction()
            .as_ref()
            .unwrap()
            .get_map(name.as_str())
            .map(YrsMap::from)
            // ^^ this is reporting as return Option<{unknown}> instead of Option<YrsMap>, and I'm not sure why...
            .map(Arc::from)
    }

    pub(crate) fn free(&self) {
        self.0.replace(None);
    }
}

#[cfg(test)]
mod tests {
    use crate::doc::YrsDoc;
    use yrs::{ClientID, Doc, Options, ReadTxn, StateVector, Text, Transact};

    /// A state a document can sit in indefinitely without any other symptom. An update
    /// that names a dependency the receiver has never seen is not rejected and does not
    /// raise: yrs parks it in `store.pending` and carries on, so the document opens,
    /// accepts edits and renders exactly as a healthy one does while every later update
    /// is quietly withheld too. Nothing above the core can distinguish the two — it is
    /// the only thing that knows, and `has_missing_updates` is it saying so. The pair of assertions is the
    /// whole contract: true while the dependency is withheld, false the moment it lands
    /// (and not merely "false eventually" — the same transaction that integrates the
    /// missing block must clear the flag, or a caller polling it would never recover).
    #[test]
    fn a_withheld_dependency_leaves_the_document_missing_updates() {
        // A pycrdt-authored id observed in the wild, well above 2^32 (see
        // doc::tests::a_53_bit_client_id_survives_the_v1_round_trip).
        let mut options = Options::default();
        options.client_id = ClientID::new(967_714_667_641_833);
        let source = Doc::with_options(options);
        let text = source.get_or_insert_text("prompt");

        let (first, after_first) = {
            let mut txn = source.transact_mut();
            text.insert(&mut txn, 0, "a");
            (
                txn.encode_state_as_update_v1(&StateVector::default()),
                txn.state_vector(),
            )
        };
        // The second block's left origin is the "a" of the first, so it cannot be
        // integrated by a peer that has not been given `first`.
        let second = {
            let mut txn = source.transact_mut();
            text.insert(&mut txn, 1, "b");
            txn.encode_diff_v1(&after_first)
        };

        let doc = YrsDoc::new();
        let peer_text = doc.get_text("prompt".into());
        let txn = doc.transact(None);
        assert!(!txn.transaction_has_missing_updates(), "a fresh document owes nothing");

        txn.transaction_apply_update(second).unwrap();
        assert!(
            txn.transaction_has_missing_updates(),
            "the dependent block is pending, not integrated"
        );
        assert_eq!(peer_text.get_string(&txn), "", "and nothing of it is visible");

        txn.transaction_apply_update(first).unwrap();
        assert!(
            !txn.transaction_has_missing_updates(),
            "delivering the dependency drains the pending store"
        );
        assert_eq!(peer_text.get_string(&txn), "ab");
        txn.free();
    }
}
