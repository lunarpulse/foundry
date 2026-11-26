#!/usr/bin/env python3
"""Render the product child profile offline. Reject extra Compose settings."""
import json
from pathlib import Path
import re

IMAGE = "${FOUNDRY_W3_IMAGE:?local image ID required}"
SOCKET = "${FOUNDRY_W3_SOCKET:?launcher-owned socket required}"
TMPFS = "/work:rw,nosuid,nodev,size=256m,uid=65534,gid=65534,mode=0700"
ENTRY = ["/usr/bin/python3", "-I", "/opt/foundry/worker.py"]


def render(profile, image, socket):
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
        raise ValueError("expected immutable local image ID")
    if not re.fullmatch(r"/[a-zA-Z0-9_./-]+/model\.sock", socket) or ".." in Path(socket).parts:
        raise ValueError("invalid socket operand")
    mount = {"type": "bind", "source": SOCKET, "target": "/run/foundry-model.sock",
             "read_only": True, "bind": {"create_host_path": False}}
    service = {"image": IMAGE, "pull_policy": "never", "user": "65534:65534",
               "read_only": True, "network_mode": "none", "cap_drop": ["ALL"],
               "security_opt": ["no-new-privileges:true"], "pids_limit": 64,
               "mem_limit": "512m", "tmpfs": [TMPFS], "working_dir": "/work",
               "stdin_open": True, "ipc": "private", "healthcheck": {"disable": True},
               "logging": {"driver": "none"}, "entrypoint": ENTRY, "volumes": [mount]}
    if json.dumps(profile, sort_keys=True) != json.dumps({"services": {"cast": service}}, sort_keys=True):
        raise ValueError("child profile differs from the supported boundary")
    service["image"] = image
    mount["source"] = socket
    return {"services": {"cast": service}}
