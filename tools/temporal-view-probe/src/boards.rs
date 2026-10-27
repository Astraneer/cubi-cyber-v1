//! Actual public landmark and authoritative backing through the shared View.
use super::*;
const SESSION: u64 = (1 << 40) | 7;
pub(super) fn landmark() -> net::spawn::Landmark {
    let message = net::ServerMessage::SpawnLandmark(net::spawn::Landmark {
        session: SESSION,
        center: BlockPos {
            space: SpaceId(0),
            x: 18,
            y: 2,
            z: -3,
        },
    });
    let bytes = net::encode_server(&message).unwrap();
    let net::ServerMessage::SpawnLandmark(value) = net::decode_server(&bytes).unwrap() else {
        unreachable!()
    };
    value
}
fn camera(view: &mut ClientView, x: f64) -> Result<(), String> {
    let mut player = sim::Player::new([x, 4., 8.]);
    player.yaw = 180.;
    view.install_remote_player(&player, Tick(1))
}
pub(super) async fn run(mut host: Host) -> Result<Value, String> {
    host.gpu
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let mut view = build_inner(&host.gpu, true, false, 1, false, true)?;
    let mut reference = build_inner(&host.gpu, false, false, 1, false, true)?;
    camera(&mut view, 8.)?;
    camera(&mut reference, 8.)?;
    actor(&mut view, None, 0.)?;
    actor(&mut reference, None, 0.)?;
    let mut results = vec![];
    results.push(
        host.frame(&mut reference, "boards-background-reference", false, 0)
            .await?,
    );
    reference.set_recipe_board_landmark(Some(landmark()))?;
    view.set_recipe_board_landmark(Some(landmark()))?;
    results.push(
        host.frame(&mut reference, "boards-reference", false, 0)
            .await?,
    );
    for phase in 0..8 {
        let frame = host.frame(&mut view, "boards-static", false, 0).await?;
        if phase == 7 {
            results.push(frame);
        }
    }
    camera(&mut view, 8.5)?;
    camera(&mut reference, 8.5)?;
    results.push(host.frame(&mut view, "boards-pan", false, 0).await?);
    results.push(
        host.frame(&mut reference, "boards-pan-reference", false, 0)
            .await?,
    );
    view.set_recipe_board_landmark(None)?;
    results.push(host.frame(&mut view, "boards-abandoned", false, 1).await?);
    results.push(host.frame(&mut view, "boards-discarded", false, 2).await?);
    results.push(host.frame(&mut view, "boards-restored", true, 0).await?);
    results.push(host.frame(&mut view, "boards-removed", false, 0).await?);
    reference.set_recipe_board_landmark(None)?;
    results.push(
        host.frame(&mut reference, "boards-removed-reference", false, 0)
            .await?,
    );
    view.set_recipe_board_landmark(Some(landmark()))?;
    results.push(host.frame(&mut view, "boards-reenabled", false, 0).await?);
    if let Some(error) = host.gpu.device.pop_error_scope().await {
        return Err(error.to_string());
    }
    Ok(
        json!({"extent":[W,H],"results":results,"session":SESSION.to_string(),
        "backend":format!("{:?}",host.gpu.adapter.get_info().backend),
        "source":"production Rust View; wire-decoded public landmark; authoritative raw/LIGHT backing; namespace19; synchronous diagnostic jobs"}),
    )
}
