#!/usr/bin/env python3
r"""Add one Windows device to an existing AgentReins audit server.

Run on the audit server by an administrator. Never prints the device token.
The generated device.json must be transferred over a secure channel to the
Windows user's %LOCALAPPDATA%\AgentReins\CloudAudit directory.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import tempfile
from datetime import datetime, timezone
import uuid


def write_private_json(path: Path, value: dict) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o600)
        return Path(temporary)
    except BaseException:
        Path(temporary).unlink(missing_ok=True)
        raise


def enroll(server_config: Path, output: Path, endpoint: str, label: str) -> str:
    if not endpoint.startswith("https://") or not endpoint.endswith("/api/audit/ingest"):
        raise ValueError("endpoint must be the HTTPS audit ingest URL")
    if output.exists():
        raise FileExistsError(f"refusing to replace existing enrollment: {output}")
    data = json.loads(server_config.read_text(encoding="utf-8"))
    if not isinstance(data.get("devices"), dict) or not isinstance(data.get("admin"), dict):
        raise ValueError("not an existing AgentReins audit server configuration")
    device_id = str(uuid.uuid4())
    token = secrets.token_urlsafe(48)
    enabled_at = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    data["devices"][device_id] = {
        "label": label,
        "tokenHash": hashlib.sha256(token.encode()).hexdigest(),
        "enabledAt": enabled_at,
    }
    client = {
        "endpoint": endpoint,
        "deviceID": device_id,
        "token": token,
        "enabledAt": enabled_at,
        "uploadEnabled": False,
    }
    staged_client = write_private_json(output, client)
    staged_server = None
    try:
        staged_server = write_private_json(server_config, data)
        backup = server_config.with_name(
            f"{server_config.name}.backup-{datetime.now(timezone.utc):%Y%m%dT%H%M%SZ}"
        )
        if backup.exists():
            raise FileExistsError(f"backup already exists: {backup}")
        shutil.copy2(server_config, backup)
        os.chmod(backup, 0o600)
        os.replace(staged_server, server_config)
        os.replace(staged_client, output)
    finally:
        if staged_server is not None:
            staged_server.unlink(missing_ok=True)
        staged_client.unlink(missing_ok=True)
    return device_id


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--label", required=True)
    options = parser.parse_args()
    device_id = enroll(options.server_config, options.output, options.endpoint, options.label)
    print(f"Enrolled device {device_id}; restart the audit server to load it.")
    print(f"Transfer the private client configuration from {options.output} securely.")


if __name__ == "__main__":
    main()
