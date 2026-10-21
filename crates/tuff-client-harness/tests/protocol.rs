use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tuff_client_harness::control::{Call, ControlListener};
use tuff_client_harness::protocol::{
    ClientState, Command, EntityInfo, parse_block, parse_entities, parse_fields, parse_light,
    parse_response,
};

#[tuff_core::watchdog]
#[test]
fn encodes_every_verb() {
    let cases = [
        (Command::State, "7 state"),
        (Command::Block { x: 1, y: -2, z: 3 }, "7 block 1 -2 3"),
        (Command::Light { x: 0, y: 64, z: 0 }, "7 light 0 64 0"),
        (Command::Entities { radius: 8.5 }, "7 entities 8.5"),
        (Command::Chat("hi there\nx".into()), "7 chat hi there x"),
        (Command::Command("tp 1 2 3".into()), "7 command tp 1 2 3"),
        (
            Command::Look {
                yaw: 90.0,
                pitch: -10.5,
            },
            "7 look 90 -10.5",
        ),
        (Command::Hotbar(3), "7 hotbar 3"),
        (
            Command::Key {
                name: "forward".into(),
                down: true,
            },
            "7 key forward down",
        ),
        (
            Command::Key {
                name: "jump".into(),
                down: false,
            },
            "7 key jump up",
        ),
        (Command::Use, "7 use"),
        (Command::Attack, "7 attack"),
        (
            Command::Click {
                slot: 4,
                button: 0,
                mode: "quick_move".into(),
            },
            "7 click 4 0 quick_move",
        ),
        (Command::Close, "7 close"),
        (Command::Respawn, "7 respawn"),
        (
            Command::Connect("[::1]:25565".into()),
            "7 connect [::1]:25565",
        ),
        (Command::Disconnect, "7 disconnect"),
        (Command::Screenshot("shot".into()), "7 screenshot shot"),
        (Command::WaitTicks(20), "7 wait_ticks 20"),
        (Command::ReleaseMouse, "7 release_mouse"),
        (
            Command::Eval("mc.player.getHealth()".into()),
            "7 eval mc.player.getHealth()",
        ),
        (
            Command::Set {
                path: "mc.options.x".into(),
                value: "3".into(),
            },
            "7 set mc.options.x = 3",
        ),
        (
            Command::Hook {
                class: "a.B".into(),
                method: "tick".into(),
            },
            "7 hook a.B tick",
        ),
        (
            Command::HookHits {
                class: "a.B".into(),
                method: "tick".into(),
            },
            "7 hook_hits a.B tick",
        ),
        (
            Command::HookAlter {
                class: "a.B".into(),
                method: "tick".into(),
                spec: "return=5".into(),
            },
            "7 hook_alter a.B tick return=5",
        ),
        (Command::Quit, "7 quit"),
    ];
    for (command, line) in cases {
        assert_eq!(command.encode(7), line);
    }
}

#[tuff_core::watchdog]
#[test]
fn parses_responses() {
    let ok = parse_response("12 ok a=1 b=2").unwrap();
    assert_eq!((ok.id, ok.outcome), (12, Ok("a=1 b=2".into())));
    let empty = parse_response("3 ok").unwrap();
    assert_eq!(empty.outcome, Ok(String::new()));
    let err = parse_response("4 err not in a world").unwrap();
    assert_eq!(err.outcome, Err("not in a world".into()));
    assert!(parse_response("x ok").is_err());
    assert!(parse_response("5 maybe").is_err());
    assert!(parse_response("").is_err());
}

#[tuff_core::watchdog]
#[test]
fn parses_typed_payloads() {
    let state = ClientState::parse(
        "connected=true screen=none title= x=1.500 y=64.000 z=-3.250 yaw=90.00 pitch=0.00 dim=minecraft:overworld mode=survival health=20.0 chunks=81 entities=4 tick=120",
    )
    .unwrap();
    assert!(state.connected);
    assert_eq!(state.position, Some([1.5, 64.0, -3.25]));
    assert_eq!(state.dimension.as_deref(), Some("minecraft:overworld"));
    assert_eq!(state.chunks, Some(81));
    assert_eq!(state.tick, Some(120));
    let menu = ClientState::parse("connected=false screen=TitleScreen title=").unwrap();
    assert!(!menu.connected && menu.position.is_none());
    assert!(ClientState::parse("screen=none").is_err());

    assert_eq!(
        parse_block("state=minecraft:oak_stairs[facing=north,half=bottom]").unwrap(),
        "minecraft:oak_stairs[facing=north,half=bottom]"
    );
    assert_eq!(parse_light("block=3 sky=15").unwrap(), (3, 15));
    assert!(parse_light("block=3").is_err());
    assert_eq!(
        parse_entities("list=5:minecraft:cow:1.000:2.000:3.000;6:minecraft:pig:0.000:1.000:0.000")
            .unwrap(),
        vec![
            EntityInfo {
                id: 5,
                kind: "minecraft:cow".into(),
                position: [1.0, 2.0, 3.0]
            },
            EntityInfo {
                id: 6,
                kind: "minecraft:pig".into(),
                position: [0.0, 1.0, 0.0]
            },
        ]
    );
    assert!(parse_entities("list=").unwrap().is_empty());
    assert!(parse_entities("list=junk").is_err());
    assert_eq!(parse_fields("a=1 b=2")["b"], "2");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn control_actor_roundtrips_with_a_fake_hook() {
    let listener = ControlListener::bind().await.unwrap();
    let (port, token) = (listener.port, listener.token.clone());
    let calls = listener.serve();

    let hook = tokio::spawn(async move {
        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(format!("0 hello {token}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let id = line.split(' ').next().unwrap().to_owned();
            let reply = if line.contains(" state") {
                format!("{id} ok connected=true screen=none title=\n")
            } else {
                format!("{id} err boom\n")
            };
            reader.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
    });

    let (tx, rx) = oneshot::channel();
    calls
        .send(Call {
            command: Command::State,
            reply: tx,
        })
        .await
        .unwrap();
    let ok = rx.await.unwrap().unwrap();
    assert!(ClientState::parse(&ok).unwrap().connected);

    let (tx, rx) = oneshot::channel();
    calls
        .send(Call {
            command: Command::Close,
            reply: tx,
        })
        .await
        .unwrap();
    assert_eq!(rx.await.unwrap(), Err("boom".into()));

    drop(calls);
    hook.await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn control_actor_rejects_a_bad_token() {
    let listener = ControlListener::bind().await.unwrap();
    let port = listener.port;
    let calls = listener.serve();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(b"0 hello wrong\n").await.unwrap();
    let (tx, rx) = oneshot::channel();
    let _ = calls
        .send(Call {
            command: Command::State,
            reply: tx,
        })
        .await;
    assert!(rx.await.is_err());
}
