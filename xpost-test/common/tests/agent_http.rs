//! Host-only HTTP fixtures for the TR3000 client. No device or database.

#[allow(dead_code)]
#[path = "../src/agent.rs"]
mod agent;

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use agent::AgentClient;

async fn record(listener: TcpListener, cases: Vec<(&str, Option<Value>, Value)>) {
    for (expected_line, expected_body, reply) in cases {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim_end(), expected_line);
        let mut content_length = 0;
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap();
                }
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).await.unwrap();
        if let Some(expected_body) = expected_body {
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                expected_body
            );
        } else {
            assert!(body.is_empty());
        }
        let data = serde_json::to_string(&reply).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",
            data.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }
}

#[tokio::test]
async fn tr3000_client_preserves_typed_http_contract() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = AgentClient::new(listener.local_addr().unwrap().to_string());
        let envelope = |message: &str| json!({"ok": true, "message": message});
        let server = tokio::spawn(record(
            listener,
            vec![
                ("GET /v1/status HTTP/1.1", None, envelope("idle")),
                ("GET /v1/get/init HTTP/1.1", None, envelope("OK init")),
                (
                    "POST /v1/flash/fw HTTP/1.1",
                    Some(json!({"name": "case-cag"})),
                    envelope("OK flash fw"),
                ),
                (
                    "POST /v1/flash/xdp HTTP/1.1",
                    Some(json!({"name": "case_mark", "slot": 1})),
                    envelope("OK xdp flash 1 41"),
                ),
                (
                    "POST /v1/xdp/switch HTTP/1.1",
                    Some(json!({"slot": 1, "generation": 41, "policy": "pass"})),
                    envelope("OK xdp switch 1 41 pass"),
                ),
                (
                    "GET /v1/get/map/rxcnt HTTP/1.1",
                    None,
                    json!({
                        "ok": true, "type": "array", "name": "rxcnt", "size": 1,
                        "entries": [{"key": "0", "value": "42"}],
                    }),
                ),
                (
                    "POST /v1/update/map/rxcnt HTTP/1.1",
                    Some(json!({"entries": [{"key": "0", "value": "3"}]})),
                    envelope("OK update map n=1"),
                ),
                (
                    "GET /v1/get/perf HTTP/1.1",
                    None,
                    envelope("OK get perf 4 b64=Q0FHVA=="),
                ),
                (
                    "POST /v1/enable/exec HTTP/1.1",
                    Some(json!({})),
                    envelope("OK enable exec"),
                ),
                (
                    "POST /v1/get/finish HTTP/1.1",
                    Some(json!({})),
                    envelope("OK finish"),
                ),
                (
                    "POST /v1/disable/xdp HTTP/1.1",
                    Some(json!({})),
                    json!({"ok": false, "message": "ERR not-ready"}),
                ),
            ],
        ));

        client.wait_idle(Duration::from_secs(1)).await.unwrap();
        assert_eq!(client.get_init().await.unwrap(), "OK init");
        client.flash_fw("case-cag").await.unwrap();
        assert_eq!(
            client.flash_xdp_slot("case_mark", 1).await.unwrap(),
            "OK xdp flash 1 41"
        );
        assert_eq!(
            client.switch_xdp(1, 41, "pass").await.unwrap(),
            "OK xdp switch 1 41 pass"
        );
        assert_eq!(client.map_slot("rxcnt", 0).await.unwrap(), 42);
        assert_eq!(
            client
                .update_map_batch("rxcnt", &[("0".into(), "3".into())])
                .await
                .unwrap(),
            "OK update map n=1"
        );
        assert_eq!(
            client.get_perf().await.unwrap(),
            "OK get perf 4 b64=Q0FHVA=="
        );
        assert_eq!(client.enable_exec().await.unwrap(), "OK enable exec");
        assert_eq!(client.get_finish().await.unwrap(), "OK finish");
        assert_eq!(
            client.disable_xdp().await.unwrap_err().to_string(),
            "ERR not-ready"
        );
        server.await.unwrap();
    })
    .await
    .unwrap();
}
