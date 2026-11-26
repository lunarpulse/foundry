#!/usr/bin/python3 -I
"""Read one provider body on stdin, forward it under this stage's capability."""
import json
import os
import socket
import sys


def main():
    body = sys.stdin.buffer.read(65537)
    if len(body) > 65536:
        raise ValueError("request size")
    request = {"run": os.environ["FOUNDRY_PROXY_RUN"],
               "provider": os.environ["FOUNDRY_PROXY_PROVIDER"],
               "budget_scope": json.loads(os.environ["FOUNDRY_PROXY_BUDGET_SCOPE"]),
               "body": body.decode()}
    wire = json.dumps({"token": os.environ["FOUNDRY_PROXY_TOKEN"], "request": request}).encode()
    if len(wire) > 256 << 10:
        raise ValueError("frame size")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(2)
        client.connect("/run/foundry-model.sock")
        client.sendall(len(wire).to_bytes(4, "big") + wire)
        with client.makefile("rb") as reader:
            header = reader.read(4)
            size = int.from_bytes(header, "big")
            if len(header) != 4 or size > 256 << 10:
                raise ValueError("bad response")
            payload = reader.read(size)
            if len(payload) != size:
                raise ValueError("partial response")
            response = json.loads(payload)
    if any(response.get(k) != request[k] for k in ("run", "provider", "budget_scope")):
        raise ValueError("request denied")
    sys.stdout.buffer.write(response["body"].encode())


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.exit("proxy request denied")
