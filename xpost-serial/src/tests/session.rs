//! End-to-end App-mode branches over injectable memory IO.

use crate::client::{Command, OkReply};
use crate::r#type::reply::{MapError, PerfError, ServerError, SyntaxError};
use crate::server::reply::ReplyWriter;
use crate::tests::harness::{TestError, with_session};

#[test]
fn ping_ok() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.ok(OkReply::Ping)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::Ping)
                .expect("ping ok");
            assert!(matches!(ok, OkReply::Ping));
        },
    );
}

#[test]
fn enable_xdp_ok() {
    with_session(
        |b| b.on_enable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::EnableXdp)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::EnableXdp)
                .expect("enable xdp ok");
            assert!(matches!(ok, OkReply::EnableXdp));
        },
    );
}

#[test]
fn disable_xdp_ok() {
    with_session(
        |b| b.on_disable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::DisableXdp)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::DisableXdp)
                .expect("disable xdp ok");
            assert!(matches!(ok, OkReply::DisableXdp));
        },
    );
}

#[test]
fn enable_exec_ok() {
    with_session(
        |b| b.on_enable_exec(|reply: ReplyWriter<'_>| reply.ok(OkReply::EnableExec)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::EnableExec)
                .expect("enable exec ok");
            assert!(matches!(ok, OkReply::EnableExec));
        },
    );
}

#[test]
fn disable_exec_ok() {
    with_session(
        |b| b.on_disable_exec(|reply: ReplyWriter<'_>| reply.ok(OkReply::DisableExec)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::DisableExec)
                .expect("disable exec ok");
            assert!(matches!(ok, OkReply::DisableExec));
        },
    );
}

#[test]
fn clear_map_ok() {
    with_session(
        |b| {
            b.on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
                assert_eq!(name, "counter");
                reply.ok(OkReply::ClearMap);
            })
            .build()
        },
        |client| {
            let ok = client
                .request::<TestError>(&Command::ClearMap { name: "counter" })
                .expect("clear map ok");
            assert!(matches!(ok, OkReply::ClearMap));
        },
    );
}

#[test]
fn update_map_ok() {
    with_session(
        |b| {
            b.on_update_map(|name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                assert_eq!(name, "counter");
                assert_eq!(key, b"0");
                assert_eq!(value, b"1");
                reply.ok(OkReply::UpdateMap);
            })
            .build()
        },
        |client| {
            let ok = client
                .request::<TestError>(&Command::UpdateMap {
                    name: "counter",
                    key: b"0",
                    value: b"1",
                })
                .expect("update map ok");
            assert!(matches!(ok, OkReply::UpdateMap));
        },
    );
}

#[test]
fn clear_perf_ok() {
    with_session(
        |b| b.on_clear_perf(|reply: ReplyWriter<'_>| reply.ok(OkReply::ClearPerf)).build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::ClearPerf)
                .expect("clear perf ok");
            assert!(matches!(ok, OkReply::ClearPerf));
        },
    );
}

#[test]
fn get_map_blob_ok() {
    const BLOB: &[u8] = b"\x01\x02\x03\xff";
    with_session(
        |b| {
            b.on_get_map(|name: &str, reply: ReplyWriter<'_>| {
                assert_eq!(name, "counter");
                reply.get_map(BLOB.len(), |write| {
                    write(BLOB).unwrap();
                });
            })
            .build()
        },
        |client| {
            let ok = client
                .request::<TestError>(&Command::GetMap { name: "counter" })
                .expect("get map ok");
            match ok {
                OkReply::GetMap { blob } => assert_eq!(blob, BLOB),
                _ => panic!("expected GetMap"),
            }
        },
    );
}

#[test]
fn get_perf_blob_ok() {
    const BLOB: &[u8] = b"perf-bytes";
    with_session(
        |b| {
            b.on_get_perf(|reply: ReplyWriter<'_>| {
                reply.get_perf(BLOB.len(), |write| {
                    write(BLOB).unwrap();
                });
            })
            .build()
        },
        |client| {
            let ok = client
                .request::<TestError>(&Command::GetPerf)
                .expect("get perf ok");
            match ok {
                OkReply::GetPerf { blob } => assert_eq!(blob, BLOB),
                _ => panic!("expected GetPerf"),
            }
        },
    );
}

#[test]
fn get_init_ok() {
    // No on_*: unsolicited `OK init` comes from server start().
    with_session(
        |b| b.build(),
        |client| {
            let ok = client
                .request::<TestError>(&Command::GetInit)
                .expect("get init ok");
            assert!(matches!(ok, OkReply::Init));
        },
    );
}

#[test]
fn get_finish_ok_from_uart_injection() {
    // `OK finish` is not emitted by SerialHandler::start; simulate eBPF printk
    // by writing the line on the memory UART duplex.
    use std::thread;
    use std::time::Duration;

    use crate::client::{MemoryPair, SerialHandler};
    use crate::server::SerialHandler as ServerHandler;
    use crate::tests::harness::TestError;

    let pair = MemoryPair::new();
    let inject = pair.server.clone();
    let client = SerialHandler::open_memory(pair.client);
    let read_end = pair.server.clone();
    let write_end = pair.server;
    let handler = ServerHandler::builder(
        move || read_end.read_byte(),
        move |bytes: &[u8]| write_end.write_all(bytes),
        [0u8; 128],
    )
    .build();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_t = std::sync::Arc::clone(&stop);
    let join = thread::spawn(move || {
        let mut handler = handler;
        handler.start();
        while !stop_t.load(std::sync::atomic::Ordering::Acquire) {
            if !handler.pump() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    thread::sleep(Duration::from_millis(20));
    let _ = inject.write_all(b"OK finish\n");
    thread::sleep(Duration::from_millis(20));
    let ok = client
        .request::<TestError>(&Command::GetFinish)
        .expect("get finish ok");
    assert!(matches!(ok, OkReply::Finish));
    stop.store(true, std::sync::atomic::Ordering::Release);
    client.shutdown();
    let _ = join.join();
}

#[test]
fn unregistered_is_unsupported() {
    with_session(
        |b| b.build(),
        |client| {
            match client.request::<TestError>(&Command::EnableXdp) {
                Err(TestError::Server(ServerError::Unsupported)) => {}
                Err(e) => panic!("expected Unsupported, got {e:?}"),
                Ok(_) => panic!("expected Unsupported, got Ok"),
            }
        },
    );
}

#[test]
fn err_unknown() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unknown)).build(),
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Server(ServerError::Unknown)) => {}
                Err(e) => panic!("expected Unknown, got {e:?}"),
                Ok(_) => panic!("expected Unknown, got Ok"),
            }
        },
    );
}

#[test]
fn err_timeout() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.error(&ServerError::Timeout)).build(),
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Server(ServerError::Timeout)) => {}
                Err(e) => panic!("expected Timeout, got {e:?}"),
                Ok(_) => panic!("expected Timeout, got Ok"),
            }
        },
    );
}

#[test]
fn err_logic() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.error(&ServerError::Logic)).build(),
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Server(ServerError::Logic)) => {}
                Err(e) => panic!("expected Logic, got {e:?}"),
                Ok(_) => panic!("expected Logic, got Ok"),
            }
        },
    );
}

#[test]
fn err_busy() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.error(&ServerError::Busy)).build(),
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Server(ServerError::Busy)) => {}
                Err(e) => panic!("expected Busy, got {e:?}"),
                Ok(_) => panic!("expected Busy, got Ok"),
            }
        },
    );
}

#[test]
fn err_unsupported() {
    with_session(
        |b| b.on_ping(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unsupported)).build(),
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Server(ServerError::Unsupported)) => {}
                Err(e) => panic!("expected Unsupported, got {e:?}"),
                Ok(_) => panic!("expected Unsupported, got Ok"),
            }
        },
    );
}

#[test]
fn err_map_name_not_found() {
    with_session(
        |b| {
            b.on_get_map(|_: &str, reply: ReplyWriter<'_>| {
                reply.error(&MapError::NameNotFound);
            })
            .build()
        },
        |client| {
            match client.request::<TestError>(&Command::GetMap { name: "missing" }) {
                Err(TestError::Map(MapError::NameNotFound)) => {}
                Err(e) => panic!("expected Map::NameNotFound, got {e:?}"),
                Ok(_) => panic!("expected Map::NameNotFound, got Ok"),
            }
        },
    );
}

#[test]
fn err_perf_unavailable() {
    with_session(
        |b| {
            b.on_get_perf(|reply: ReplyWriter<'_>| reply.error(&PerfError::Unavailable))
                .build()
        },
        |client| {
            match client.request::<TestError>(&Command::GetPerf) {
                Err(TestError::Perf(PerfError::Unavailable)) => {}
                Err(e) => panic!("expected Perf::Unavailable, got {e:?}"),
                Ok(_) => panic!("expected Perf::Unavailable, got Ok"),
            }
        },
    );
}

#[test]
fn err_syntax_bad_arity() {
    with_session(
        |b| {
            b.on_ping(|reply: ReplyWriter<'_>| reply.error(&SyntaxError::BadArity))
                .build()
        },
        |client| {
            match client.request::<TestError>(&Command::Ping) {
                Err(TestError::Syntax(SyntaxError::BadArity)) => {}
                Err(e) => panic!("expected Syntax::BadArity, got {e:?}"),
                Ok(_) => panic!("expected Syntax::BadArity, got Ok"),
            }
        },
    );
}
