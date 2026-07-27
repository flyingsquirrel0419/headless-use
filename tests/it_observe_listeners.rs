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
    assert!(
        canvas.opaque_interactive,
        "canvas must be opaque_interactive"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn direct_listener_div_is_promoted() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let refresh = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#refresh")
        .expect("refresh div promoted via getEventListeners");
    assert!(
        refresh.visual,
        "listener-promoted elements are heuristic (visual)"
    );
    assert!(!refresh.opaque_interactive, "few children: not opaque");
    s.shutdown().await;
}

#[tokio::test]
async fn delegation_container_is_opaque() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let board = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#board")
        .expect("board container found");
    assert!(
        board.opaque_interactive,
        "9 inert children + listener = opaque"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn delegated_container_over_real_links_is_not_opaque() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    let menu = obs
        .elements
        .iter()
        .find(|e| e.selector_hint == "#linkmenu")
        .expect("linkmenu container promoted (has click listener)");
    assert!(
        !menu.opaque_interactive,
        "interior links ARE enumerated; container must not claim otherwise"
    );
    // Its interior really is enumerated: all nine links have refs.
    let links = obs
        .elements
        .iter()
        .filter(|e| e.role == "link")
        .filter(|e| {
            [
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india",
            ]
            .contains(&e.name.as_str())
        })
        .count();
    assert_eq!(
        links, 9,
        "all nine links enumerated alongside the container"
    );
    s.shutdown().await;
}

#[tokio::test]
async fn inert_div_is_not_promoted() {
    let (_profile, s, srv) = session().await;
    s.open(&srv.url("delegated-tictactoe.html")).await.unwrap();
    s.wait(Default::default()).await.unwrap();
    let obs = s.observe().await.unwrap();
    assert!(
        !obs.elements.iter().any(|e| e.selector_hint == "#inert"),
        "no listener: must not be promoted"
    );
    s.shutdown().await;
}
