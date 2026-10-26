//! Owned tool attributes are replaced only with their immutable payload record.
use super::*;
#[path = "tool_owner/block.rs"]
mod block;
pub(super) use block::BlockTool;
pub(super) fn inventory_attributes(
    inventory: &rules::survival::Inventory,
    payloads: &rules::item_payload::Ledger,
) -> Result<net::item_attributes::InventoryAttributes, rules::item_payload::Error> {
    use net::item_attributes::{Attribute, InventoryAttributes, ItemAttribute};
    let mut rows = Vec::new();
    for (slot, stack) in inventory.slots().iter().copied().enumerate() {
        if stack.is_empty() {
            continue;
        }
        let attribute = if stack.item() == rules::items::I_CONFIGURATOR {
            Some(Attribute::Configurator(
                rules::item_payload::resolve_configurator(stack, payloads)? as u8,
            ))
        } else if stack.item() == rules::items::I_ENERGY_TABLET {
            let joules = stack
                .payload()
                .map(|id| payloads.resolve(id, stack.item())?.joules(stack.item()))
                .transpose()?
                .unwrap_or(0);
            Some(Attribute::Charge(joules))
        } else if stack.payload().is_some()
            && matches!(
                rules::blocks::bdef(stack.item()).be,
                Some(rules::blocks::BeKind::Mek(_))
            )
        {
            payloads
                .resolve(stack.payload().expect("checked payload"), stack.item())?
                .resolve_machine(stack.item())?;
            Some(Attribute::SavedMachine(stack.item()))
        } else {
            None
        };
        if let Some(attribute) = attribute {
            rows.push(ItemAttribute {
                slot: slot as u8,
                item: stack.item(),
                payload: stack.payload().map_or(0, |id| id.0),
                attribute,
            });
        }
    }
    Ok(InventoryAttributes {
        inventory_revision: inventory.revision(),
        rows,
    })
}
impl Server {
    pub(super) fn cycle_tool_mode(
        &self,
        peer: &mut Peer,
        back: bool,
        guard: net::InventoryGuard,
        batch: &mut Batch,
    ) -> Result<(), RejectReason> {
        if !peer.health.alive() || peer.player.mode == GameMode::Spectator {
            return Err(RejectReason::Unauthorized);
        }
        let hand = peer.inventory.hand();
        if guard.revision != peer.inventory.revision()
            || usize::from(guard.selected) != peer.inventory.selected()
            || guard.item != hand.item()
            || guard.count != hand.count()
            || guard.damage != hand.damage()
            || guard.payload != hand.payload().map_or(0, |id| id.0)
        {
            return Err(RejectReason::Stale);
        }
        let pending = self.payload_delta(batch)?;
        let view = pending
            .as_ref()
            .map(|d| self.runtimes.payloads.view(d))
            .transpose()
            .map_err(|_| RejectReason::InvalidCell)?;
        let resolver: &dyn rules::item_payload::Resolver = match view.as_ref() {
            Some(view) => view,
            None => &self.runtimes.payloads,
        };
        let (after, payload) = rules::item_payload::cycle_configurator(hand, back, resolver)
            .map_err(|_| RejectReason::Unauthorized)?;
        let mut inventory = peer.inventory;
        inventory
            .set_trusted(inventory.selected(), after)
            .map_err(|_| RejectReason::Capacity)?;
        // Quota and nested-reference validation precede either owner mutation.
        let record = self.plan_payloads(batch, vec![payload])?;
        Self::stage_payloads(batch, record);
        peer.inventory = inventory;
        Ok(())
    }
}
