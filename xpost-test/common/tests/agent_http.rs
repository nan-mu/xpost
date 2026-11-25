//! Host-only HTTP fixtures for the TR3000 client. No device or database.

#[allow(dead_code)]
#[path = "../src/agent.rs"]
mod agent;

use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
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
                ("GET /health HTTP/1.1", None, envelope("ok")),
                ("GET /v1/status HTTP/1.1", None, envelope("busy")),
                ("GET /v1/status HTTP/1.1", None, envelope("busy")),
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
                    "POST /v1/enable/xdp HTTP/1.1",
                    Some(json!({})),
                    envelope("OK enable xdp"),
                ),
                (
                    "POST /v1/disable/xdp HTTP/1.1",
                    Some(json!({})),
                    json!({"ok": false, "message": "ERR not-ready"}),
                ),
            ],
        ));

        client.health().await.unwrap();
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
        assert_eq!(client.enable_xdp().await.unwrap(), "OK enable xdp");
        assert_eq!(
            client.disable_xdp().await.unwrap_err().to_string(),
            "ERR not-ready"
        );
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn tr3000_reads_deployment_perf_and_exb2_from_typed_routes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = AgentClient::new(listener.local_addr().unwrap().to_string());
    let image_sha = "ab".repeat(32);
    let firmware = json!({"firmware_profile": "carrier", "firmware_sha256": image_sha});
    let report = json!({
        "release": "v0.1.1", "required": 1, "deployments": [{}], "issues": [],
        "prepared": [{
            "program": "case_mark", "mode": "aot", "profile": "mark", "slot": 1,
            "firmware_profile": "carrier", "firmware_sha256": image_sha,
        }],
        "hal_firmware": [firmware.clone()],
    });
    let exp = "0".repeat(8258 * 2);
    let capture = vec![57; 600];
    // Independent FNV-1a digest for 600 ASCII '9' bytes.
    let digest = 0xda68698d_u32;
    let envelope = |message: &str| json!({"ok": true, "message": message});
    let server = tokio::spawn(record(
        listener,
        vec![
            (
                "GET /v1/preflight/medium/hal-aot HTTP/1.1",
                None,
                json!({"ok": true, "preflight": report}),
            ),
            (
                "POST /v1/flash/fw HTTP/1.1",
                Some(json!({"name": "carrier", "expected_sha256": image_sha})),
                json!({"ok": true, "message": "flashed", "flash": {
                   "firmware_profile": "carrier", "firmware_sha256": image_sha, "containers": [],
                }}),
            ),
            (
                "POST /v1/flash/xdp HTTP/1.1",
                Some(json!({"name": "mark", "firmware": "carrier", "expected_sha256": image_sha})),
                envelope("installed"),
            ),
            (
                "POST /v1/perf/prepare HTTP/1.1",
                Some(json!({
                   "uuid": "01010101010101010101010101010101", "variant": "trace", "profile": image_sha,
                })),
                envelope("OK perf prepare epoch=7 boot_id=42"),
            ),
            (
                "POST /v1/perf/arm HTTP/1.1",
                Some(json!({"epoch": 7})),
                envelope("OK perf arm tick=21"),
            ),
            (
                "POST /v1/perf/freeze HTTP/1.1",
                Some(json!({"epoch": 7})),
                envelope(&format!(
                    "OK perf freeze tick=24 flags=2 len=600 digest={digest}"
                )),
            ),
            (
                "POST /v1/perf/read HTTP/1.1",
                Some(json!({"epoch": 7, "offset": 0, "max_bytes": 512})),
                envelope(&format!(
                    "OK perf read total=600 digest={digest} n=512 b64={}",
                    STANDARD.encode(&capture[..512])
                )),
            ),
            (
                "POST /v1/perf/read HTTP/1.1",
                Some(json!({"epoch": 7, "offset": 512, "max_bytes": 512})),
                envelope(&format!(
                    "OK perf read total=600 digest={digest} n=88 b64={}",
                    STANDARD.encode(&capture[512..])
                )),
            ),
            (
                "POST /v1/perf/release HTTP/1.1",
                Some(json!({"epoch": 7})),
                envelope("OK perf release"),
            ),
            (
                "POST /v1/exec/batch HTTP/1.1",
                Some(json!({"input_hex": exp})),
                json!({"ok": true, "result": {
                   "boot_id": 42, "begin_tick": 20, "end_tick": 30, "execution_return": 2,
                   "output": STANDARD.encode(vec![0x5a; 8258]),
                }}),
            ),
            (
                "GET /v1/preflight/medium/hal-aot HTTP/1.1",
                None,
                json!({"ok": false, "preflight": {
                   "release": "v0.1.1", "required": 1, "deployments": [], "issues": ["missing layout"],
                }}),
            ),
        ],
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        let preflight = client.medium_preflight(Some("hal-aot")).await.unwrap();
        assert_eq!(
            (
                preflight.prepared[0].slot,
                preflight.prepared[0].mode.as_str()
            ),
            (1, "aot")
        );
        assert_eq!(preflight.prepared[0].firmware.firmware_sha256, image_sha);
        assert_eq!(preflight.hal_firmware[0].firmware_profile, "carrier");
        let flash = client
            .flash_fw_evidence("carrier", Some(&image_sha))
            .await
            .unwrap();
        assert_eq!(flash["containers"], json!([]));
        assert_eq!(flash["firmware_sha256"], image_sha);
        client
            .bind_xdp("mark", "carrier", &image_sha)
            .await
            .unwrap();
        let run = uuid::Uuid::from_bytes([1; 16]);
        assert_eq!(
            client.perf_prepare(run, &image_sha, "trace").await.unwrap(),
            (7, 42)
        );
        assert_eq!(client.perf_arm(7).await.unwrap(), 21);
        let freeze = client.perf_freeze(7).await.unwrap();
        assert_eq!((freeze.tick, freeze.flags), (24, 2));
        assert_eq!(client.perf_read(7, freeze).await.unwrap(), capture);
        client.perf_release(7).await.unwrap();
        let result = client.exb2(&exp).await.unwrap();
        assert_eq!(
            (
                result.boot_id,
                result.begin_tick,
                result.end_tick,
                result.execution_return
            ),
            (42, 20, 30, 2)
        );
        assert_eq!(result.output, vec![0x5a; 8258]);
        let rejection = client
            .medium_preflight(Some("hal-aot"))
            .await
            .err()
            .expect("preflight must refuse");
        assert!(rejection.to_string().contains("missing layout"));
        server.await.unwrap();
    })
    .await
    .unwrap();
}
