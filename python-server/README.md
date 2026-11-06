# python-server

sidecar として同梱する Python の Web サーバー (FastAPI + uvicorn) です。

```sh
uv run python-server --port 8765 --no-stdin-watch   # 単体で起動 (http://127.0.0.1:8765/docs)
uv run python build_sidecar.py                      # sidecar 用バイナリをビルド
```

- `--port 0` で空きポートを自動選択
- 起動すると stdout に `READY {"port": ..., "pid": ...}` を出力
- stdin に `shutdown` が来るか、stdin が閉じると終了 (`--no-stdin-watch` で無効化)

API: `GET /api/health`, `GET /api/info`, `POST /api/echo`, `GET /api/counter`, `POST /api/counter/increment`
