// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::rc::Rc;

use crate::bindings::tuff::plugin::capability_types as cap;
use crate::exec::{dispatch as io, spawn};
use crate::iface_fixture::control::{self, Color, Control, Perms, Point, Shape};
use crate::messaging::stub::STUB;
use crate::messaging::{CallError, Incoming};
use crate::wire::{Error, decode_all, encode_to_vec};

fn incoming(method: u64, request: u64, payload: Vec<u8>) -> Incoming {
    Incoming {
        sender: "caller".into(),
        interface: control::INTERFACE.into(),
        method,
        payload,
        request: Some(request),
    }
}

fn responses() -> Vec<(u64, Result<Vec<u8>, String>)> {
    STUB.with(|s| std::mem::take(&mut s.borrow_mut().responses))
}

struct Impl {
    seen: Rc<RefCell<Vec<String>>>,
}

impl control::Serve for Impl {
    fn add(&mut self, from: &str, a: i32, b: i32) -> i32 {
        self.seen.borrow_mut().push(from.into());
        a + b
    }
    fn find(&mut self, _: &str, name: String) -> Result<Point, String> {
        if name == "origin" {
            Ok(Point { x: 0, y: 0 })
        } else {
            Err(format!("no {name}"))
        }
    }
    fn paint(&mut self, _: &str, c: Color, p: Perms, s: Shape) {
        self.seen.borrow_mut().push(format!("{c:?} {p:?} {s:?}"));
    }
    fn ping(&mut self, _: &str) {}
    fn shapes(&mut self, _: &str, n: u32) -> Vec<Shape> {
        vec![Shape::Dot; n as usize]
    }
    fn echo(&mut self, _: &str, v: control::Every) -> control::Every {
        v
    }
    fn look(&mut self, _: &str, i: Vec<u64>) -> Result<u32, String> {
        Ok(i.len() as u32)
    }
    fn pick(&mut self, _: &str, n: control::Named) -> Option<(u8, String)> {
        Some((n.tags.len() as u8, n.name))
    }
}

#[tuff_core::watchdog]
#[test]
fn served_methods_decode_run_and_answer() {
    responses();
    let seen = Rc::new(RefCell::new(Vec::new()));
    control::serve(Impl { seen: seen.clone() }).unwrap();
    STUB.with(|s| {
        let provided = &s.borrow().provided;
        assert_eq!(provided.last().unwrap().0, control::INTERFACE);
        assert_eq!(provided.last().unwrap().1.len(), 8);
    });

    let args = encode_to_vec(&(2i32, 3i32));
    assert!(crate::messaging::dispatch(incoming(control::ADD_KEY, 1, args)).is_none());
    assert_eq!(responses(), vec![(1, Ok(encode_to_vec(&5i32)))]);
    assert_eq!(seen.borrow()[0], "caller");

    let args = encode_to_vec(&("nowhere".to_string(),));
    crate::messaging::dispatch(incoming(control::FIND_KEY, 2, args));
    let bytes = responses().remove(0).1.unwrap();
    assert_eq!(
        decode_all::<Result<Point, String>>(&bytes).unwrap(),
        Err("no nowhere".to_string())
    );

    crate::messaging::dispatch(incoming(control::PING_KEY, 3, vec![]));
    assert_eq!(responses(), vec![(3, Ok(vec![]))]);
}

#[tuff_core::watchdog]
#[test]
fn malformed_payloads_are_refused() {
    responses();
    control::serve(Impl {
        seen: Rc::default(),
    })
    .unwrap();
    crate::messaging::dispatch(incoming(control::ADD_KEY, 1, vec![1, 2]));
    crate::messaging::dispatch(incoming(control::ADD_KEY, 2, vec![0; 9]));
    let out = responses();
    assert!(out[0].1.as_ref().unwrap_err().starts_with("bad payload"));
    assert!(out[1].1.as_ref().unwrap_err().starts_with("bad payload"));
}

fn run_client<T: 'static>(
    reply: Vec<u8>,
    call: impl std::future::Future<Output = T> + 'static,
) -> Rc<RefCell<Option<T>>> {
    let out = Rc::new(RefCell::new(None));
    let slot = out.clone();
    STUB.with(|s| s.borrow_mut().next_request = Some(Ok(9100)));
    spawn(async move {
        *slot.borrow_mut() = Some(call.await);
    });
    assert!(out.borrow().is_none());
    let event = cap::IoEvent::Reply(cap::ReplyDone {
        handle: 9100,
        outcome: Ok(reply),
    });
    assert!(io(event).is_none());
    out
}

#[tuff_core::watchdog]
#[test]
fn clients_decode_plain_and_app_error_results() {
    let out = run_client(encode_to_vec(&7i32), async {
        Control::any().add(3, 4).await
    });
    assert_eq!(out.borrow_mut().take().unwrap(), Ok(7));

    let err: Result<Point, String> = Err("nope".into());
    let out = run_client(encode_to_vec(&err), async {
        Control::at("owner").find("x".into()).await
    });
    assert_eq!(
        out.borrow_mut().take().unwrap(),
        Err(Error::App("nope".to_string()))
    );

    let ok: Result<Point, String> = Ok(Point { x: 1, y: 2 });
    let out = run_client(encode_to_vec(&ok), async {
        Control::any().find("x".into()).await
    });
    assert_eq!(out.borrow_mut().take().unwrap(), Ok(Point { x: 1, y: 2 }));
}

#[tuff_core::watchdog]
#[test]
fn clients_report_call_and_decode_failures() {
    let out = run_client(vec![1, 2, 3], async { Control::any().add(1, 1).await });
    assert!(matches!(
        out.borrow_mut().take().unwrap(),
        Err(CallError::Refused(_))
    ));

    STUB.with(|s| s.borrow_mut().next_request = Some(Err(CallError::NoProvider)));
    let out = Rc::new(RefCell::new(None));
    let slot = out.clone();
    spawn(async move {
        *slot.borrow_mut() = Some(Control::any().find("x".into()).await);
    });
    assert_eq!(
        out.borrow_mut().take().unwrap(),
        Err(Error::Call(CallError::NoProvider))
    );
}

#[tuff_core::watchdog]
#[test]
fn notify_variants_exist_only_for_resultless_methods() {
    assert_eq!(
        Control::any().notify_paint(Color::Red, Perms::READ | Perms::WRITE, Shape::Dot),
        Ok(1)
    );
    assert_eq!(Control::at("p").notify_ping(), Ok(1));
}

#[tuff_core::watchdog]
#[test]
fn flags_and_enums_reject_bad_bits_and_cases() {
    assert!(decode_all::<Perms>(&8u64.to_le_bytes()).is_err());
    assert_eq!(
        decode_all::<Perms>(&3u64.to_le_bytes()).unwrap(),
        Perms::READ | Perms::WRITE
    );
    assert!(decode_all::<Color>(&[3]).is_err());
    assert_eq!(decode_all::<Color>(&[2]).unwrap(), Color::Blue);
    assert!(decode_all::<Shape>(&[9]).is_err());
}

/// The values the C++ guest `cpp-iface` encodes, as `name hex` lines; the
/// checked-in golden file is compared by both sides.
pub(crate) fn golden_lines() -> Vec<String> {
    use crate::iface_fixture::control::{Every, Ids, Named, Outcome};
    fn line<T: crate::wire::Wire>(name: &str, value: &T) -> String {
        let hex: String = encode_to_vec(value)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("{name} {hex}")
    }
    let shapes = vec![
        Shape::Dot,
        Shape::Circle(1.5),
        Shape::Poly(vec![Point { x: 1, y: 2 }, Point { x: 3, y: 4 }]),
        Shape::Pair((7, "q".into())),
    ];
    let named = Named {
        name: "ab".into(),
        tags: vec!["x".into(), "yz".into()],
        at: Some(Point { x: -1, y: 2 }),
    };
    let every = Every {
        a: 0xab,
        b: 60000,
        c: 0xdead_beef,
        d: 0x0102_0304_0506_0708,
        e: -5,
        f: -3,
        g: -9,
        h: i64::MIN,
        i: 1.5,
        j: -2.25,
        k: true,
        l: '\u{1f600}',
        m: "h\u{e9}llo".into(),
    };
    let ids: Ids = vec![1, u64::MAX];
    let err: Outcome = Err("no".into());
    let ok: Outcome = Ok(9);
    let found: Result<Point, String> = Err("nowhere".into());
    vec![
        line("point", &Point { x: 3, y: -4 }),
        line("named", &named),
        line("shapes", &shapes),
        line("color", &Color::Blue),
        line("perms", &(Perms::READ | Perms::ADMIN)),
        line("ids", &ids),
        line("outcome-err", &err),
        line("outcome-ok", &ok),
        line("every", &every),
        line("pick-some", &Some((9u8, "z".to_string()))),
        line("pick-none", &None::<(u8, String)>),
        line("find-err", &found),
    ]
}

#[tuff_core::watchdog]
#[test]
fn shared_fixture_encodes_to_the_golden_bytes() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("c/fixtures/iface_demo.golden");
    let mut expected = golden_lines().join("\n");
    expected.push('\n');
    if std::env::var("TUFF_UPDATE_IFACE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&path, &expected).unwrap();
    }
    let golden = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        golden, expected,
        "run with TUFF_UPDATE_IFACE_GOLDEN=1 to refresh"
    );
}
