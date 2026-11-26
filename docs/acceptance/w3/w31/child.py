"""Executed by docker exec in the ID verified by the product launcher."""
import errno
import json
import os
from pathlib import Path
import socket
import sys


def open_results(path):
    rows = []
    # No create, truncate or write. A successful writable fd disproves denial.
    for op, flags in (("read", os.O_RDONLY), ("write", os.O_WRONLY)):
        try:
            fd = os.open(path, flags | os.O_NONBLOCK)
        except OSError as error:
            rows.append({"path": path, "op": op, "errno": errno.errorcode[error.errno]})
        else:
            os.close(fd)
            rows.append({"path": path, "op": op, "opened": True})
    return rows


def observed(read):
    try:
        return read()
    except OSError as error:
        return {"errno": errno.errorcode.get(error.errno, "UNKNOWN")}


def proc(who):
    path = Path("/proc") / who
    def status():
        keys = {"Uid", "Gid", "CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb", "NoNewPrivs"}
        values = dict(line.split(":", 1) for line in (path / "status").read_text().splitlines())
        return {k: values[k].split() for k in sorted(keys) if k in values}

    def env_names():
        # Record names only. The stage input is still on the controller side.
        return sorted(v.split(b"=", 1)[0].decode() for v in (path / "environ").read_bytes().split(b"\0") if v)

    return {"env_keys": observed(env_names), "status": observed(status),
            "cmdline": observed(lambda: (path / "cmdline").read_bytes().decode().split("\0")[:3]) if who == "1" else None,
            "mountinfo": observed(lambda: (path / "mountinfo").read_text().splitlines()),
            "net_devices": observed(lambda: [line.split(":")[0].strip() for line in (path / "net/dev").read_text().splitlines()[2:]]),
            "namespace": {k: observed(lambda: os.readlink(path / "ns" / k)) for k in ("mnt", "net", "pid", "user")}}


def observe(paths):
    def workspace():
        own = Path("/work/w31-canary")
        with own.open("xb") as f:
            f.write(b"W31 workspace\n")
        return {"path": str(own), "reread": own.read_bytes().hex()}
    local = observed(workspace)
    serve = paths["serve"]
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(1)
        try:
            client.connect(serve)
        except OSError as error:
            access = {"errno": errno.errorcode.get(error.errno, "UNKNOWN")}
        else:
            access = {"connected": True}
    return {"worker": proc("1"), "observer": proc("self"), "own": local,
            "serve": {"path": serve, **access},
            "opens": {k: open_results(v) for k, v in paths.items() if k != "serve"}}


if __name__ == "__main__":
    print(json.dumps(observe(json.load(sys.stdin))))
