#!/usr/bin/env python3
"""
Manual MCP-lite test client for the browser service.

Usage:
    # Start the binary first:
    #   OPENAGENT_SOCKET_PATH=/tmp/browser-test.sock \\
    #   SEARXNG_URL=http://100.96.81.109:8888 \\
    #   ./target/debug/browser

    # Then run this script from services/browser/:
    python3 tests/manual_test.py

    # Or let the script create/use a venv automatically:
    #   python3 tests/manual_test.py   (no dependencies needed — stdlib only)

Options (env vars):
    BROWSER_SOCK   Unix socket path  (default: /tmp/browser-test.sock)
    SEARXNG_URL    SearXNG base URL  (default: http://100.96.81.109:8888)
    TEST_QUERY     Search query      (default: rust tokio async runtime)
    TEST_URL       URL to fetch      (default: https://httpbin.org/get)
    TIMEOUT        Socket timeout s  (default: 30)
"""

# ── venv bootstrap ─────────────────────────────────────────────────────────────
# This script uses only the standard library so no venv is strictly required.
# The block below is kept as a hook if you ever add third-party deps (e.g. rich).
import sys
import os

_VENV = os.path.join(os.path.dirname(__file__), ".venv")

def _ensure_venv() -> None:
    """Create a venv and re-exec into it if we're not already inside one."""
    if sys.prefix != sys.base_prefix:
        return  # already in a venv
    if not os.path.isdir(_VENV):
        import venv
        print(f"Creating venv at {_VENV} …")
        venv.create(_VENV, with_pip=True, clear=False)
    python = os.path.join(_VENV, "bin", "python3")
    os.execv(python, [python] + sys.argv)

_ensure_venv()
# ───────────────────────────────────────────────────────────────────────────────

import json
import os
import socket
import sys
import textwrap
from typing import Optional

SOCK_PATH  = os.getenv("BROWSER_SOCK",  "/tmp/browser-test.sock")
TIMEOUT    = int(os.getenv("TIMEOUT",   "30"))
TEST_QUERY = os.getenv("TEST_QUERY",    "rust tokio async runtime")
TEST_URL   = os.getenv("TEST_URL",      "https://httpbin.org/get")

RESET  = "\033[0m"
BOLD   = "\033[1m"
GREEN  = "\033[32m"
RED    = "\033[31m"
YELLOW = "\033[33m"
CYAN   = "\033[36m"

def hdr(title: str) -> None:
    print(f"\n{BOLD}{CYAN}{'─'*60}{RESET}")
    print(f"{BOLD}{CYAN}  {title}{RESET}")
    print(f"{BOLD}{CYAN}{'─'*60}{RESET}")

def ok(msg: str) -> None:
    print(f"{GREEN}✓ {msg}{RESET}")

def err(msg: str) -> None:
    print(f"{RED}✗ {msg}{RESET}")

def warn(msg: str) -> None:
    print(f"{YELLOW}⚠ {msg}{RESET}")


class McpClient:
    def __init__(self, path: str, timeout: int) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        try:
            self.sock.connect(path)
        except FileNotFoundError:
            print(f"{RED}Socket not found: {path}{RESET}")
            print("Start the browser binary first:")
            print(f"  OPENAGENT_SOCKET_PATH={path} \\")
            print(f"  SEARXNG_URL=http://100.96.81.109:8888 \\")
            print(f"  ./target/debug/browser")
            sys.exit(1)
        except ConnectionRefusedError:
            print(f"{RED}Connection refused: {path}{RESET}")
            sys.exit(1)

    def send(self, frame: dict) -> dict:
        self.sock.sendall((json.dumps(frame) + "\n").encode())
        data = b""
        while not data.endswith(b"\n"):
            chunk = self.sock.recv(4096)
            if not chunk:
                break
            data += chunk
        return json.loads(data)

    def close(self) -> None:
        self.sock.close()


def test_ping(client: McpClient) -> None:
    hdr("1 / 5  PING")
    resp = client.send({"id": "ping-1", "type": "ping"})
    if resp.get("type") == "pong":
        ok(f"pong — status={resp.get('status')}")
    else:
        err(f"unexpected response: {resp}")


def test_tools_list(client: McpClient) -> None:
    hdr("2 / 5  TOOLS.LIST")
    resp = client.send({"id": "list-1", "type": "tools.list"})
    tools = resp.get("tools", [])
    if tools:
        ok(f"{len(tools)} tool(s) registered")
        for t in tools:
            print(f"   • {BOLD}{t['name']}{RESET}")
            desc = t.get("description", "")
            print(f"     {textwrap.shorten(desc, width=72, placeholder='...')}")
    else:
        err(f"no tools returned: {resp}")


def test_fetch(client: McpClient, url: str, label: str = "WEB.FETCH") -> Optional[str]:
    hdr(f"3 / 5  {label}  →  {url}")
    print(f"Fetching … (timeout {TIMEOUT}s)")
    resp = client.send({
        "id": "fetch-1",
        "type": "tool.call",
        "tool": "web.fetch",
        "params": {"url": url},
    })
    if resp.get("error"):
        err(f"tool error: {resp['error']}")
        return None
    result = resp.get("result", "")
    chars = len(result)
    ok(f"{chars} chars returned")
    if result.strip():
        preview = result.strip()[:600].replace("\n", " ")
        print(f"\n{YELLOW}Preview:{RESET}")
        print(textwrap.fill(preview, width=72, initial_indent="  ", subsequent_indent="  "))
    else:
        warn("empty result — page may require JavaScript or TLS failed")
    return result


def test_search(client: McpClient, query: str) -> Optional[str]:
    hdr(f"4 / 5  WEB.SEARCH  →  \"{query}\"")
    print(f"Searching … (timeout {TIMEOUT}s)")
    resp = client.send({
        "id": "search-1",
        "type": "tool.call",
        "tool": "web.search",
        "params": {"query": query, "max_results": 3},
    })
    if resp.get("error"):
        err(f"tool error: {resp['error']}")
        warn("SearXNG may not be reachable from this machine")
        base = os.getenv("SEARXNG_URL", "http://100.96.81.109:8888")
        warn(f'Test connectivity: curl "{base}/search?q=test&format=json"')
        return None
    result = resp.get("result", "")
    try:
        hits = json.loads(result)
        ok(f"{len(hits)} result(s)")
        for i, h in enumerate(hits, 1):
            print(f"\n  {BOLD}[{i}]{RESET} {h.get('title','(no title)')}")
            print(f"       {CYAN}{h.get('url','')}{RESET}")
            snippet = h.get("snippet", "")
            if snippet:
                print(f"       {textwrap.shorten(snippet, width=68, placeholder='...')}")
        return hits[0]["url"] if hits else None
    except json.JSONDecodeError:
        warn(f"could not parse result as JSON: {result[:200]}")
        return None


def test_search_then_fetch(client: McpClient, query: str) -> None:
    """Full two-turn workflow: search → pick top result → fetch and extract."""
    hdr(f"5 / 5  SEARCH → FETCH  (two-turn workflow)")
    print(f"Query: {BOLD}{query}{RESET}")
    print(f"Searching … (timeout {TIMEOUT}s)")

    resp = client.send({
        "id": "sf-search",
        "type": "tool.call",
        "tool": "web.search",
        "params": {"query": query, "max_results": 3},
    })
    if resp.get("error"):
        err(f"search failed: {resp['error']}")
        return

    try:
        hits = json.loads(resp.get("result", "[]"))
    except json.JSONDecodeError:
        err("could not parse search results as JSON")
        return

    if not hits:
        warn("search returned no results")
        return

    ok(f"{len(hits)} result(s) from search")
    top = hits[0]
    print(f"\n  Picked top result:")
    print(f"  {BOLD}{top.get('title', '(no title)')}{RESET}")
    print(f"  {CYAN}{top['url']}{RESET}")

    print(f"\nFetching … (timeout {TIMEOUT}s)")
    resp2 = client.send({
        "id": "sf-fetch",
        "type": "tool.call",
        "tool": "web.fetch",
        "params": {"url": top["url"]},
    })
    if resp2.get("error"):
        err(f"fetch failed: {resp2['error']}")
        return

    content = resp2.get("result", "")
    if not content.strip():
        warn("empty content — page may require JavaScript")
        return

    ok(f"{len(content)} chars of Markdown extracted")
    lines = [l for l in content.splitlines() if l.strip()]
    print(f"\n{YELLOW}Extracted content (first 20 lines):{RESET}")
    for line in lines[:20]:
        print(f"  {line}")
    if len(lines) > 20:
        print(f"  {YELLOW}… {len(lines) - 20} more lines{RESET}")


def main() -> None:
    print(f"\n{BOLD}Browser service — MCP-lite manual test{RESET}")
    print(f"  Socket : {SOCK_PATH}")
    print(f"  Query  : {TEST_QUERY}")
    print(f"  URL    : {TEST_URL}")
    print(f"  Timeout: {TIMEOUT}s")

    client = McpClient(SOCK_PATH, TIMEOUT)

    try:
        test_ping(client)
        test_tools_list(client)
        test_fetch(client, TEST_URL)
        test_search(client, TEST_QUERY)
        test_search_then_fetch(client, TEST_QUERY)
    except TimeoutError:
        err(f"socket timed out after {TIMEOUT}s — increase TIMEOUT env var")
    except KeyboardInterrupt:
        print("\ninterrupted")
    finally:
        client.close()

    print(f"\n{BOLD}Done.{RESET}\n")


if __name__ == "__main__":
    main()
