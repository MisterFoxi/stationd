//! Browser-owned credentials. Providers receive only the slot they need.
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct TokenValue {
    value: String,
    revision: u64,
}
/// Deliberately has no Debug/Serialize implementation: persistence belongs to the host.
#[derive(Clone, Default)]
pub struct TokenSlot(Rc<RefCell<TokenValue>>);
impl TokenSlot {
    pub fn get(&self) -> String {
        self.0.borrow().value.clone()
    }
    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }
    pub fn is_empty(&self) -> bool {
        self.0.borrow().value.is_empty()
    }
    pub fn set(&self, value: &str) {
        let value = value.trim();
        let mut stored = self.0.borrow_mut();
        if stored.value != value {
            stored.value = value.to_owned();
            stored.revision = stored.revision.wrapping_add(1);
        }
    }
}
#[derive(Clone, Default)]
pub struct TokenStore {
    pub gateway: TokenSlot,
    pub configuration: TokenSlot,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_gateway_changes_revoke_clients_without_affecting_configuration() {
        let store = TokenStore::default();
        let station = store.gateway.clone();
        let icecast = store.gateway.clone();
        store.configuration.set("author-fixture");
        store.gateway.set("  gateway-fixture  ");
        assert_eq!(station.get(), "gateway-fixture");
        assert_eq!(icecast.get(), station.get());
        let revision = station.revision();
        store.gateway.set("gateway-fixture");
        assert_eq!(station.revision(), revision);
        store.gateway.set("");
        assert!(icecast.is_empty());
        assert_ne!(station.revision(), revision);
        assert_eq!(store.configuration.get(), "author-fixture");
    }
}
