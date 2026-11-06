//! Python Web サーバー (sidecar) のプロセス管理.
//!
//! - アプリ起動時に sidecar を自動起動する (初回のみ。以降は Web 側から制御)
//! - `start_server` / `stop_server` / `server_status` / `server_logs` を Web 側に公開する
//! - sidecar の stdout/stderr は `server-log` イベントで Web 側へ転送する
//!   (Web 側の購読開始前のログも取りこぼさないよう、直近のログを保持して `server_logs` で返す)
//! - 状態変化は `server-status` イベントで通知する
//! - アプリ終了時には sidecar も停止する

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tokio::sync::{oneshot, watch};

const SIDECAR_NAME: &str = "python-server";
/// PyInstaller onefile は初回起動時に展開処理があるため余裕を持たせる
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// 起動時に自動起動する際のポート (使用中なら空きポートにフォールバック)
const AUTO_START_PORT: u16 = 8765;
/// 保持するログの最大行数
const LOG_CAPACITY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Stopped,
    Starting,
    Running,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerStatus {
    phase: Phase,
    pid: Option<u32>,
    port: Option<u16>,
    url: Option<String>,
}

impl ServerStatus {
    fn stopped() -> Self {
        Self {
            phase: Phase::Stopped,
            pid: None,
            port: None,
            url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct LogEntry {
    /// 連番 (Web 側で backlog とイベントの重複排除に使う)
    seq: u64,
    /// UNIX エポックからのミリ秒
    timestamp: u64,
    stream: &'static str,
    line: String,
}

#[derive(Default)]
struct LogBuffer(Mutex<(u64, VecDeque<LogEntry>)>);

/// Python 側が待ち受け開始時に出力する `READY {...}` の中身
#[derive(Debug, Deserialize)]
struct ReadyMessage {
    port: u16,
}

struct ServerProcess {
    child: CommandChild,
    pid: u32,
    port: Option<u16>,
    /// プロセス終了時に true になる
    exited: watch::Receiver<bool>,
}

impl ServerProcess {
    fn status(&self) -> ServerStatus {
        match self.port {
            Some(port) => ServerStatus {
                phase: Phase::Running,
                pid: Some(self.pid),
                port: Some(port),
                url: Some(format!("http://127.0.0.1:{port}")),
            },
            None => ServerStatus {
                phase: Phase::Starting,
                pid: Some(self.pid),
                port: None,
                url: None,
            },
        }
    }
}

#[derive(Default)]
struct ServerState(Mutex<Option<ServerProcess>>);

impl ServerState {
    fn status(&self) -> ServerStatus {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .map(ServerProcess::status)
            .unwrap_or_else(ServerStatus::stopped)
    }
}

fn emit_status(app: &AppHandle) {
    let status = app.state::<ServerState>().status();
    let _ = app.emit("server-status", status);
}

fn emit_log(app: &AppHandle, stream: &'static str, line: impl AsRef<str>) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let buffer = app.state::<LogBuffer>();
    // 連番の採番と emit を同じロック内で行い、イベントの順序を保証する
    let mut guard = buffer.0.lock().unwrap();
    guard.0 += 1;
    let entry = LogEntry {
        seq: guard.0,
        timestamp,
        stream,
        line: line.as_ref().trim_end().to_string(),
    };
    if guard.1.len() >= LOG_CAPACITY {
        guard.1.pop_front();
    }
    guard.1.push_back(entry.clone());
    let _ = app.emit("server-log", entry);
}

#[tauri::command]
fn server_status(state: State<'_, ServerState>) -> ServerStatus {
    state.status()
}

#[tauri::command]
fn server_logs(buffer: State<'_, LogBuffer>) -> Vec<LogEntry> {
    buffer.0.lock().unwrap().1.iter().cloned().collect()
}

#[tauri::command]
async fn start_server(app: AppHandle, port: Option<u16>) -> Result<ServerStatus, String> {
    start_process(&app, port.unwrap_or(0)).await
}

/// sidecar を起動し、READY になるまで待つ
async fn start_process(app: &AppHandle, port: u16) -> Result<ServerStatus, String> {
    let state = app.state::<ServerState>();
    let (ready_tx, ready_rx) = oneshot::channel::<u16>();
    let (exited_tx, exited_rx) = watch::channel(false);

    let pid = {
        let mut guard = state.0.lock().unwrap();
        if guard.is_some() {
            return Err("server is already running".into());
        }

        let (mut rx, child) = app
            .shell()
            .sidecar(SIDECAR_NAME)
            .map_err(|e| e.to_string())?
            .args(["--port", &port.to_string()])
            .spawn()
            .map_err(|e| format!("failed to spawn sidecar: {e}"))?;
        let pid = child.pid();

        // sidecar の出力を監視するタスク
        let app_handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut ready_tx = Some(ready_tx);
            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        if let Some(json) = text.trim().strip_prefix("READY ")
                            && let Ok(msg) = serde_json::from_str::<ReadyMessage>(json)
                        {
                            let state = app_handle.state::<ServerState>();
                            if let Some(p) = state.0.lock().unwrap().as_mut()
                                && p.pid == pid
                            {
                                p.port = Some(msg.port);
                            }
                            if let Some(tx) = ready_tx.take() {
                                let _ = tx.send(msg.port);
                            }
                            emit_status(&app_handle);
                        }
                        emit_log(&app_handle, "stdout", text);
                    }
                    CommandEvent::Stderr(bytes) => {
                        emit_log(&app_handle, "stderr", String::from_utf8_lossy(&bytes))
                    }
                    CommandEvent::Error(err) => emit_log(&app_handle, "error", err),
                    CommandEvent::Terminated(payload) => {
                        let msg = format!(
                            "process {pid} exited (code: {:?}, signal: {:?})",
                            payload.code, payload.signal
                        );
                        emit_log(&app_handle, "system", msg);
                        // 再起動後に古いプロセスの終了通知が来ても新しい方を消さないよう PID で確認
                        let state = app_handle.state::<ServerState>();
                        {
                            let mut guard = state.0.lock().unwrap();
                            if guard.as_ref().is_some_and(|p| p.pid == pid) {
                                *guard = None;
                            }
                        }
                        let _ = exited_tx.send(true);
                        emit_status(&app_handle);
                        break;
                    }
                    _ => {}
                }
            }
        });

        *guard = Some(ServerProcess {
            child,
            pid,
            port: None,
            exited: exited_rx,
        });
        pid
    };

    emit_log(
        app,
        "system",
        format!("spawned sidecar (pid {pid}, port {port})"),
    );
    emit_status(app);

    // READY を待つ
    match tokio::time::timeout(READY_TIMEOUT, ready_rx).await {
        Ok(Ok(_port)) => Ok(state.status()),
        Ok(Err(_)) => Err("server exited before becoming ready (see logs)".into()),
        Err(_) => {
            let _ = stop_process(&state).await;
            emit_status(app);
            Err(format!(
                "server did not become ready within {}s",
                READY_TIMEOUT.as_secs()
            ))
        }
    }
}

/// stdin に `shutdown` を送って graceful に停止させ、タイムアウトしたら kill する
async fn stop_process(state: &ServerState) -> bool {
    let Some(mut process) = state.0.lock().unwrap().take() else {
        return false;
    };
    let _ = process.child.write(b"shutdown\n");

    let mut exited = process.exited.clone();
    let graceful = tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, exited.wait_for(|v| *v))
        .await
        .is_ok();
    if !graceful {
        let _ = process.child.kill();
    }
    true
}

#[tauri::command]
async fn stop_server(
    app: AppHandle,
    state: State<'_, ServerState>,
) -> Result<ServerStatus, String> {
    if !stop_process(&state).await {
        return Err("server is not running".into());
    }
    emit_status(&app);
    Ok(state.status())
}

/// アプリ起動時の自動起動。ポートが使用中などで失敗したら空きポートで再試行する
async fn auto_start(app: AppHandle) {
    emit_log(&app, "system", "auto-starting server on app launch");
    let Err(err) = start_process(&app, AUTO_START_PORT).await else {
        return;
    };
    emit_log(
        &app,
        "error",
        format!("auto-start on port {AUTO_START_PORT} failed: {err}"),
    );
    // 既に別プロセスが起動済み (= ユーザーが先に Start した) 場合は再試行しない
    if app.state::<ServerState>().0.lock().unwrap().is_some() {
        return;
    }
    emit_log(
        &app,
        "system",
        "retrying with an automatically assigned port",
    );
    if let Err(err) = start_process(&app, 0).await {
        emit_log(&app, "error", format!("auto-start failed: {err}"));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ServerState::default())
        .manage(LogBuffer::default())
        .setup(|app| {
            tauri::async_runtime::spawn(auto_start(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_server,
            stop_server,
            server_status,
            server_logs
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // アプリ終了時に sidecar を確実に止める。
                // kill でブートローダーが終了すると stdin が閉じ、Python 側も EOF を検知して終了する。
                if let Some(mut process) = app.state::<ServerState>().0.lock().unwrap().take() {
                    let _ = process.child.write(b"shutdown\n");
                    let _ = process.child.kill();
                }
            }
        });
}
