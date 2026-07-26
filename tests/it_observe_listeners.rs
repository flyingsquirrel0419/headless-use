//! Integration tests: listener-detection observe pass + opaque flagging.

mod common;

async fn session() -> (
    common::TempProfile,
    headless_use::session::Session,
    common::FixtureServer,
) {
    common::init();
    let srv = common::FixtureServer::start().await;
    let profile = common::TempProfile::new();
    let s = headless_use::session::Session::start(profile.launch_opts())
        .await
        .expect("session start");
    (profile, s, srv)
}

#[tokio::test]
async fn canvas_is_flagged_opaque() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let canvas = obs
        .elements
        .iter()
        .find(|e| e.tag_name == "canvas")
        .expect("canvas captured");
    assert!(canvas.opaque_interactive, "canvas must be opaque_interactive");
    s.shutdown().await;
}
