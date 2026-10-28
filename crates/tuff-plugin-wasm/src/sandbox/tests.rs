use super::*;
use crate::wit::bindings::capability_types::CallError;
use crate::wit::bindings::types::{
    CommandNode, NodeKind, Parser, RichClick, RichSpan, TeamSpec, TextKind, TitleSpec,
};
use crate::wit::testing::{self, Data, RET, export, ok_code, ok_unit};

fn sandbox_with(body: &str, limits: Limits) -> Result<Sandbox, Fault> {
    let bytes = wat::parse_str(testing::module(body)).unwrap();
    Sandbox::new(&engine(), &bytes, "test", limits)
}

fn sandbox(body: &str) -> Sandbox {
    sandbox_with(body, Limits::default()).unwrap()
}

fn steve() -> Player {
    Player::new(7, "Steve")
}

/// `name` taking the rest of the line, as the SDK registers a command.
fn line_tree(name: &str) -> CommandTree {
    let node = |name: &str, kind, children: Vec<u16>| CommandNode {
        name: name.into(),
        kind,
        children,
        executes: Some(0),
        redirect: None,
        permission: None,
        suggest: false,
    };
    CommandTree {
        nodes: vec![
            node(name, NodeKind::Literal, vec![1]),
            node(
                "args",
                NodeKind::Argument(Parser::Text(TextKind::Greedy)),
                vec![],
            ),
        ],
    }
}

/// A `register` that runs `more`, then claims `/name`.
fn register(data: &mut Data, name: &str, more: &str) -> String {
    let tree = line_tree(name);
    format!(
        "(func {} {more} {} {})",
        export("register"),
        data.call("register-command", &[tree.to_value()]),
        ok_unit()
    )
}

fn expect_err_at_ret() -> String {
    format!("(if (i32.ne (i32.load8_u (i32.const {RET})) (i32.const 1)) (then unreachable))")
}

#[tuff_core::watchdog]
#[test]
fn registers_runs_commands_and_keeps_player_state() {
    let mut data = Data::new();
    let subscribe = data.call("subscribe", &[EventKinds::CHAT.to_value()]);
    let register = register(&mut data, "echo", &subscribe);
    let key = data.string("k");
    // An invocation is (handler, sender: 5 slots, args, line); the event
    // arrives as a pointer, and a chat's message follows its player at
    // offset 32 of the payload.
    let body = format!(
        r#"{register}
        (func {run}
          (call $reply (local.get 8) (local.get 9) (i32.const {RET}))
          (call $schedule (i64.const 3) (i32.const 9) (i32.const {RET}))
          {code})
        (func {event}
          (call $state-get (i32.const {k}) (i32.const {k_len}) (i32.const {RET}))
          (if (i32.and (i32.eqz (i32.load8_u (i32.const {RET}))) (i32.load8_u (i32.const {some})))
            (then (call $broadcast (i32.load offset=32 (local.get 0)) (i32.load offset=36 (local.get 0)) (i32.const {RET})))
            (else (call $state-set (i32.const {k}) (i32.const {k_len}) (i32.const 1)
                    (i32.load offset=32 (local.get 0)) (i32.load offset=36 (local.get 0)) (i32.const {RET})))))
        {data}"#,
        run = export("run-command"),
        code = ok_code("(local.get 9)"),
        event = export("on-event"),
        k = key.0,
        k_len = key.1,
        some = RET + 4,
        data = data.wat(),
    );
    let mut sandbox = sandbox(&body);
    let registration = sandbox.register().unwrap();
    assert_eq!(registration.commands, [line_tree("echo")]);
    assert_eq!(registration.events, EventKinds::CHAT);
    let (result, effects) = sandbox.command(steve(), "echo hi").unwrap();
    assert_eq!(result, Ok(7));
    assert_eq!(effects.replies, ["echo hi"]);
    assert_eq!(effects.timers, [(3, 9)]);

    let chat = |message: &str| {
        Event::Chat(wit::ChatMessage {
            player: steve().to_wit(),
            message: message.into(),
        })
    };
    assert_eq!(
        sandbox.event(&chat("one"), Some(steve())).unwrap(),
        Effects::default()
    );
    assert_eq!(sandbox.players_with_state(), 1);
    let second = sandbox.event(&chat("two"), Some(steve())).unwrap();
    assert_eq!(second.broadcasts, ["two"]);
    sandbox.forget_player(7);
    assert_eq!(sandbox.players_with_state(), 0);
    assert_eq!(
        sandbox.event(&chat("three"), None).unwrap(),
        Effects::default()
    );
    let join = Event::Join(steve().to_wit());
    assert_eq!(
        sandbox.event(&join, Some(steve())).unwrap(),
        Effects::default(),
        "not subscribed"
    );
}

#[tuff_core::watchdog]
#[test]
fn lines_the_tree_refuses_never_reach_the_guest() {
    let mut data = Data::new();
    let tree = CommandTree {
        nodes: vec![CommandNode {
            name: "only".into(),
            kind: NodeKind::Literal,
            children: vec![],
            executes: Some(3),
            redirect: None,
            permission: None,
            suggest: false,
        }],
    };
    let call = data.call("register-command", &[tree.to_value()]);
    let body = format!(
        "(func {} {call} {}) (func {} unreachable) {}",
        export("register"),
        ok_unit(),
        export("run-command"),
        data.wat()
    );
    let mut sandbox = sandbox(&body);
    sandbox.register().unwrap();
    let (result, effects) = sandbox.command(steve(), "only extra").unwrap();
    assert!(result.unwrap_err().contains("<--[HERE]"));
    assert_eq!(effects, Effects::default());
    assert!(sandbox.command(steve(), "only").is_err(), "the guest traps");
}

#[tuff_core::watchdog]
#[test]
fn registering_outside_register_is_refused() {
    let mut data = Data::new();
    let late = data.call("register-command", &[line_tree("late").to_value()]);
    let body = format!(
        "(func {} {}) (func {} {late} {}) {}",
        export("register"),
        ok_unit(),
        export("enable"),
        expect_err_at_ret(),
        data.wat()
    );
    let mut sandbox = sandbox(&body);
    assert_eq!(sandbox.register().unwrap(), Registration::default());
    assert_eq!(sandbox.enable().unwrap(), Effects::default());
}

#[tuff_core::watchdog]
#[test]
fn hostile_pointers_and_names_fault() {
    let register = |call: &str| {
        format!(
            "(func {} {call} {}) (data (i32.const 2048) \"\\ff\\fe\")",
            export("register"),
            ok_unit()
        )
    };
    for call in [
        format!("(call $register-command (i32.const 65530) (i32.const 100) (i32.const {RET}))"),
        format!("(call $register-command (i32.const -8) (i32.const 1) (i32.const {RET}))"),
        format!("(call $register-command (i32.const 0) (i32.const 1000000) (i32.const {RET}))"),
        format!("(call $register-command (i32.const 4) (i32.const 1) (i32.const {RET}))"),
        "(call $log (i32.const 2) (i32.const 2048) (i32.const 2))".to_owned(),
        "(call $log (i32.const 9) (i32.const 2048) (i32.const 0))".to_owned(),
        "(call $log (i32.const 2) (i32.const 0) (i32.const 40000))".to_owned(),
        "(call $reply (i32.const 0) (i32.const 0) (i32.const -4))".to_owned(),
    ] {
        assert!(sandbox(&register(&call)).register().is_err(), "{call}");
    }
    let mut data = Data::new();
    let bad_name = data.call("register-command", &[line_tree("Bad Name").to_value()]);
    let body = format!(
        "(func {} {bad_name} {} {}) {}",
        export("register"),
        expect_err_at_ret(),
        ok_unit(),
        data.wat()
    );
    assert_eq!(sandbox(&body).register().unwrap(), Registration::default());
}

#[tuff_core::watchdog]
#[test]
fn runaway_code_runs_out_of_fuel() {
    let limits = Limits {
        fuel: 100_000,
        ..Limits::default()
    };
    let mut data = Data::new();
    let register = register(&mut data, "spin", "");
    let body = format!(
        "{register} (func {} (loop $forever (br $forever)) (i32.const 0)) {}",
        export("run-command"),
        data.wat()
    );
    let mut sandbox = sandbox_with(&body, limits).unwrap();
    sandbox.register().unwrap();
    let fault = sandbox.command(steve(), "spin").unwrap_err();
    assert!(fault.0.contains("fuel"), "{fault}");
}

#[tuff_core::watchdog]
#[test]
fn default_fuel_allows_heavy_command() {
    let mut data = Data::new();
    let register = register(&mut data, "heavy", "");
    let body = format!(
        "{register} (func {} (local $i i32) (loop $again (local.set $i (i32.add (local.get $i) (i32.const 1))) (br_if $again (i32.lt_u (local.get $i) (i32.const 200000000)))) (i32.const 0)) {}",
        export("run-command"),
        data.wat()
    );
    let mut sandbox = sandbox_with(&body, Limits::default()).unwrap();
    sandbox.register().unwrap();
    assert!(sandbox.command(steve(), "heavy").is_ok());
}

#[tuff_core::watchdog]
#[test]
fn unbounded_recursion_traps() {
    let body = format!(
        "(func $down (call $down)) (func {} (call $down) {})",
        export("register"),
        ok_unit()
    );
    let recursion = sandbox(&body).register().unwrap_err();
    assert!(recursion.0.contains("trap"), "{recursion}");
}

#[tuff_core::watchdog]
#[test]
fn memory_and_message_limits_hold() {
    let limits = Limits {
        memory: 4 << 16,
        messages: 2,
        ..Limits::default()
    };
    let mut data = Data::new();
    let register = register(&mut data, "m", "");
    let text = data.string("m");
    let say = format!(
        "(call $broadcast (i32.const {}) (i32.const {}) (i32.const {RET}))",
        text.0, text.1
    );
    let body = format!(
        "{register} (func {} {say} {say} {say} {} {}) {}",
        export("run-command"),
        expect_err_at_ret(),
        ok_code("(memory.grow (i32.const 4))"),
        data.wat()
    );
    let mut sandbox = sandbox_with(&body, limits).unwrap();
    sandbox.register().unwrap();
    let (grown, effects) = sandbox.command(steve(), "m").unwrap();
    assert_eq!(grown, Ok(-1), "growing past the memory cap must fail");
    assert_eq!(effects.broadcasts.len(), 2);
}

#[tuff_core::watchdog]
#[test]
fn modules_missing_required_exports_are_refused() {
    let no_register = wat::parse_str(testing::module("")).unwrap();
    assert!(Sandbox::new(&engine(), &no_register, "test", Limits::default()).is_err());
    let bare = format!(
        r#"(module (memory (export "memory") 1) (func {} (i32.const 0)))"#,
        export("register")
    );
    let bare = wat::parse_str(bare).unwrap();
    assert!(Sandbox::new(&engine(), &bare, "test", Limits::default()).is_err());
    let mut data = Data::new();
    let register = register(&mut data, "x", "");
    let no_command = format!("{register} {}", data.wat());
    assert!(sandbox(&no_command).register().is_err());
    let wrong_type = format!(
        "{register} (func (export \"tuff:plugin/guest@0.1.0#on-timer\") (param i64)) {}",
        data.wat()
    );
    assert!(sandbox_with(&wrong_type, Limits::default()).is_err());
    assert!(Sandbox::new(&engine(), b"\0asm\x01\0\0\0\xff", "test", Limits::default()).is_err());
}

#[tuff_core::watchdog]
#[test]
fn a_guest_error_from_register_fails_the_load() {
    let mut data = Data::new();
    let why = data.string("not today");
    let body = format!(
        "(func {} {}) {}",
        export("register"),
        testing::err_text(why),
        data.wat()
    );
    let fault = sandbox(&body).register().unwrap_err();
    assert!(fault.0.contains("not today"), "{fault}");
}

#[tuff_core::watchdog]
#[test]
fn messages_cross_with_their_bytes_intact() {
    let body = format!(
        "(func {} {}) (func {} (call $broadcast (local.get 0) (local.get 1) (i32.const {RET}))) {}",
        export("register"),
        ok_unit(),
        export("on-message"),
        "(func (export \"tuff:plugin/guest@0.1.0#on-timer\") (param i32))"
    );
    let mut sandbox = sandbox(&body);
    sandbox.register().unwrap();
    let echoed = sandbox.message(&incoming("other", 1, b"hello")).unwrap();
    assert_eq!(echoed.broadcasts, ["other"]);
    assert_eq!(sandbox.timer(3).unwrap(), Effects::default());
}

fn incoming(
    sender: &str,
    method: u64,
    payload: &[u8],
) -> crate::wit::bindings::capability_types::Incoming {
    crate::wit::bindings::capability_types::Incoming {
        sender: sender.into(),
        interface: "t:i".into(),
        method,
        payload: payload.to_vec(),
        request: None,
    }
}

fn with_providers(state: &mut State, providers: &[(&str, &[u64])]) {
    let mut index = crate::host::Index::default();
    index.providers.insert(
        "t:i".into(),
        providers
            .iter()
            .map(|(id, methods)| ((*id).to_owned(), methods.to_vec()))
            .collect(),
    );
    let (tx, rx) = tokio::sync::watch::channel(Arc::new(index));
    std::mem::forget(tx);
    state.index = Some(rx);
}

#[tuff_core::watchdog]
#[test]
fn requests_and_notifies_fail_fast_without_a_provider() {
    let mut state = state();
    let call = |state: &mut State, target: Option<&str>, method, payload: &[u8]| {
        state
            .host_request(target.map(str::to_owned), "t:i", method, payload, 0)
            .unwrap()
    };
    assert_eq!(call(&mut state, None, 1, b""), Err(CallError::NoProvider));
    with_providers(&mut state, &[("a", &[1]), ("b", &[1, 2])]);
    assert_eq!(call(&mut state, None, 1, b""), Err(CallError::Ambiguous));
    assert_eq!(
        call(&mut state, Some("a"), 2, b""),
        Err(CallError::NoMethod)
    );
    assert_eq!(call(&mut state, None, 2, b""), Ok(1));
    assert_eq!(
        call(&mut state, Some("a"), 1, &vec![0; abi::MESSAGE_BYTES + 1]),
        Err(CallError::TooLarge)
    );
    assert_eq!(state.host_notify(None, "t:i", 1, b"x").unwrap(), Ok(2));
    assert_eq!(state.effects.messages.len(), 3);
    assert_eq!(state.host_plugin_loaded("a"), Ok(None));
}

#[tuff_core::watchdog]
#[test]
fn responses_need_a_delivered_request_and_requests_are_bounded() {
    let mut state = state();
    with_providers(&mut state, &[("a", &[1])]);
    assert!(state.host_respond(7, Ok(vec![1])).unwrap().is_err());
    state.owed.insert(7);
    assert_eq!(state.host_respond(7, Ok(vec![1])), Ok(Ok(())));
    assert!(state.host_respond(7, Ok(vec![1])).unwrap().is_err());
    assert_eq!(state.effects.answers.len(), 1);
    state.limits.sends = usize::MAX;
    for _ in 0..abi::MAX_OUTSTANDING_REQUESTS {
        state.host_request(None, "t:i", 1, b"", 0).unwrap().unwrap();
    }
    assert!(matches!(
        state.host_request(None, "t:i", 1, b"", 0).unwrap(),
        Err(CallError::Refused(_))
    ));
    assert!(matches!(
        state
            .host_request(None, "t:i", 1, b"", abi::MAX_REQUEST_TICKS + 1)
            .unwrap(),
        Err(CallError::Refused(_))
    ));
}

#[tuff_core::watchdog]
#[test]
fn a_busy_reply_reaches_the_guest_and_frees_the_request_slot() {
    use crate::wit::bindings::capability_types::{IoEvent, ReplyDone};
    let mut data = Data::new();
    let busy = data.string("busy");
    let body = format!(
        "(func {} {}) (func {}
           (if (i32.and
                 (i32.and
                   (i32.eq (local.get 0) (i32.const 10))
                   (i32.eq (local.get 2) (i32.const 1)))
                 (i32.eq (local.get 3) (i32.const 6)))
             (then (call $broadcast (i32.const {}) (i32.const {}) (i32.const {RET}))))) {} {}",
        export("register"),
        ok_unit(),
        export("on-io"),
        busy.0,
        busy.1,
        "(func (export \"tuff:plugin/guest@0.1.0#on-timer\") (param i32))",
        data.wat()
    );
    let mut sandbox = sandbox(&body);
    sandbox.register().unwrap();
    grant_groups(sandbox.state(), &["cross-plugin"]);
    with_providers(sandbox.state(), &[("a", &[1])]);
    sandbox.state().limits.sends = usize::MAX;
    let mut handles = Vec::new();
    for _ in 0..abi::MAX_OUTSTANDING_REQUESTS {
        handles.push(
            sandbox
                .state()
                .host_request(None, "t:i", 1, b"", 0)
                .unwrap()
                .unwrap(),
        );
    }
    assert!(
        sandbox
            .state()
            .host_request(None, "t:i", 1, b"", 0)
            .unwrap()
            .is_err()
    );
    let effects = sandbox
        .io(&IoEvent::Reply(ReplyDone {
            handle: handles[0],
            outcome: Err(CallError::Busy),
        }))
        .unwrap();
    assert_eq!(effects.broadcasts, ["busy"]);
    assert!(
        sandbox
            .state()
            .host_request(None, "t:i", 1, b"", 0)
            .unwrap()
            .is_ok()
    );
}

fn closed_state() -> State {
    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Running;
    state
}

const GROUPS: [&str; 4] = ["world-write", "player-write", "player-read", "cross-plugin"];

fn group_policy(groups: &[&str]) -> Arc<crate::caps::Policy> {
    use crate::caps::{Grants, Policy, Request};
    let requests = groups
        .iter()
        .flat_map(|group| {
            Request::parse_line(&format!("cap.{group}"), "all")
                .unwrap()
                .unwrap()
        })
        .collect();
    let body: String = groups.iter().map(|g| format!("{g} = all\n")).collect();
    Arc::new(Policy::new(
        requests,
        Grants::parse(&format!("[p]\n{body}")).unwrap().get("p"),
    ))
}

fn grant_groups(state: &mut State, groups: &[&str]) {
    let (tx, rx) = tokio::sync::watch::channel(group_policy(groups));
    std::mem::forget(tx);
    state.policy = Some(rx);
}

fn state() -> State {
    let mut state = closed_state();
    grant_groups(&mut state, &GROUPS);
    state
}

fn acting(state: &mut State, player: Player) -> &mut State {
    state.caller = Some(player);
    state
}

#[tuff_core::watchdog]
#[test]
fn declared_permissions_decide_by_level_and_are_namespaced() {
    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Register;
    assert_eq!(state.host_declare_permission("test.use", 0), Ok(Ok(())));
    assert_eq!(state.host_declare_permission("test.admin", 3), Ok(Ok(())));
    for (node, level) in [
        ("other.use", 0),
        ("test.", 0),
        ("test.use", 0),
        ("test.x", 5),
    ] {
        assert!(
            matches!(
                state.host_declare_permission(node, level),
                Ok(Err(RegisterError::Invalid(_)))
            ),
            "{node} {level}"
        );
    }
    assert_eq!(
        state.registration.permissions,
        [
            Permission {
                node: "test.use".into(),
                level: 0
            },
            Permission {
                node: "test.admin".into(),
                level: 3
            }
        ]
    );
    state.phase = Phase::Running;
    assert_eq!(state.host_has_permission("test.use"), Ok(None));
    let at = |level| Player {
        level: Some(level),
        ..steve()
    };
    let checks = |state: &mut State| {
        (
            state.host_has_permission("test.use").unwrap(),
            state.host_has_permission("test.admin").unwrap(),
        )
    };
    assert_eq!(
        checks(acting(&mut state, at(PermissionLevel::All))),
        (Some(true), Some(false))
    );
    assert_eq!(
        checks(acting(&mut state, at(PermissionLevel::Gamemasters))),
        (Some(true), Some(false))
    );
    assert_eq!(
        checks(acting(&mut state, at(PermissionLevel::Admins))),
        (Some(true), Some(true))
    );
    assert_eq!(
        state.host_declare_permission("test.late", 0),
        Ok(Err(RegisterError::WrongPhase))
    );
}

#[tuff_core::watchdog]
#[test]
fn storage_reads_its_own_writes_and_keeps_to_its_quota() {
    let mut state = State::new(
        "test",
        Limits {
            storage: Quota { bytes: 8, keys: 4 },
            ..Limits::default()
        },
    );
    state.phase = Phase::Running;
    assert_eq!(
        state.host_storage_set("k", Some(b"value".to_vec())),
        Ok(Ok(()))
    );
    assert_eq!(state.host_storage_get("k"), Ok(Some(b"value".to_vec())));
    assert!(
        state
            .host_storage_set("k", Some(b"toolongvalue".to_vec()))
            .unwrap()
            .is_err()
    );
    assert_eq!(state.host_storage_set("k", None), Ok(Ok(())));
    assert_eq!(state.effects.stored, [("k".to_owned(), None)]);
    assert!(state.storage.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn repeated_storage_writes_coalesce_and_distinct_keys_are_capped() {
    let mut state = State::new(
        "test",
        Limits {
            storage: Quota {
                bytes: 1 << 20,
                keys: 4,
            },
            ..Limits::default()
        },
    );
    state.phase = Phase::Running;
    for round in 0..1000u32 {
        assert_eq!(
            state.host_storage_set("k", Some(round.to_le_bytes().to_vec())),
            Ok(Ok(()))
        );
    }
    assert_eq!(state.effects.stored.len(), 1);
    for key in ["a", "b", "c"] {
        assert_eq!(state.host_storage_set(key, None), Ok(Ok(())));
    }
    assert_eq!(
        state.host_storage_set("d", None),
        Ok(Err(HostError::LimitReached))
    );
    assert_eq!(state.effects.stored.len(), 4);
}

fn route_answer(len: usize) -> RouteResponse {
    RouteResponse {
        status: 200,
        headers: Vec::new(),
        body: crate::wit::bindings::capability_types::RouteBody::Bytes(vec![0; len]),
    }
}

#[tuff_core::watchdog]
#[test]
fn route_answers_are_capped_by_count_duplicates_and_bytes() {
    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Running;
    assert!(state.net_respond(1, route_answer(4)).unwrap().is_ok());
    assert!(state.net_respond(1, route_answer(4)).unwrap().is_err());
    for handle in 2..abi::MAX_ROUTE_RESPONSES as u32 + 1 {
        assert!(state.net_respond(handle, route_answer(0)).unwrap().is_ok());
    }
    assert!(
        state
            .net_respond(100_000, route_answer(0))
            .unwrap()
            .is_err()
    );
    assert_eq!(state.effects.responses.len(), abi::MAX_ROUTE_RESPONSES);

    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Running;
    let big = state.limits.message_bytes;
    assert!(
        state
            .net_respond(1, route_answer(big + 1))
            .unwrap()
            .is_err()
    );
    let mut handle = 1;
    while state
        .net_respond(handle, route_answer(big))
        .unwrap()
        .is_ok()
    {
        handle += 1;
    }
    assert!(handle as usize * big <= abi::MAX_RESPONSE_BYTES + big);
    assert!(state.effects.responses.len() < abi::MAX_ROUTE_RESPONSES);
}

#[tuff_core::watchdog]
#[test]
fn per_player_state_needs_an_acting_player_and_keeps_to_its_cap() {
    let mut state = State::new(
        "test",
        Limits {
            state_keys: 2,
            ..Limits::default()
        },
    );
    state.phase = Phase::Running;
    assert!(state.host_state_get("a").unwrap().is_err());
    acting(&mut state, steve());
    for key in ["a", "b"] {
        assert_eq!(state.host_state_set(key, Some(vec![1])), Ok(Ok(())));
    }
    assert!(state.host_state_set("c", Some(vec![1])).unwrap().is_err());
    assert_eq!(state.host_state_set("a", Some(vec![2])), Ok(Ok(())));
    assert_eq!(state.host_state_get("a"), Ok(Ok(Some(vec![2]))));
    assert_eq!(state.host_state_get("c"), Ok(Ok(None)));
    state.host_state_set("a", None).unwrap().unwrap();
    state.host_state_set("b", None).unwrap().unwrap();
    assert!(state.players.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn locations_world_calls_and_messages_without_a_world() {
    let mut state = state();
    let origin = wit::BlockPos { x: 0, y: 0, z: 0 };
    assert_eq!(
        state.host_block_get("overworld", origin),
        Ok(Err(WorldError::UnknownDimension))
    );
    assert_eq!(
        state.host_block_set("overworld", origin, "stone", SetFlags::empty()),
        Ok(Err(WorldError::UnknownDimension))
    );
    assert_eq!(state.host_acting_location(), Ok(None));
    acting(
        &mut state,
        Player {
            position: Some(Position {
                dimension: "minecraft:overworld".into(),
                at: [1.5, 64.0, -2.25],
                look: Some([90.0, -10.0]),
                eye_height: STANDING_EYE_HEIGHT,
            }),
            ..steve()
        },
    );
    assert_eq!(
        state.host_acting_location(),
        Ok(Some(Location {
            dimension: "minecraft:overworld".into(),
            pos: Vec3 {
                x: 1.5,
                y: 64.0,
                z: -2.25
            },
            rotation: Some(Rotation {
                yaw: 90.0,
                pitch: -10.0
            }),
            eye_height: STANDING_EYE_HEIGHT,
        }))
    );
    assert_eq!(
        state.host_acting_player(),
        Ok(Some(wit::Player {
            uuid: Uuid { high: 0, low: 7 },
            name: "Steve".into()
        }))
    );
    assert_eq!(state.host_acting_inventory(), Ok(None));
    state.caller.as_mut().unwrap().items = Some(Items {
        held: Some(("minecraft:stick".into(), 1)),
        slots: vec![(36, "minecraft:stick".into(), 1)],
        stacks: Vec::new(),
    });
    let inventory = state.host_acting_inventory().unwrap().unwrap();
    assert_eq!(inventory.held.unwrap().item, "minecraft:stick");
    assert_eq!(inventory.slots[0].index, 36);
}

#[tuff_core::watchdog]
#[test]
fn messages_tells_and_timers_keep_to_their_limits() {
    let mut state = State::new(
        "test",
        Limits {
            messages: 3,
            schedules: 1,
            ..Limits::default()
        },
    );
    state.phase = Phase::Running;
    assert!(state.host_reply("no one").unwrap().is_err());
    acting(&mut state, steve());
    assert_eq!(state.host_reply("hi"), Ok(Ok(())));
    assert_eq!(state.host_broadcast("all"), Ok(Ok(())));
    assert_eq!(
        state.host_tell(
            Uuid {
                high: 0,
                low: 0xabc
            },
            "psst"
        ),
        Ok(Ok(()))
    );
    assert!(state.host_broadcast("more").unwrap().is_err());
    assert_eq!(state.effects.tells, [(0xabc, "psst".to_owned())]);
    assert_eq!(state.host_schedule(5, 1), Ok(Ok(())));
    assert!(state.host_schedule(5, 2).unwrap().is_err());
    assert!(state.host_schedule(abi::MAX_DELAY + 1, 3).unwrap().is_err());
    state.phase = Phase::Register;
    assert!(state.host_broadcast("early").unwrap().is_err());
}

#[tuff_core::watchdog]
#[test]
fn registry_imports_answer_and_reject_bad_input() {
    let mut state = state();
    assert_eq!(state.host_block_traits("nope"), Ok(None));
    assert_eq!(state.host_block_traits("stone"), Ok(Some(Traits::SOLID)));
    assert_eq!(
        state.host_block_normalize("oak_log[axis=x]"),
        Ok(Some("minecraft:oak_log[axis=x]".into()))
    );
    assert_eq!(state.host_block_normalize("nope"), Ok(None));
    let properties = state.host_block_properties("oak_log").unwrap().unwrap();
    assert_eq!(properties[0].name, "axis");
    assert_eq!(properties[0].values, ["x", "y", "z"]);
    assert_eq!(state.host_block_properties("stone"), Ok(Some(vec![])));
    assert!(
        state
            .host_block_tag("#minecraft:logs")
            .unwrap()
            .unwrap()
            .contains(&"minecraft:oak_log".to_owned())
    );
    assert_eq!(state.host_block_tag("minecraft:no_such_tag"), Ok(None));
    assert!(
        state
            .host_item_tag("#minecraft:logs")
            .unwrap()
            .unwrap()
            .contains(&"minecraft:oak_log".to_owned())
    );
    assert_eq!(state.host_item_tag("no_such_tag"), Ok(None));
    assert_eq!(
        state.host_block_transform("oak_log[axis=x]", Transform::Rotate90),
        Ok(Some("minecraft:oak_log[axis=z]".into()))
    );
    assert_eq!(
        state.host_block_transform("nope", Transform::Rotate90),
        Ok(None)
    );
}

#[tuff_core::watchdog]
#[test]
fn capabilities_are_refused_until_they_exist() {
    let mut state = state();
    assert_eq!(
        state.net_granted(CapabilityKind::Http, "https://example.com"),
        Ok(false)
    );
    assert!(matches!(
        state.files_read("a.txt", 0, 10),
        Ok(Err(Denied::NotGranted(_)))
    ));
    assert_eq!(state.files_usage(), Ok((0, 0)));
}

#[tuff_core::watchdog]
#[test]
fn suggestions_come_from_the_guest_for_flagged_arguments() {
    let mut data = Data::new();
    let node = |name: &str, kind, children: Vec<u16>, suggest| CommandNode {
        name: name.into(),
        kind,
        children,
        executes: Some(0),
        redirect: None,
        permission: None,
        suggest,
    };
    let tree = CommandTree {
        nodes: vec![
            node("pick", NodeKind::Literal, vec![1, 2], false),
            node(
                "word",
                NodeKind::Argument(Parser::Text(TextKind::Word)),
                vec![],
                true,
            ),
            node(
                "plain",
                NodeKind::Argument(Parser::Integer(wit::IntRange {
                    min: None,
                    max: None,
                })),
                vec![],
                false,
            ),
        ],
    };
    let list = vec!["alpha".to_owned(), "beta".to_owned()];
    let answer = data.export_result("suggest", &list.to_value());
    let body = format!(
        "(func {register} {reg} {unit})
         (func {run} {code})
         (func {suggest} (i32.const {answer}))
         {data}",
        run = export("run-command"),
        code = ok_code("(i32.const 0)"),
        register = export("register"),
        reg = data.call("register-command", &[tree.to_value()]),
        unit = ok_unit(),
        suggest = export("suggest"),
        data = data.wat(),
    );
    let mut sandbox = sandbox(&body);
    sandbox.register().unwrap();
    assert_eq!(sandbox.suggest(steve(), "pick al").unwrap(), list);
    assert_eq!(sandbox.suggest(steve(), "pick ").unwrap(), list);
    assert!(sandbox.suggest(steve(), "pick").unwrap().is_empty());
    assert!(sandbox.suggest(steve(), "other al").unwrap().is_empty());
}

#[tuff_core::watchdog]
#[test]
fn a_guest_without_suggest_offers_nothing() {
    let mut data = Data::new();
    let register = register(&mut data, "echo", "");
    let run = format!(
        "(func {} {})",
        export("run-command"),
        ok_code("(i32.const 0)")
    );
    let mut sandbox = sandbox(&format!("{register} {run} {}", data.wat()));
    sandbox.register().unwrap();
    assert!(sandbox.suggest(steve(), "echo hi").unwrap().is_empty());
}

fn pack_zip() -> Vec<u8> {
    let v = tuff_vanilla_data::version::RESOURCE_PACK_VERSION;
    let meta = format!(
        r#"{{"pack":{{"description":"test","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
        v.major, v.minor
    );
    tuff_zip::write_all(
        &[("pack.mcmeta", meta.as_bytes())],
        tuff_zip::DosTime::MIN,
        6,
        u64::MAX,
    )
    .unwrap()
}

#[tuff_core::watchdog]
#[test]
fn settings_and_packs_are_declared_while_registering_and_checked() {
    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Register;
    let decl = |key: &str, default| ConfigDecl {
        key: key.into(),
        default,
        description: "about it".into(),
        secret: false,
        text_component: false,
    };
    assert_eq!(
        state.host_declare_config(decl("speed", ConfigValue::Integer(3))),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_declare_config(ConfigDecl {
            secret: true,
            text_component: true,
            ..decl("relay.token", ConfigValue::Text(String::new()))
        }),
        Ok(Ok(()))
    );
    for bad in ["speed", "", "a..b", "with space"] {
        assert!(
            matches!(
                state.host_declare_config(decl(bad, ConfigValue::Boolean(true))),
                Ok(Err(RegisterError::Invalid(_)))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        state.host_declare_config(ConfigDecl {
            description: "x".repeat(abi::MAX_SETTING_DESCRIPTION + 1),
            ..decl("long", ConfigValue::Boolean(true))
        }),
        Ok(Err(RegisterError::Invalid(_)))
    ));
    let keys = state.registration.settings[1].to_key("test");
    assert_eq!(keys.name, "plugins.test.relay.token");
    assert!(keys.secret);
    assert!(keys.text_component);
    assert_eq!(state.host_config_get("speed"), Ok(None));
    assert_eq!(
        state.host_config_set("speed", ConfigValue::Integer(4)),
        Ok(Err(HostError::WrongPhase))
    );

    let pack = |name: &str, data: Vec<u8>| ResourcePack {
        name: name.into(),
        data,
        required: false,
        prompt: Some("please".into()),
    };
    assert_eq!(
        state.host_register_resource_pack(pack("look", pack_zip())),
        Ok(Ok(()))
    );
    for (name, data) in [
        ("look", pack_zip()),
        ("Bad Name", pack_zip()),
        ("junk", b"not a zip".to_vec()),
        (
            "empty",
            tuff_zip::write_all(
                &[("readme.txt", b"hi".as_slice())],
                tuff_zip::DosTime::MIN,
                6,
                u64::MAX,
            )
            .unwrap(),
        ),
    ] {
        assert!(
            matches!(
                state.host_register_resource_pack(pack(name, data)),
                Ok(Err(RegisterError::Invalid(_)))
            ),
            "{name}"
        );
    }
    for n in 1..abi::MAX_RESOURCE_PACKS {
        assert_eq!(
            state.host_register_resource_pack(pack(&format!("p{n}"), pack_zip())),
            Ok(Ok(()))
        );
    }
    assert_eq!(
        state.host_register_resource_pack(pack("over", pack_zip())),
        Ok(Err(RegisterError::LimitReached(
            abi::MAX_RESOURCE_PACKS as u32
        )))
    );
    let ids: std::collections::HashSet<u128> = state
        .registration
        .packs
        .iter()
        .map(|p| p.id("test"))
        .collect();
    assert_eq!(ids.len(), abi::MAX_RESOURCE_PACKS);
    assert_ne!(
        state.registration.packs[0].id("test"),
        state.registration.packs[0].id("other")
    );

    state.phase = Phase::Running;
    assert_eq!(
        state.host_declare_config(decl("late", ConfigValue::Boolean(true))),
        Ok(Err(RegisterError::WrongPhase))
    );
    assert_eq!(
        state.host_register_resource_pack(pack("late", pack_zip())),
        Ok(Err(RegisterError::WrongPhase))
    );
    assert_eq!(state.host_config_get("speed"), Ok(None));
    assert!(matches!(
        state.host_config_set("nope", ConfigValue::Integer(1)),
        Ok(Err(_))
    ));
    assert!(matches!(
        state.host_config_set("speed", ConfigValue::Text("fast".into())),
        Ok(Err(_))
    ));
    assert_eq!(
        state
            .host_item_info("minecraft:shield")
            .unwrap()
            .map(|i| i.max_stack),
        Some(1)
    );
}

#[tuff_core::watchdog]
#[test]
fn a_login_listener_refuses_only_while_deciding_a_login() {
    let mut data = Data::new();
    let subscribe = data.call("subscribe", &[EventKinds::LOGIN.to_value()]);
    let refuse = data.call("refuse-login", &[Value::String("Banned by plugin".into())]);
    let body = format!(
        "(func {register} {subscribe} {ok}) (func {event} {refuse}) {data}",
        register = export("register"),
        ok = ok_unit(),
        event = export("on-event"),
        data = data.wat(),
    );
    let mut sandbox = sandbox(&body);
    assert_eq!(sandbox.register().unwrap().events, EventKinds::LOGIN);
    let attempt = Event::Login(wit::LoginAttempt {
        player: steve().to_wit(),
        address: "127.0.0.1".into(),
    });
    let effects = sandbox.event(&attempt, Some(steve())).unwrap();
    assert_eq!(effects.login_refusal.as_deref(), Some("Banned by plugin"));
}

#[tuff_core::watchdog]
#[test]
fn advancement_ops_stay_in_granted_content_namespaces() {
    use crate::caps::{Grants, Policy, Request};
    let policy = |grants: &str| {
        let mut requests = Request::parse_line("cap.content", "mine").unwrap().unwrap();
        requests.extend(
            Request::parse_line("cap.player-write", "all")
                .unwrap()
                .unwrap(),
        );
        Policy::new(requests, Grants::parse(grants).unwrap().get("p"))
    };
    let player = Uuid { high: 0, low: 7 };
    let mut state = state();
    assert_eq!(
        state.host_advancement_grant(player, "mine:root", None),
        Ok(Err(WorldError::NotGranted))
    );
    let (_tx, rx) =
        tokio::sync::watch::channel(Arc::new(policy("[p]\ncontent = mine\nplayer-write = all")));
    state.policy = Some(rx);
    assert_eq!(
        state.host_advancement_grant(player, "mine:story/root", Some("go".into())),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_advancement_revoke(player, "mine:story/root", None),
        Ok(Ok(()))
    );
    for denied in ["other:root", "minecraft:story/root", "root"] {
        assert_eq!(
            state.host_advancement_grant(player, denied, None),
            Ok(Err(WorldError::NotGranted)),
            "{denied}"
        );
    }
    assert_eq!(
        state.host_advancement_grant(player, "Mine:Bad Name", None),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.effects.advancements,
        [
            AdvancementOp {
                player: 7,
                grant: true,
                advancement: "mine:story/root".into(),
                criterion: Some("go".into()),
            },
            AdvancementOp {
                player: 7,
                grant: false,
                advancement: "mine:story/root".into(),
                criterion: None,
            },
        ]
    );
}

fn team(color: Option<&str>, members: Vec<String>) -> TeamSpec {
    TeamSpec {
        color: color.map(Into::into),
        prefix: String::new(),
        suffix: String::new(),
        members,
    }
}

#[tuff_core::watchdog]
#[test]
fn display_calls_queue_and_validate() {
    let player = Uuid { high: 0, low: 9 };
    let mut state = state();
    assert_eq!(
        state.host_show_title(
            player,
            TitleSpec {
                title: "T".into(),
                subtitle: "S".into(),
                fade_in: 5,
                stay: 40,
                fade_out: 5,
            }
        ),
        Ok(Ok(()))
    );
    assert_eq!(state.host_action_bar(player, "bar"), Ok(Ok(())));
    assert_eq!(state.host_sidebar_set("t", vec!["a".into()]), Ok(Ok(())));
    assert!(
        state
            .host_sidebar_set("t", vec![String::new(); 16])
            .unwrap()
            .is_err()
    );
    assert!(
        state
            .host_team_set("red", team(Some("nope"), vec![]))
            .unwrap()
            .is_err()
    );
    assert_eq!(
        state.host_team_set("red", team(Some("red"), vec!["Alex".into()])),
        Ok(Ok(()))
    );
    assert_eq!(state.effects.displays.len(), 4);
    let mut registering = State::new("test", Limits::default());
    registering.phase = Phase::Register;
    assert!(registering.host_action_bar(player, "x").unwrap().is_err());
}

#[tuff_core::watchdog]
#[test]
fn inventory_writes_queue_by_uuid_and_validate() {
    let mut state = state();
    let who = Uuid { high: 0, low: 9 };
    assert_eq!(
        state.host_take_items(who, "minecraft:gold_ingot", 4, true),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_take_items(who, "minecraft:nope", 4, true),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(state.host_clear_inventory(who), Ok(Ok(())));
    assert_eq!(state.host_set_keep_inventory(who, Some(true)), Ok(Ok(())));
    assert_eq!(
        state.host_set_slot(who, 2, None),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_set_slot(
            who,
            5,
            Some("{id:\"minecraft:diamond_helmet\",count:1}".into())
        ),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_set_slot(who, 5, Some("{bad".into())),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_give_item_to(who, "minecraft:stick", 2, None, None),
        Ok(Ok(()))
    );
    assert_eq!(state.effects.inventory_ops.len(), 5);
    assert_eq!(state.effects.inventory_ops[4].0, 9);
    assert!(matches!(
        state.effects.inventory_ops[4].1,
        InventoryOp::Give { count: 2, .. }
    ));
}

#[tuff_core::watchdog]
#[test]
fn stashes_move_items_by_handle_and_queue_their_hand_over() {
    let mut state = state();
    let who = Uuid { high: 0, low: 9 };
    let vault = state.host_stash_new().unwrap().unwrap();
    assert_eq!(
        state.host_stash_mint(vault, "minecraft:stone", 70, None, None),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_stash_mint(vault + 99, "minecraft:stone", 1, None, None),
        Ok(Err(WorldError::InvalidState))
    );
    let shown = state.host_stash_contents(vault).unwrap().unwrap();
    assert_eq!(
        shown
            .iter()
            .map(|s| (s.item.as_str(), s.count))
            .collect::<Vec<_>>(),
        vec![("minecraft:stone", 64), ("minecraft:stone", 6)]
    );
    let part = state
        .host_stash_split(vault, Some("minecraft:stone".into()), 10)
        .unwrap()
        .unwrap();
    let count = |state: &mut State, stash| -> u32 {
        state
            .host_stash_contents(stash)
            .unwrap()
            .unwrap_or_default()
            .iter()
            .map(|s| u32::from(s.count))
            .sum()
    };
    assert_eq!(
        (count(&mut state, vault), count(&mut state, part)),
        (60, 10)
    );
    assert_eq!(state.host_stash_merge(vault, part), Ok(Ok(())));
    assert_eq!(
        state.host_stash_contents(part),
        Ok(None),
        "a merged stash is gone"
    );
    assert_eq!(
        count(&mut state, vault),
        70,
        "nothing was made or lost moving"
    );
    assert_eq!(
        state.host_take_into_stash(who, "minecraft:dirt", 3, false, vault),
        Ok(Ok(()))
    );
    assert_eq!(state.host_give_stash(who, vault), Ok(Ok(())));
    assert_eq!(
        state.host_stash_burn(vault),
        Ok(Err(WorldError::InvalidState))
    );
    let ops = std::mem::take(&mut state.effects.inventory_ops);
    assert!(matches!(
        &ops[0].1,
        InventoryOp::TakeInto { count: 3, sink, .. } if sink.stash == vault
    ));
    let InventoryOp::GiveStacks(given) = &ops[1].1 else {
        panic!("{:?}", ops[1]);
    };
    assert_eq!(given.iter().map(|s| u32::from(s.count())).sum::<u32>(), 70);
    let burnt = state.host_stash_new().unwrap().unwrap();
    assert_eq!(state.host_stash_burn(burnt), Ok(Ok(())));
}

#[tuff_core::watchdog]
#[test]
fn player_state_ops_queue_validated_changes_by_uuid() {
    use crate::player_state::PlayerOp;
    use tuff_core::protocol::play::GameMode;
    let player = Uuid { high: 0, low: 9 };
    let mut state = state();
    assert_eq!(
        state.host_set_game_mode(player, crate::wit::bindings::types::GameMode::Spectator),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_set_vitals(player, Some(0.0), None),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_set_vitals(player, Some(20.0), Some(18)),
        Ok(Ok(()))
    );
    assert_eq!(
        state.host_set_xp(player, -1, 0),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_add_effect(player, "minecraft:nope", 100, 0, true),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_add_effect(player, "speed", 0, 0, true),
        Ok(Err(WorldError::InvalidState))
    );
    assert_eq!(
        state.host_add_effect(player, "speed", 100, 1, false),
        Ok(Ok(()))
    );
    assert_eq!(state.host_clear_effects(player, None), Ok(Ok(())));
    assert_eq!(state.host_respawn_player(player), Ok(Ok(())));
    assert_eq!(state.host_player_info(player), Ok(None));
    let uuid = uuid_of(player);
    assert_eq!(state.effects.player_ops.len(), 5);
    assert_eq!(
        state.effects.player_ops[0],
        (uuid, PlayerOp::GameMode(GameMode::Spectator))
    );
    assert_eq!(
        state.effects.player_ops[1],
        (
            uuid,
            PlayerOp::Vitals {
                health: Some(20.0),
                food: Some(18)
            }
        )
    );
}

#[tuff_core::watchdog]
#[test]
fn every_host_call_is_classified_gated_or_open() {
    use crate::caps::classes::HOST_CALL_GROUPS;
    let mut listed: Vec<&str> = HOST_CALL_GROUPS.iter().map(|(name, _)| *name).collect();
    let mut imported: Vec<&str> = crate::wit::bindings::IMPORTS
        .iter()
        .map(|(_, name)| *name)
        .collect();
    listed.sort_unstable();
    imported.sort_unstable();
    assert_eq!(listed, imported);
    let gated = |kind| {
        HOST_CALL_GROUPS
            .iter()
            .filter(|(_, group)| *group == Some(kind))
            .count()
    };
    assert_eq!(gated(crate::caps::Kind::WorldWrite), 12);
    assert_eq!(gated(crate::caps::Kind::CrossPlugin), 2);
    assert_eq!(gated(crate::caps::Kind::Panel), 3);
}

#[tuff_core::watchdog]
#[test]
fn panel_registration_needs_the_panel_grant() {
    let card = || wit::PanelCard {
        series: "test.count".into(),
        title: "Count".into(),
        unit: wit::PanelUnit::Count,
        scope: None,
    };
    let mut state = closed_state();
    state.phase = Phase::Register;
    grant_groups(&mut state, &["world-write"]);
    assert_eq!(
        state.host_register_panel_card(card()),
        Ok(Err(RegisterError::Invalid(
            "cap.panel is not granted".into()
        )))
    );
    grant_groups(&mut state, &["panel"]);
    assert_eq!(state.host_register_panel_card(card()), Ok(Ok(())));
}

#[tuff_core::watchdog]
#[test]
fn ungranted_groups_close_their_host_calls() {
    let player = Uuid { high: 0, low: 7 };
    let origin = wit::BlockPos { x: 0, y: 0, z: 0 };
    for groups in [
        &[][..],
        &["player-write", "player-read", "cross-plugin"][..],
    ] {
        let mut state = closed_state();
        grant_groups(&mut state, groups);
        assert_eq!(
            state.host_block_set("overworld", origin, "stone", SetFlags::empty()),
            Ok(Err(WorldError::NotGranted))
        );
        assert_eq!(
            state.host_spawn_entity(
                "overworld",
                "pig",
                Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                },
                None
            ),
            Ok(Err(WorldError::NotGranted))
        );
        assert_eq!(
            state.host_spawn_custom_entity(
                "overworld",
                "ex:golem",
                Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0
                },
                0.0
            ),
            Ok(Err(WorldError::NotGranted))
        );
    }
    let mut state = closed_state();
    grant_groups(&mut state, &["world-write"]);
    assert_eq!(
        state.host_spawn_custom_entity(
            "overworld",
            "ex:golem",
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0
            },
            0.0
        ),
        Ok(Err(WorldError::UnknownDimension))
    );
    assert_eq!(
        state.host_kill_player(player),
        Ok(Err(WorldError::NotGranted))
    );
    assert_eq!(
        state.host_set_vitals(player, Some(1.0), None),
        Ok(Err(WorldError::NotGranted))
    );
    assert_eq!(state.host_team_remove("t"), Ok(Err(HostError::NotGranted)));
    assert_eq!(state.host_online_players(), Ok(Vec::new()));
    assert_eq!(state.host_player_info(player), Ok(None));
    assert_eq!(state.host_acting_inventory(), Ok(None));
    assert!(matches!(
        state.host_notify(None, "t:i", 1, b""),
        Ok(Err(CallError::Refused(_)))
    ));
    assert!(matches!(
        state.host_request(None, "t:i", 1, b"", 0),
        Ok(Err(CallError::Refused(_)))
    ));
    grant_groups(&mut state, &["player-write"]);
    assert_eq!(state.host_kill_player(player), Ok(Ok(())));
}

fn span(text: &str) -> RichSpan {
    RichSpan {
        text: text.to_owned(),
        color: None,
        bold: false,
        italic: false,
        underlined: false,
        click: None,
        hover: None,
        insertion: None,
    }
}

#[tuff_core::watchdog]
#[test]
fn rich_tells_carry_events_and_refuse_bad_input() {
    use tuff_item::text::{ClickEvent, Hover};
    let player = Uuid { high: 0, low: 7 };
    let mut state = State::new("test", Limits::default());
    state.phase = Phase::Running;
    let mut link = span("here");
    link.color = Some("aqua".to_owned());
    link.click = Some(RichClick::RunCommand("/spawn".to_owned()));
    link.hover = Some("Go".to_owned());
    assert_eq!(
        state.host_tell_rich(player, vec![span("Click "), link]),
        Ok(Ok(()))
    );
    let (uuid, message) = &state.effects.rich_tells[0];
    assert_eq!(*uuid, 7);
    assert_eq!(message.plain(), "Click here");
    let events = message.extra[1].events().unwrap();
    assert_eq!(
        events.click,
        Some(ClickEvent::RunCommand("/spawn".to_owned()))
    );
    assert!(matches!(events.hover, Some(Hover::Text(_))));

    let refused = |state: &mut State, edit: &dyn Fn(&mut RichSpan)| {
        let mut bad = span("x");
        edit(&mut bad);
        state.host_tell_rich(player, vec![bad]).unwrap().is_err()
    };
    assert!(refused(&mut state, &|s| s.text = "a".repeat(257)));
    assert!(refused(&mut state, &|s| s.hover = Some("a".repeat(257))));
    assert!(refused(&mut state, &|s| s.color = Some("not a color".to_owned())));
    assert!(refused(&mut state, &|s| s.click =
        Some(RichClick::RunCommand("op me".to_owned()))));
    assert!(refused(&mut state, &|s| s.click =
        Some(RichClick::OpenUrl("file:///etc".to_owned()))));
    assert!(
        state
            .host_tell_rich(player, vec![span("x"); 33])
            .unwrap()
            .is_err()
    );
    assert_eq!(state.effects.rich_tells.len(), 1);
}
