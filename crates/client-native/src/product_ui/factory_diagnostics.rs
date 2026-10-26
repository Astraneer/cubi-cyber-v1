//! Fixed-capacity observations of permitted windows and uploaded pipe packets.
//! All output is opt-in diagnostic data; it never drives simulation or rendering.
use client::{network::Connection, view::ClientView};
use core_types::{BlockPos, PageId};
use render::entity_gpu::EntityInstance;

const PACKETS: usize = 64;
const PIPE: u32 = client::network::machine_packets::PIPE_INSTANCE_FLAG;
type UploadStamp = (u64, u64);
#[derive(Clone, Copy, PartialEq, Eq)]
struct WindowStamp {
    id: u64,
    revision: u64,
    inventory: u64,
    read_only: bool,
    closed: bool,
    kind: [u8; 2],
    binding: Option<BlockPos>,
    cursor: [u16; 3],
    slots: [[u16; 3]; 54],
    count: usize,
    progress: u16,
    energy: u64,
    sides: [u8; 6],
}
#[derive(Clone, Copy, PartialEq)]
struct PacketStamp {
    gpu: EntityInstance,
    page: Option<PageId>,
    pose: Option<net::machine_packets::Pose>,
    view: u64,
    revision: u64,
    raw_view: u64,
    scene_view: u64,
    raw_source: u64,
    raw_block: u16,
}
pub(super) struct Observations {
    window: Option<WindowStamp>,
    session: Option<u64>,
    upload: Option<UploadStamp>,
    rows: [Option<PacketStamp>; PACKETS],
    count: usize,
    truncated: bool,
    last_emit: Option<(u64, u64, u64, u64)>,
    completed: u64,
    submitted: u64,
    awaiting_world_sample: bool,
    encoded_upload: Option<UploadStamp>,
    encoded_frame: u64,
}
impl Default for Observations {
    fn default() -> Self {
        Self {
            window: None,
            session: None,
            upload: None,
            rows: [None; PACKETS],
            count: 0,
            truncated: false,
            last_emit: None,
            completed: 0,
            submitted: 0,
            awaiting_world_sample: false,
            encoded_upload: None,
            encoded_frame: 0,
        }
    }
}
impl Observations {
    pub(super) fn completed(&mut self, frame: u64) {
        self.completed = self.completed.max(frame);
    }
    pub(super) fn submitted(&mut self, frame: u64, world: bool, ui: Option<u64>) {
        self.submitted = frame;
        if world {
            // Existing native order: next sync precedes upload_ready. With one
            // in-flight frame, that sync sees this frame's unchanged GPU bank.
            self.encoded_frame = frame;
            self.awaiting_world_sample = true;
        }
        if self.count > 0 || self.truncated {
            println!(
                "NATIVE_UI_FACTORY_FRAME {{\"frame\":{},\"world\":{},\"ui_revision\":{},\"ui_known\":{}}}",
                frame,
                world,
                ui.unwrap_or(0),
                ui.is_some(),
            );
        }
    }
    pub(super) fn observe(&mut self, network: &Connection, view: &ClientView, ui_revision: u64) {
        let Some(welcome) = network.welcome() else {
            return;
        };
        if self.session != Some(welcome.session) {
            self.window = None;
            self.upload = None;
            self.rows.fill(None);
            self.count = 0;
            self.truncated = false;
            self.last_emit = None;
            self.encoded_upload = None;
            self.awaiting_world_sample = false;
            self.encoded_frame = 0;
            self.session = Some(welcome.session);
        }
        if let Some(window) = network.window() {
            let mut slots = [[0; 3]; 54];
            for (out, stack) in slots.iter_mut().zip(&window.container) {
                *out = [stack.item, stack.count, stack.damage];
            }
            let kind = match window.kind {
                net::window::WindowKind::Inventory => [0, 0],
                net::window::WindowKind::Workbench => [1, 0],
                net::window::WindowKind::Chest(n) => [2, n],
                net::window::WindowKind::Furnace => [3, 0],
                net::window::WindowKind::Machine(n) => [4, n],
            };
            let next = WindowStamp {
                id: window.guard.id,
                revision: window.guard.revision,
                inventory: window.guard.inventory_revision,
                read_only: window.read_only,
                closed: window.closed,
                kind,
                binding: window.binding,
                cursor: [
                    window.cursor.item,
                    window.cursor.count,
                    window.cursor.damage,
                ],
                slots,
                count: window.container.len(),
                progress: window.progress,
                energy: window.energy,
                sides: window.sides,
            };
            if self.window != Some(next) {
                let position = next.binding.map_or([0; 3], |p| [p.x, p.y, p.z]);
                println!(
                    "NATIVE_UI_FACTORY_WINDOW {{\"session\":{},\"observed_tick\":{},\"id\":{},\"revision\":{},\"inventory_revision\":{},\"ui_revision\":{},\"kind\":{:?},\"read_only\":{},\"closed\":{},\"bound\":{},\"space\":{},\"position\":{:?},\"cursor\":{:?},\"slots\":{:?},\"progress\":{},\"energy\":{},\"sides\":{:?}}}",
                    welcome.session,
                    network.acknowledged_tick().0,
                    next.id,
                    next.revision,
                    next.inventory,
                    ui_revision,
                    next.kind,
                    next.read_only,
                    next.closed,
                    next.binding.is_some(),
                    next.binding.map_or(0, |p| p.space.0),
                    position,
                    next.cursor,
                    &next.slots[..next.count],
                    next.progress,
                    next.energy,
                    next.sides,
                );
                self.window = Some(next);
            }
        }
        let Some(stats) = view.entity_stats() else {
            return;
        };
        let upload = (stats.upload_bytes, stats.instance_records);
        if self.awaiting_world_sample {
            self.encoded_upload = Some(upload);
            self.awaiting_world_sample = false;
        }
        let mut changed = false;
        if self.upload != Some(upload) {
            let old_count = self.count;
            self.count = 0;
            self.truncated = false;
            // Production RemoteSession appends the pipe packet group, followed
            // only by its optional local body. Inspect at most66 tail records,
            // never scan all actors or allocate a per-frame staging vector.
            if let Some(records) = view.factory_uploaded_entities() {
                for (offset, instance) in records.iter().rev().take(PACKETS + 2).enumerate() {
                    if instance.flags & PIPE == 0 {
                        if offset == 0 && instance.kind == 5 {
                            continue;
                        }
                        break;
                    }
                    if self.count == PACKETS {
                        self.truncated = true;
                        break;
                    }
                    self.rows[self.count] = Some(PacketStamp {
                        gpu: *instance,
                        page: None,
                        pose: None,
                        view: 0,
                        revision: 0,
                        raw_view: 0,
                        scene_view: 0,
                        raw_source: 0,
                        raw_block: 0,
                    });
                    self.count += 1;
                }
            }
            self.rows[self.count..].fill(None);
            self.upload = Some(upload);
            changed = self.count > 0 || old_count > 0;
        }
        for row in self.rows[..self.count].iter_mut().flatten() {
            let previous = *row;
            let id = u64::from(row.gpu.id_low) | (u64::from(row.gpu.id_high) << 32);
            let current = network.packet_scene().get(id);
            row.page = current.map(|(page, _)| page);
            row.pose = current.map(|(_, pose)| pose);
            let page = row.page.and_then(|p| network.packet_scene().page(p));
            row.view = page.and_then(|p| p.view_version()).map_or(0, |v| v.0);
            row.revision = page.and_then(|p| p.revision()).unwrap_or(0);
            let raw = row.page.and_then(|p| network.replica().page(p));
            row.raw_view = raw.map_or(0, |p| p.version);
            row.raw_source = raw.map_or(0, |p| p.source_version.0);
            row.raw_block = row
                .pose
                .zip(raw)
                .map_or(0, |(p, raw)| raw.cells[usize::from(p.index.get())].id());
            row.scene_view = row
                .page
                .and_then(|p| view.scene.authoritative_version(p))
                .map_or(0, |v| v.view);
            changed |= previous != *row;
        }
        let bank_frame = if self.encoded_upload == Some(upload) {
            self.encoded_frame
        } else {
            0
        };
        let stamp = (
            self.submitted,
            self.completed,
            view.rendered_camera_signature(),
            bank_frame,
        );
        if (self.count == 0 && !changed && !self.truncated)
            || (!changed && self.last_emit == Some(stamp))
        {
            return;
        }
        self.last_emit = Some(stamp);
        let camera = view.factory_encoded_camera();
        println!(
            "NATIVE_UI_FACTORY_PACKETS {{\"session\":{},\"observed_tick\":{},\"uploaded_bytes\":{},\"uploaded_records\":{},\"count\":{},\"truncated\":{},\"submitted_frame\":{},\"completed_frame\":{},\"bank_world_frame\":{},\"camera_signature\":{},\"visual_revision\":{},\"camera_known\":{},\"origin\":{:?},\"view_projection\":{:?}}}",
            welcome.session,
            network.acknowledged_tick().0,
            upload.0,
            upload.1,
            self.count,
            self.truncated,
            self.submitted,
            self.completed,
            bank_frame,
            stamp.2,
            view.rendered_visual_revision(),
            camera.is_some(),
            camera.map_or([0; 3], |(_, origin)| origin),
            camera.map_or([[0.; 4]; 4], |(camera, _)| camera.view_projection),
        );
        for row in self.rows[..self.count].iter().flatten() {
            let id = u64::from(row.gpu.id_low) | (u64::from(row.gpu.id_high) << 32);
            let position = row.page.map_or([0; 3], |p| [p.x, p.y, p.z]);
            let pose = row.pose;
            println!(
                "NATIVE_UI_FACTORY_PACKET {{\"id\":{},\"wire_present\":{},\"page\":{:?},\"space\":{},\"view\":{},\"revision\":{},\"raw_view\":{},\"raw_source\":{},\"scene_view\":{},\"raw_block\":{},\"item\":{},\"count\":{},\"index\":{},\"entry\":{},\"exit\":{},\"progress\":{},\"epoch\":{},\"gpu_position\":{:?},\"gpu_kind\":{},\"gpu_flags\":{},\"gpu_scale\":{}}}",
                id,
                pose.is_some(),
                position,
                row.page.map_or(0, |p| p.space.0),
                row.view,
                row.revision,
                row.raw_view,
                row.raw_source,
                row.scene_view,
                row.raw_block,
                pose.map_or(0, |p| p.item),
                pose.map_or(0, |p| p.count),
                pose.map_or(0, |p| p.index.get()),
                pose.map_or(0, |p| p.entry),
                pose.map_or(0, |p| p.exit),
                pose.map_or(0, |p| p.progress),
                pose.map_or(0, |p| p.epoch.0),
                row.gpu.position,
                row.gpu.kind,
                row.gpu.flags,
                row.gpu.scale,
            );
        }
    }
}
