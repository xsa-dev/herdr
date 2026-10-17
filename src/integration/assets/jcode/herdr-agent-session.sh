#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=jcode
# HERDR_INTEGRATION_VERSION=1

[ "${1:-}" = "session" ] || exit 0
[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 -c '
import json
import os
import random
import socket
import subprocess
import time

try:
    pane_id = os.environ.get("HERDR_PANE_ID")
    socket_path = os.environ.get("HERDR_SOCKET_PATH")
    if not pane_id or not socket_path:
        raise SystemExit(0)

    payload = {}
    raw_payload = os.environ.get("JCODE_HOOK_PAYLOAD", "")
    if raw_payload.strip():
        try:
            parsed = json.loads(raw_payload)
            if isinstance(parsed, dict):
                payload = parsed
        except Exception:
            payload = {}

    event = os.environ.get("JCODE_HOOK_EVENT") or payload.get("event")
    if event not in (None, "", "session_start", "SessionStart"):
        raise SystemExit(0)

    session_id = os.environ.get("JCODE_HOOK_SESSION_ID") or payload.get("session_id")
    if not isinstance(session_id, str) or not session_id.strip():
        raise SystemExit(0)

    raw_source = os.environ.get("JCODE_HOOK_SOURCE") or payload.get("source")
    if raw_source in ("create", "attach", "startup"):
        session_start_source = "startup"
    elif raw_source == "resume":
        session_start_source = "resume"
    else:
        session_start_source = None

    seq = time.time_ns()
    herdr_bin = os.environ.get("HERDR_BIN_PATH")
    if herdr_bin and os.path.isfile(herdr_bin) and os.access(herdr_bin, os.X_OK):
        args = [
            herdr_bin,
            "pane",
            "report-agent-session",
            pane_id,
            "--source",
            "herdr:jcode",
            "--agent",
            "jcode",
            "--agent-session-id",
            session_id,
            "--seq",
            str(seq),
        ]
        if session_start_source:
            args.extend(["--session-start-source", session_start_source])
        subprocess.run(
            args,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=1,
            check=False,
        )
        raise SystemExit(0)

    params = {
        "pane_id": pane_id,
        "source": "herdr:jcode",
        "agent": "jcode",
        "seq": seq,
        "agent_session_id": session_id,
    }
    if session_start_source:
        params["session_start_source"] = session_start_source
    request = {
        "id": f"herdr:jcode:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": "pane.report_agent_session",
        "params": params,
    }
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
' 2>/dev/null || true
