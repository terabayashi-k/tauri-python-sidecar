import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Phase = "stopped" | "starting" | "running";

interface ServerStatus {
  phase: Phase;
  pid: number | null;
  port: number | null;
  url: string | null;
}

interface LogEntry {
  seq: number;
  timestamp: number;
  stream: "stdout" | "stderr" | "error" | "system";
  line: string;
}

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel)!;

let current: ServerStatus = { phase: "stopped", pid: null, port: null, url: null };
let busy = false;
let lastLogSeq = 0;

// ---------- 表示 ----------

function render() {
  const badge = $("#status-badge");
  badge.textContent = current.phase;
  badge.className = `badge ${current.phase}`;

  $("#pid").textContent = current.pid?.toString() ?? "-";
  // 実際に使われているポートを入力欄にも反映 (自動起動や自動割り当ての場合)
  if (current.port !== null) $<HTMLInputElement>("#port-input").value = String(current.port);
  $("#url").textContent = current.url ?? "-";

  const running = current.phase === "running";
  $<HTMLButtonElement>("#start-btn").disabled = busy || current.phase !== "stopped";
  $<HTMLButtonElement>("#stop-btn").disabled = busy || current.phase === "stopped";
  $<HTMLButtonElement>("#restart-btn").disabled = busy || !running;
  $<HTMLInputElement>("#port-input").disabled = busy || current.phase !== "stopped";
  document.querySelectorAll<HTMLButtonElement>(".api-btn").forEach((b) => (b.disabled = !running));
}

function setError(msg: string) {
  $("#control-error").textContent = msg;
}

function appendLog({ seq, timestamp, stream, line }: LogEntry) {
  // backlog とイベントの重複を除外
  if (seq <= lastLogSeq) return;
  lastLogSeq = seq;
  const log = $("#log");
  const atBottom = log.scrollTop + log.clientHeight >= log.scrollHeight - 4;
  const el = document.createElement("div");
  el.className = `log-line ${stream}`;
  const time = new Date(timestamp).toLocaleTimeString();
  el.textContent = `[${time}] [${stream}] ${line}`;
  log.appendChild(el);
  // 古いログは間引く
  while (log.childElementCount > 500) log.firstElementChild?.remove();
  if (atBottom) log.scrollTop = log.scrollHeight;
}

// ---------- サーバー制御 (Rust コマンド呼び出し) ----------

async function control(action: () => Promise<void>) {
  busy = true;
  setError("");
  render();
  try {
    await action();
  } catch (e) {
    setError(String(e));
  } finally {
    busy = false;
    current = await invoke<ServerStatus>("server_status");
    render();
  }
}

async function startServer() {
  const port = Number($<HTMLInputElement>("#port-input").value) || 0;
  current = await invoke<ServerStatus>("start_server", { port });
}

async function stopServer() {
  current = await invoke<ServerStatus>("stop_server");
}

// ---------- Python サーバーへの HTTP 通信 ----------

async function callApi(method: "GET" | "POST", path: string, body?: unknown) {
  if (!current.url) return;
  const started = performance.now();
  const meta = $("#response-meta");
  const out = $("#response");
  try {
    const res = await fetch(`${current.url}${path}`, {
      method,
      headers: body ? { "Content-Type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    const json = await res.json();
    meta.textContent = `${method} ${path} → ${res.status} (${Math.round(performance.now() - started)} ms)`;
    out.textContent = JSON.stringify(json, null, 2);
    out.classList.toggle("failed", !res.ok);
  } catch (e) {
    meta.textContent = `${method} ${path} → failed`;
    out.textContent = String(e);
    out.classList.add("failed");
  }
}

const apis: Record<string, () => Promise<void>> = {
  health: () => callApi("GET", "/api/health"),
  info: () => callApi("GET", "/api/info"),
  counter: () => callApi("GET", "/api/counter"),
  increment: () => callApi("POST", "/api/counter/increment"),
};

// ---------- 初期化 ----------

window.addEventListener("DOMContentLoaded", async () => {
  $("#start-btn").addEventListener("click", () => control(startServer));
  $("#stop-btn").addEventListener("click", () => control(stopServer));
  $("#restart-btn").addEventListener("click", () =>
    control(async () => {
      await stopServer();
      await startServer();
    }),
  );
  $("#clear-log-btn").addEventListener("click", () => ($("#log").textContent = ""));

  document.querySelectorAll<HTMLButtonElement>("[data-api]").forEach((btn) => {
    btn.addEventListener("click", () => apis[btn.dataset.api!]?.());
  });
  $("#echo-form").addEventListener("submit", (e) => {
    e.preventDefault();
    callApi("POST", "/api/echo", { message: $<HTMLInputElement>("#echo-input").value });
  });

  // Rust 側からのイベント
  await listen<ServerStatus>("server-status", (e) => {
    current = e.payload;
    render();
  });

  // サーバーはアプリ起動時に Rust 側で自動起動されるため、購読開始前のログは backlog から取得する。
  // backlog 取得中に届いたイベントはいったん溜めておき、backlog の後に追加する。
  let backlogLoaded = false;
  const pending: LogEntry[] = [];
  await listen<LogEntry>("server-log", (e) => {
    if (backlogLoaded) appendLog(e.payload);
    else pending.push(e.payload);
  });
  (await invoke<LogEntry[]>("server_logs")).forEach(appendLog);
  pending.forEach(appendLog);
  backlogLoaded = true;

  current = await invoke<ServerStatus>("server_status");
  render();
});
