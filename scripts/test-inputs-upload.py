#!/usr/bin/env python3
"""
Verifies the file-upload roundtrip through the server's chunked-upload
contract. Exercises the same surface the UI's InputsTab uses.

Headers contract (crates/server/src/chat_routes/inputs/upload.rs):
  Upload-Token: <alnum/-, ≤64 chars>
  Upload-Filename: <original-name>  (required on first chunk)
  Content-Range: bytes <start>-<end>/<total>
  Upload-Sha256: <full-file sha256>  (required on final chunk)

Finalize is a path-param POST: /inputs/upload/:upload_token/finalize
"""

import hashlib
import json
import os
import secrets
import sys
import urllib.error
import urllib.request

SERVER = os.environ.get("SWFC_SERVER_URL", "http://127.0.0.1:3000")
PREFIX = "/api/v1/chat"


def http(method: str, path: str, *, body=None, headers=None, timeout: int = 30):
    url = SERVER + path
    data = body if isinstance(body, (bytes, bytearray)) else (
        json.dumps(body).encode() if body is not None else None
    )
    req = urllib.request.Request(url, data=data, method=method)
    if headers:
        for k, v in headers.items():
            req.add_header(k, v)
    if body is not None and not isinstance(body, (bytes, bytearray)):
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            try:
                return r.status, json.loads(raw) if raw else None
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


def main() -> int:
    code, body = http("POST", f"{PREFIX}/session", body={})
    if code != 200:
        print(f"FAIL session: {code} {body}")
        return 1
    sid = body["session_id"]
    print(f"session: {sid}")

    payload = b"sample_id,group\n1,A\n2,A\n3,B\n4,B\n"
    sha256 = hashlib.sha256(payload).hexdigest()
    upload_token = secrets.token_hex(16)

    code, body = http(
        "POST",
        f"{PREFIX}/session/{sid}/inputs/upload",
        body=payload,
        headers={
            "Content-Type": "application/octet-stream",
            "Upload-Token": upload_token,
            "Upload-Filename": "tiny.csv",
            "Content-Range": f"bytes 0-{len(payload)-1}/{len(payload)}",
            "Upload-Sha256": sha256,
        },
    )
    if code not in (200, 201, 204):
        print(f"FAIL chunk: {code} {body}")
        return 1
    body_str = body if isinstance(body, str) else json.dumps(body, default=str)
    print(f"chunk: {code} body={body_str[:160]}")

    code, body = http(
        "POST",
        f"{PREFIX}/session/{sid}/inputs/upload/{upload_token}/finalize",
    )
    if code not in (200, 201, 204):
        print(f"FAIL finalize: {code} {body}")
        return 1
    body_str = body if isinstance(body, str) else json.dumps(body, default=str)
    print(f"finalize: {code} body={body_str[:200]}")

    code, body = http("GET", f"{PREFIX}/session/{sid}/inputs")
    if code != 200:
        print(f"FAIL list: {code} {body}")
        return 1
    inputs = body if isinstance(body, list) else (body.get("inputs") or [])
    if not inputs:
        print(f"FAIL: no inputs registered after upload+finalize: {body}")
        return 1

    found = any(
        any(
            f.get("name") == "tiny.csv"
            or f.get("filename") == "tiny.csv"
            or f.get("relpath") == "tiny.csv"
            for f in (inp.get("files") or [])
        )
        for inp in inputs
    )
    if not found:
        print(f"FAIL: tiny.csv not in registered inputs: {json.dumps(inputs, default=str)[:400]}")
        return 1

    print("PASS upload roundtrip")
    return 0


if __name__ == "__main__":
    sys.exit(main())
