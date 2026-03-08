"""Memory test — report RSS used by Python (OpenAgent + app), Go, and Rust services.

Run with: pytest app/tests/test_memory.py -v -s
Or standalone: python -m app.tests.test_memory

Requires the app to be running (uvicorn app.main:app) so that ServiceManager
has spawned Go/Rust services. Run this while the app is up.
"""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

import psutil
import pytest

ROOT = Path(__file__).resolve().parents[2]

# Known Go services from services/
GO_SERVICES = {"discord", "telegram", "slack", "whatsapp"}
# Known Rust services
RUST_SERVICES = {"sandbox", "browser"}


def _rss_mb(proc: psutil.Process) -> float | None:
    try:
        return round(proc.memory_info().rss / (1024 * 1024), 1)
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return None


def _collect_memory() -> dict[str, list[dict]]:
    """Collect RSS for Python, Go, and Rust processes related to OpenAgent."""
    python_procs: list[dict] = []
    go_procs: list[dict] = []
    rust_procs: list[dict] = []
    root_str = str(ROOT)

    for proc in psutil.process_iter(["pid", "name", "cmdline", "exe"]):
        try:
            cmdline = proc.info.get("cmdline") or []
            cmd_str = " ".join(cmdline) if isinstance(cmdline, list) else str(cmdline)
            exe = proc.info.get("exe") or ""
            combined = f"{cmd_str} {exe}"

            rss = _rss_mb(proc)
            if rss is None:
                continue

            entry = {"pid": proc.pid, "name": proc.info.get("name", "?"), "rss_mb": rss}

            # Python: uvicorn app.main, openagent
            pname = (proc.info.get("name") or "").lower()
            if "python" in pname:
                if "uvicorn" in cmd_str or "app.main" in cmd_str or "openagent" in cmd_str:
                    python_procs.append(entry)
                    continue

            # Go/Rust: binary from <root>/bin/<name>-<os>-<arch>
            if root_str in combined or "openagent" in combined.lower():
                for svc in GO_SERVICES:
                    if f"/bin/{svc}-" in combined or f"/{svc}-" in combined:
                        entry["service"] = svc
                        go_procs.append(entry)
                        break
                else:
                    # Rust: sandbox, browser
                    for svc in RUST_SERVICES:
                        if f"/bin/{svc}-" in combined or f"/{svc}" in exe:
                            entry["service"] = svc
                            rust_procs.append(entry)
                            break

        except (psutil.NoSuchProcess, psutil.AccessDenied, TypeError):
            continue

    return {
        "python": python_procs,
        "go": go_procs,
        "rust": rust_procs,
    }


def test_memory_report() -> None:
    """Report memory used by Python, Go, and Rust components."""
    data = _collect_memory()

    python_total = sum(p["rss_mb"] for p in data["python"])
    go_total = sum(p["rss_mb"] for p in data["go"])
    rust_total = sum(p["rss_mb"] for p in data["rust"])
    grand_total = python_total + go_total + rust_total

    lines = [
        "",
        "=== OpenAgent Memory Report ===",
        "",
        "Python (OpenAgent + app + extensions):",
    ]
    for p in data["python"]:
        lines.append(f"  PID {p['pid']}: {p['rss_mb']} MB")
    lines.append(f"  Subtotal: {python_total:.1f} MB")
    lines.append("")

    lines.append("Go services:")
    for p in data["go"]:
        svc = p.get("service", "?")
        lines.append(f"  {svc} (PID {p['pid']}): {p['rss_mb']} MB")
    lines.append(f"  Subtotal: {go_total:.1f} MB")
    lines.append("")

    lines.append("Rust services:")
    for p in data["rust"]:
        svc = p.get("service", "?")
        lines.append(f"  {svc} (PID {p['pid']}): {p['rss_mb']} MB")
    lines.append(f"  Subtotal: {rust_total:.1f} MB")
    lines.append("")

    lines.append(f"Total: {grand_total:.1f} MB")
    lines.append("")

    out = "\n".join(lines)
    print(out)

    # Always pass — this is a reporting test (run with pytest -s to see output)
    assert True


def run_standalone() -> None:
    """Run as standalone script: python -m app.tests.test_memory"""
    print("Collecting memory (ensure app is running: uvicorn app.main:app)...")
    time.sleep(0.5)
    data = _collect_memory()

    python_total = sum(p["rss_mb"] for p in data["python"])
    go_total = sum(p["rss_mb"] for p in data["go"])
    rust_total = sum(p["rss_mb"] for p in data["rust"])
    grand_total = python_total + go_total + rust_total

    print("\n=== OpenAgent Memory Report ===\n")
    print("Python (OpenAgent + app + extensions):")
    for p in data["python"]:
        print(f"  PID {p['pid']}: {p['rss_mb']} MB")
    print(f"  Subtotal: {python_total:.1f} MB\n")

    print("Go services:")
    for p in data["go"]:
        svc = p.get("service", "?")
        print(f"  {svc} (PID {p['pid']}): {p['rss_mb']} MB")
    print(f"  Subtotal: {go_total:.1f} MB\n")

    print("Rust services:")
    for p in data["rust"]:
        svc = p.get("service", "?")
        print(f"  {svc} (PID {p['pid']}): {p['rss_mb']} MB")
    print(f"  Subtotal: {rust_total:.1f} MB\n")

    print(f"Total: {grand_total:.1f} MB\n")


if __name__ == "__main__":
    run_standalone()
