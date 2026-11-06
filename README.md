# Tauri + Python Sidecar Demo

Python の Web サーバー (FastAPI) を Tauri の [sidecar](https://v2.tauri.app/develop/sidecar/) として動かし、
その振る舞いを確認するデモです。

- アプリ起動時にサーバーを自動起動し、画面から停止 / 起動 / 再起動できる
- 画面から `fetch` でサーバーの API を呼び出せる
- サーバーのログを画面にリアルタイム表示する

## 起動

[mise](https://mise.jdx.dev/) と [Tauri の前提条件](https://v2.tauri.app/start/prerequisites/) が必要です。

```sh
mise install
pnpm install
pnpm tauri:dev   # sidecar をビルドしてから tauri dev
```

Python 側を変更したら `pnpm build:sidecar` で sidecar を再ビルドしてください。
リリースビルドは `pnpm tauri build` です (sidecar も自動でビルドされます)。

## 構成

```
├── src/                 # フロントエンド (TypeScript)
├── src-tauri/src/lib.rs # sidecar のプロセス管理
└── python-server/       # Python サーバー (uv) と PyInstaller でのビルドスクリプト
```

## 仕組み

- `pnpm build:sidecar` で Python サーバーを PyInstaller で単一バイナリにし、
  `src-tauri/binaries/python-server-<target-triple>` に置く
- Rust がアプリ起動時に sidecar を起動する (ポート 8765。使用中なら空きポート)
- Python は待ち受けを始めると stdout に `READY {"port": ...}` を出力し、Rust はこれで起動完了を知る
- 停止時は Rust が stdin に `shutdown` を送り、Python が自分で終了する
- アプリが強制終了した場合も、stdin が閉じたことを Python が検知して終了する
  (PyInstaller のバイナリは 2 プロセス構成で、親を kill するだけでは Python 側が残るため)

動作確認は macOS でのみ行っています。
