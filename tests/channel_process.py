"""Exercise the shipped CLI with a local mock terminal; never contacts SecureCRT."""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser()
parser.add_argument("--exe", type=Path, required=True)
parser.add_argument("--data", type=Path, required=True)
parser.add_argument("--serve", action="store_true")
args = parser.parse_args()
jobs = {}
calls = []

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.handle_request()

    def do_POST(self):
        self.handle_request()

    def handle_request(self):
        assert self.headers["Authorization"] == "Bearer fixture-only"
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        calls.append((self.path, body))
        if self.path == "/sessions/fixture/screen":
            value = {"ok": True, "connected": True, "screen": "fixture$ "}
        elif self.path == "/sessions/fixture/exec":
            rid = body["request_id"]
            value = {"ok": True, "status": "queued", "id": rid}
            jobs[rid] = {"ok": True, "status": "done", "session": "fixture", "owner": body["owner"], "result": {"output": body["command"], "exit_code": 0}}
        elif self.path.startswith("/jobs/"):
            value = jobs[self.path.split("/")[-1]]
        elif self.path in ("/sessions/fixture/claim", "/sessions/fixture/release"):
            value = {"ok": True}
        else:
            value = {"ok": False, "error": "Unexpected fixture request"}
        data = json.dumps(value).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
args.data.mkdir(parents=True, exist_ok=True)
(args.data / "connection.json").write_text(json.dumps({"port": server.server_port, "token": "fixture-only", "epoch": "fixture", "pid": os.getpid(), "version": "fixture"}))
threading.Thread(target=server.serve_forever, daemon=True).start()
if args.serve:
    print("Fixture ready; expires after 180 seconds", flush=True)
    threading.Event().wait(180)
else:
    prefix = [str(args.exe)] + ([] if args.exe.stem == "securecrt-session" else ["attach"])
    process = subprocess.Popen(prefix + ["fixture", "--owner", "fixture-chat"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", env={**os.environ, "SCB_DATA_DIR": str(args.data)})
    lines = queue.Queue()
    def read_lines():
        for line in process.stdout:
            lines.put(json.loads(line))
    threading.Thread(target=read_lines, daemon=True).start()
    def receive():
        return lines.get(timeout=10)
    def send(value):
        process.stdin.write(json.dumps(value, ensure_ascii=False) + "\n")
        process.stdin.flush()
    try:
        assert receive()["event"] == "ready"
        for command in ["printf 'first'", "中文输出\n第二行含引号\" 和反斜杠\\"]:
            assert process.poll() is None
            send({"op": "exec", "command": command, "timeout": 15})
            rid = receive()["request_id"]
            assert receive()["ok"] is True
            send({"op": "result", "request_id": rid})
            assert receive()["result"]["output"] == command
        send({"op": "exec", "session": "different", "command": "forbidden"})
        assert receive()["ok"] is False
        send({"op": "close"})
        assert receive()["released"] is True
        assert process.wait(timeout=5) == 0
        assert sum(path.endswith("/exec") for path, _ in calls) == 2
        print("PASS: one CLI process, separate stdin writes, Unicode/multiline, scope rejection, close/release")
    finally:
        if process.poll() is None:
            process.kill()
        server.shutdown()
