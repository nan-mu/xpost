"""Check the binary's HTTP requests against the baseline route contract."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
from queue import Queue
import subprocess
import sys
from threading import Thread


binary = str(Path(sys.argv[1]).resolve())
requests = Queue()
replies = Queue()


class Record(BaseHTTPRequestHandler):
    def handle_request(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length)) if length else None
        requests.put((self.command, self.path, body))
        status, response = replies.get(timeout=2)
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    do_GET = handle_request
    do_POST = handle_request

    def log_message(self, *_):
        pass


def invoke(endpoint, args):
    return subprocess.run(
        [binary, "--endpoint", endpoint, *args], capture_output=True, text=True, timeout=10,
    )


ok = {"ok": True, "message": "ok", "identity": {"offset": 4096, "digest": "ab"}}
map_reply = {
    "ok": True, "name": "rxcnt", "type": "array", "size": 1,
    "entries": [{"key": "0", "value": "42"}],
}
perf_reply = {"ok": True, "message": "OK get perf 4 b64=Q0FHVA=="}
cases = [
    (["health"], "GET", "/health", None, {"ok": True, "message": "ok"}),
    (["get", "status"], "GET", "/v1/status", None, {"ok": True, "message": "idle"}),
    (["dut", "status"], "GET", "/v1/dut_status", None, ok),
    (["get", "init"], "GET", "/v1/get/init", None, ok),
    (["get", "finish"], "POST", "/v1/get/finish", {}, ok),
    (["flash", "fw", "test-fw"], "POST", "/v1/flash/fw", {"name": "test-fw"}, ok),
    (["flash", "xdp", "image"], "POST", "/v1/flash/xdp", {"name": "image"}, ok),
    (["flash", "xdp", "image", "--slot", "1"], "POST", "/v1/flash/xdp",
     {"name": "image", "slot": 1}, ok),
    (["clear", "xdp", "*"], "POST", "/v1/clear/xdp", {"name": "*"}, ok),
    (["clear", "xdp", "a", "b"], "POST", "/v1/clear/xdp", {"name": ["a", "b"]}, ok),
    (["get", "xdp"], "GET", "/v1/get/xdp", None, ok),
    (["enable", "xdp"], "POST", "/v1/enable/xdp", {}, ok),
    (["disable", "xdp"], "POST", "/v1/disable/xdp", {}, ok),
    (["xdp", "switch", "--slot", "1", "--generation", "550", "--policy", "pass"],
     "POST", "/v1/xdp/switch", {"slot": 1, "generation": 550, "policy": "pass"}, ok),
    (["xdp", "status"], "GET", "/v1/xdp/status", None, ok),
    (["enable", "exec"], "POST", "/v1/enable/exec", {}, ok),
    (["disable", "exec"], "POST", "/v1/disable/exec", {}, ok),
    (["clear", "map", "rxcnt"], "POST", "/v1/clear/map/rxcnt", {}, ok),
    (["get", "map", "rxcnt"], "GET", "/v1/get/map/rxcnt", None, map_reply),
    (["get", "map", "a/b"], "GET", "/v1/get/map/a%2Fb", None, map_reply),
    (["update", "map", "rxcnt", "--key", "0", "--value", "42"],
     "POST", "/v1/update/map/rxcnt", {"key": "0", "value": "42"}, ok),
    (["update", "map", "blacklist", "--entries",
      '[{"key":"203.0.113.10","value":"1"},{"key":"203.0.113.77","value":"1"}]'],
     "POST", "/v1/update/map/blacklist", {"entries": [
         {"key": "203.0.113.10", "value": "1"}, {"key": "203.0.113.77", "value": "1"},
     ]}, ok),
    (["clear", "perf"], "POST", "/v1/clear/perf", {}, ok),
    (["get", "perf"], "GET", "/v1/get/perf", None, perf_reply),
    (["get", "serial", "--since-unix-ms", "309924007", "--until-unix-ms", "309925009"],
     "POST", "/v1/get/serial", {"since_unix_ms": 309924007, "until_unix_ms": 309925009}, ok),
    (["run", "ping"], "POST", "/v1/run", {"cmd": "ping"}, ok),
]


with ThreadingHTTPServer(("127.0.0.1", 0), Record) as server:
    endpoint = f"127.0.0.1:{server.server_port}"
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        for args, method, path, body, response in cases:
            replies.put((200, json.dumps(response).encode()))
            result = invoke(endpoint, args)
            assert result.returncode == 0, (args, result.stderr)
            assert requests.get(timeout=2) == (method, path, body), args
            assert json.loads(result.stdout) == response, args
        for status, body, message in [
            (200, b'{"ok":false,"message":"DUT refused"}', "DUT refused"),
            (200, b'{"message":"missing ok"}', "no boolean ok"),
            (200, b'not JSON', "decode HTTP response JSON"),
            (503, b'{"ok":true,"message":"offline"}', "HTTP 503"),
            (302, b'{"ok":true,"message":"redirect"}', "HTTP 302"),
        ]:
            replies.put((status, body))
            result = invoke(endpoint, ["enable", "xdp"])
            assert result.returncode != 0 and message in result.stderr, result
            assert requests.get(timeout=2) == ("POST", "/v1/enable/xdp", {}), result
            assert requests.empty(), "request was retried"
            if body.startswith(b"{"):
                assert json.loads(result.stdout) == json.loads(body)
    finally:
        server.shutdown()
        thread.join(timeout=2)

connection = invoke(endpoint, ["health"])
assert connection.returncode != 0 and "send request" in connection.stderr, connection
print(f"{len(cases)} CLI cases matched the baseline HTTP methods, paths, bodies, and responses")
print("Business failure, missing ok, malformed JSON, HTTP failure, redirect, and connection failure exited nonzero")
