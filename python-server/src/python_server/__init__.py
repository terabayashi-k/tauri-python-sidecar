"""Tauri sidecar として動作する Python Web サーバー.

起動プロトコル (Tauri 側 = src-tauri/src/lib.rs と対応):
  - 引数 `--port <n>` で待ち受けポートを指定 (0 なら空いているポートを自動選択)
  - 待ち受け開始後、stdout に `READY {"port": <n>, "pid": <pid>}` を 1 行出力する
  - stdin に `shutdown` 行が来るか、stdin が閉じられたら (= 親プロセス消滅) graceful shutdown する
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import socket
import sys
import threading
import time
from contextlib import asynccontextmanager
from datetime import datetime, timezone

import uvicorn
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

# Tauri WebView のオリジン (dev: Vite, macOS/Linux: tauri://, Windows: http://tauri.localhost)
ALLOWED_ORIGINS = [
    "http://localhost:1420",
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
]

STARTED_AT = time.time()


class _State:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.counter = 0
        self.port = 0


state = _State()


class EchoRequest(BaseModel):
    message: str


def _out(msg: str, *, err: bool = False) -> None:
    """stdout/stderr へ 1 行出力する。

    親 (Tauri) が強制終了するとパイプが壊れて BrokenPipeError になるため、
    出力の失敗でシャットダウン処理が止まらないよう例外は握りつぶす。
    """
    try:
        print(msg, file=sys.stderr if err else sys.stdout, flush=True)
    except (OSError, ValueError):
        pass


def _create_app() -> FastAPI:
    @asynccontextmanager
    async def lifespan(_: FastAPI):
        # ソケットは bind/listen 済みなので、この時点で接続を受け付けられる
        _out("READY " + json.dumps({"port": state.port, "pid": os.getpid()}))
        yield
        _out("server shutting down")

    app = FastAPI(title="Tauri Python Sidecar", lifespan=lifespan)
    app.add_middleware(
        CORSMiddleware,
        allow_origins=ALLOWED_ORIGINS,
        allow_methods=["*"],
        allow_headers=["*"],
    )

    @app.get("/api/health")
    def health() -> dict:
        return {"status": "ok"}

    @app.get("/api/info")
    def info() -> dict:
        return {
            "pid": os.getpid(),
            "port": state.port,
            "python": sys.version.split()[0],
            "platform": platform.platform(),
            "frozen": getattr(sys, "frozen", False),  # PyInstaller でビルドされていれば True
            "startedAt": datetime.fromtimestamp(STARTED_AT, timezone.utc).isoformat(),
            "uptimeSec": round(time.time() - STARTED_AT, 2),
        }

    @app.post("/api/echo")
    def echo(req: EchoRequest) -> dict:
        return {
            "echo": req.message,
            "upper": req.message.upper(),
            "length": len(req.message),
            "receivedAt": datetime.now(timezone.utc).isoformat(),
        }

    @app.get("/api/counter")
    def get_counter() -> dict:
        with state.lock:
            return {"counter": state.counter}

    @app.post("/api/counter/increment")
    def increment_counter() -> dict:
        with state.lock:
            state.counter += 1
            return {"counter": state.counter}

    return app


def _watch_stdin(server: uvicorn.Server) -> None:
    """親 (Tauri) からの停止指示を待つ。stdin が EOF になった場合も停止する。

    PyInstaller の onefile バイナリはブートローダーが子プロセスとして Python を起動するため、
    Tauri がブートローダーを kill しても Python 側が残ることがある。stdin のパイプは子にも
    継承されるので、EOF を検知して自分で終了することで孤児プロセス化を防ぐ。
    """
    try:
        for line in sys.stdin:
            if line.strip() == "shutdown":
                _out("shutdown requested via stdin")
                break
        else:
            _out("stdin closed (parent exited)")
    except Exception as exc:  # noqa: BLE001
        _out(f"stdin watcher error: {exc}", err=True)
    finally:
        server.should_exit = True


def main() -> None:
    parser = argparse.ArgumentParser(description="Tauri Python sidecar web server")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0, help="0 = 空きポートを自動選択")
    parser.add_argument(
        "--no-stdin-watch",
        action="store_true",
        help="stdin の監視を無効化 (ターミナルから単体起動する場合など)",
    )
    args = parser.parse_args()

    # 自前で bind しておくことで、port=0 の場合も実際のポート番号を READY で通知できる
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock.bind((args.host, args.port))
    except OSError as exc:
        _out(f"failed to bind {args.host}:{args.port}: {exc.strerror}", err=True)
        sys.exit(1)
    sock.listen(128)
    state.port = sock.getsockname()[1]

    config = uvicorn.Config(
        _create_app(),
        loop="asyncio",
        http="h11",
        lifespan="on",
        log_level="info",
    )
    server = uvicorn.Server(config)

    if not args.no_stdin_watch:
        threading.Thread(target=_watch_stdin, args=(server,), daemon=True).start()

    try:
        server.run(sockets=[sock])
    finally:
        # 親が消えて stdout が壊れている場合、終了時の flush で出るエラーを抑止する
        try:
            sys.stdout.flush()
        except (OSError, ValueError):
            devnull = os.open(os.devnull, os.O_WRONLY)
            os.dup2(devnull, sys.stdout.fileno())


if __name__ == "__main__":
    main()
