"""PyInstaller で Python サーバーを単一バイナリ化し、Tauri の sidecar として配置する.

Tauri の externalBin は `<name>-<target-triple>[.exe]` というファイル名を要求するため、
rustc からホストの target triple を取得してリネームする。

    uv run --directory python-server python build_sidecar.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

NAME = "python-server"
ROOT = Path(__file__).resolve().parent
BUILD_DIR = ROOT / "build"
BINARIES_DIR = ROOT.parent / "src-tauri" / "binaries"


def target_triple() -> str:
    # 環境変数で上書き可能 (クロスビルド用)
    if triple := os.environ.get("TAURI_TARGET_TRIPLE"):
        return triple
    out = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    for line in out.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    raise RuntimeError("failed to determine target triple from `rustc -vV`")


def main() -> None:
    triple = target_triple()
    ext = ".exe" if sys.platform == "win32" else ""

    subprocess.run(
        [
            sys.executable,
            "-m",
            "PyInstaller",
            "--onefile",
            "--clean",
            "--noconfirm",
            "--name",
            NAME,
            "--distpath",
            str(BUILD_DIR / "dist"),
            "--workpath",
            str(BUILD_DIR / "work"),
            "--specpath",
            str(BUILD_DIR),
            str(ROOT / "entry.py"),
        ],
        check=True,
        cwd=ROOT,
    )

    src = BUILD_DIR / "dist" / f"{NAME}{ext}"
    BINARIES_DIR.mkdir(parents=True, exist_ok=True)
    dst = BINARIES_DIR / f"{NAME}-{triple}{ext}"
    shutil.copy2(src, dst)
    print(f"sidecar copied: {dst}")


if __name__ == "__main__":
    main()
