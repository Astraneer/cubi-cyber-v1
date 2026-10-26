//! Only exact owned slot/item/payload/revision matches become display facts.
//! Private payload bytes and nested machine contents never enter the UI.
pub fn project(
    attributes: Option<&net::item_attributes::InventoryAttributes>,
    inventory: &rules::survival::Inventory,
) -> [Option<ui::app::ItemFact>; 41] {
    let mut facts = [None; 41];
    let Some(attributes) = attributes else {
        return facts;
    };
    for (slot, stack) in inventory.slots().iter().enumerate() {
        let Some(attribute) = attributes.matching(
            slot as u8,
            stack.item(),
            stack.payload().map_or(0, |id| id.0),
            inventory.revision(),
        ) else {
            continue;
        };
        let fact = match attribute {
            net::item_attributes::Attribute::Configurator(mode) => {
                ui::app::ItemFact::Configurator(mode)
            }
            net::item_attributes::Attribute::Charge(joules) => ui::app::ItemFact::Charge(joules),
            net::item_attributes::Attribute::SavedMachine(block) => {
                ui::app::ItemFact::SavedMachine(block)
            }
        };
        if fact.valid_for(*stack) {
            facts[slot] = Some(fact);
        }
    }
    facts
}
