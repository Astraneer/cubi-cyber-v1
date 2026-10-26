//! One retained UI operation preserves intent under bounded network pressure.
use super::{Dispatch, dispatch};
use crate::network::{Connection, NetworkError};
use ui::app::{Intent, UiApp};
#[derive(Default)]
pub struct IntentSender {
    pending: Option<Intent>,
}
impl IntentSender {
    pub fn clear(&mut self) {
        self.pending = None;
    }
    pub fn take(&mut self, app: &mut UiApp) -> Option<Intent> {
        if self.pending.is_some()
            && let Some(local) = app.take_local_intent()
        {
            return Some(local);
        }
        self.pending.take().or_else(|| app.take_intent())
    }
    pub fn send(
        &mut self,
        network: &mut Connection,
        intent: Intent,
        now_ms: u64,
    ) -> Result<Dispatch, NetworkError> {
        if network.welcome().is_none() {
            return Err(NetworkError::NotConnected);
        }
        if !network.can_queue_action() {
            self.pending = Some(intent);
            return Err(NetworkError::Capacity);
        }
        // Clone only admitted user operations. Repeated blocked frames neither
        // allocate strings nor consume an ActionId or change owned inventory.
        match dispatch(network, intent.clone(), now_ms) {
            Err(NetworkError::Capacity) => {
                self.pending = Some(intent);
                Err(NetworkError::Capacity)
            }
            result => result,
        }
    }
}
