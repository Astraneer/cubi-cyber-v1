//! Bounded read-only public binding diagnostics; no credentials or private data.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightBinding {
    pub page: core_types::PageId,
    pub scene_cells: Option<u64>,
    pub network_cells: Option<u64>,
    pub network_light: Option<net::light::Header>,
}
impl RemoteSession {
    /// Only loaded requested pages lacking a current source certificate. The
    /// caller chooses a small output cap; iteration allocates no frame history.
    pub fn pending_light_bindings<'a>(
        &'a self,
        scene: &'a ClientScene,
    ) -> impl Iterator<Item = LightBinding> + 'a {
        scene.requested_pages().filter_map(|page| {
            if scene.world().page(page).is_none()
                || scene.authoritative_light_version(page).is_some()
            {
                return None;
            }
            Some(LightBinding {
                page,
                scene_cells: scene.authoritative_version(page).map(|v| v.view),
                network_cells: self.connection.replica().page(page).map(|p| p.version),
                network_light: self.connection.light_replica().page(page).map(|p|p.header),
            })
        })
    }
}
