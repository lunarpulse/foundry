#!/usr/bin/env python3
"""Render the product child profile offline."""
import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "deploy"))
from profile import render

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", type=Path, default=Path(__file__).resolve().parents[1] / "deploy/compose.yaml")
    parser.add_argument("--image", required=True)
    parser.add_argument("--socket", required=True)
    args = parser.parse_args()
    try:
        print(json.dumps(render(json.loads(args.profile.read_bytes()), args.image, args.socket), indent=2))
    except (ValueError, OSError) as error:
        parser.exit(1, f"{error}\n")
