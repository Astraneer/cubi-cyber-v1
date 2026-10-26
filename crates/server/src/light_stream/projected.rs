//! Public LIGHT revisions describe the admitted public values, never the number
//! of private lava/ore light updates. Uniform pages retain one packed value.
use super::*;
const VOLUME: usize = core_types::PAGE_VOLUME;
pub(super) enum Values {
    Uniform(u32),
    Dense(Box<[u32; VOLUME]>),
}
impl Values {
    fn get(&self, index: usize) -> u32 {
        match self {
            Self::Uniform(value) => *value,
            Self::Dense(values) => values[index],
        }
    }
    fn from_box(values: Box<[u32; VOLUME]>) -> Self {
        if values.iter().all(|value| *value == values[0]) {
            Self::Uniform(values[0])
        } else {
            Self::Dense(values)
        }
    }
    fn boxed(&self) -> Box<[u32; VOLUME]> {
        match self {
            Self::Uniform(value) => Box::new([*value; VOLUME]),
            Self::Dense(values) => values.clone(),
        }
    }
    fn patch(&mut self, changes: &[(u16, u32)]) {
        if let Self::Uniform(value) = self {
            *self = Self::Dense(Box::new([*value; VOLUME]));
        }
        if let Self::Dense(values) = self {
            for &(index, value) in changes {
                values[usize::from(index)] = value;
            }
        }
    }
}
impl LightStream {
    pub(super) fn build_projected(
        &mut self,
        connection: u64,
        request: Request,
        page: &world::Page,
        admitted: &[Cell; VOLUME],
    ) -> Result<Option<Update>, ServerError> {
        let source = page.channel_version(core_types::ChannelMask::LIGHTING);
        let history = self
            .peers
            .get(&connection)
            .and_then(|peer| peer.pages.get(&request.page))
            .ok_or(ServerError::Invalid("public light history"))?;
        let latest = history.headers.back().copied();
        if request.acknowledged.is_some()
            && latest.is_some_and(|header| {
                header.cell_view == request.cell_view
                    && history.confirmed.is_none_or(|v| v < header.version)
            })
        {
            return Ok(None);
        }
        let base = request
            .acknowledged
            .and(history.confirmed)
            .and_then(|version| {
                history
                    .headers
                    .iter()
                    .find(|header| header.version == version)
            })
            .copied();
        let same_binding = history.public_cell_view == Some(request.cell_view);
        if base.is_some()
            && base == latest
            && same_binding
            && history.private_source == Some(source)
        {
            return Ok(None);
        }
        let delta = same_binding
            .then(|| {
                history
                    .private_source
                    .and_then(|old| self.delta(request.page, old, source))
            })
            .flatten();
        let mut patch = Vec::new();
        let mut complete = None;
        let changed = if let Some(delta) = &delta {
            for &(index, value) in delta {
                let index_usize = usize::from(index);
                let visible = admitted[index_usize]
                    == page.cell(core_types::LocalIdx::new(index).expect("page index"));
                let value = if visible { value } else { 0 };
                if history
                    .public_values
                    .as_ref()
                    .is_none_or(|old| old.get(index_usize) != value)
                {
                    patch.push((index, value));
                }
            }
            !patch.is_empty()
        } else {
            let mut values = Box::new([0; VOLUME]);
            for (index, value) in values.iter_mut().enumerate() {
                let local = core_types::LocalIdx::new(index as u16).expect("page index");
                if admitted[index] == page.cell(local) {
                    let sample = page.light(local);
                    *value = net::light::pack(sample.sky, sample.block, sample.rgb)
                        .ok_or(ServerError::Invalid("public light sample"))?;
                }
            }
            let changed = history.public_values.as_ref().is_none_or(|old| {
                values
                    .iter()
                    .enumerate()
                    .any(|(index, value)| old.get(index) != *value)
            });
            complete = Some(values);
            changed
        };
        let snapshot =
            request.acknowledged.is_none() || base != latest || !same_binding || delta.is_none();
        let peer = self.peers.get_mut(&connection).expect("validated peer");
        let history = peer.pages.get_mut(&request.page).expect("validated page");
        history.private_source = Some(source);
        history.public_cell_view = Some(request.cell_view);
        if let Some(values) = complete {
            if changed || history.public_values.is_none() {
                history.public_values = Some(Values::from_box(values));
            }
        } else if changed {
            history
                .public_values
                .as_mut()
                .expect("delta has public base")
                .patch(&patch);
        }
        if changed {
            history.public_source = history
                .public_source
                .checked_add(1)
                .ok_or(ServerError::Invalid("public light source exhausted"))?;
        }
        if !changed && !snapshot && same_binding {
            return Ok(None);
        }
        let header = match latest {
            Some(header) if !changed && same_binding => header,
            _ => {
                peer.next = peer
                    .next
                    .checked_add(1)
                    .ok_or(ServerError::Invalid("light view revision exhausted"))?;
                let header = Header {
                    page: request.page,
                    cell_view: request.cell_view,
                    version: peer.next,
                    source: Version(history.public_source),
                };
                history.headers.push_back(header);
                while history.headers.len() > HISTORY {
                    history.headers.pop_front();
                }
                header
            }
        };
        if snapshot {
            let update = Update::Snapshot(net::light::Snapshot {
                header,
                values: history
                    .public_values
                    .as_ref()
                    .expect("initialized public values")
                    .boxed(),
            });
            // A full image has now serviced this binding's repair credit.
            // Production normally popped it before calling this method; retain
            // the same lifecycle for direct owner calls and deferred requests.
            peer.requests.retain(|pending| {
                pending.page != request.page
                    || pending.cell_view != request.cell_view
                    || pending.acknowledged.is_some()
            });
            Ok(Some(update))
        } else {
            Ok(Some(Update::Delta(net::light::Delta {
                header,
                base: base.expect("delta base").version,
                changes: patch,
            })))
        }
    }
}
