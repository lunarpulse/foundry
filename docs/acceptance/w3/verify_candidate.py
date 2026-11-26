"""Reconstruct the uncommitted W30b source delta without touching a Git index."""
from pathlib import Path
import hashlib
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "docs/acceptance/w3"
BASE = "a4e07fd56646503fd06ba5b846130a1d2189c9f0"


def git(*args, cwd=ROOT):
    return subprocess.check_output(["git", *args], cwd=cwd)


assert git("rev-parse", "HEAD").decode().strip() == BASE
scope = ["crates", "deploy", "scripts"]
tracked = git("diff","--name-only",BASE,"--",*scope).decode().splitlines()
added = git("ls-files","--others","--exclude-standard","--",*scope).decode().splitlines()
files = sorted(tracked + added)
assert files and not set(tracked).intersection(added)
patch = git("diff","--no-ext-diff","--binary",BASE,"--",*tracked)
for name in added:
    delta = subprocess.run(["git","diff","--no-ext-diff","--binary","--no-index","--","/dev/null",name],
                           cwd=ROOT, stdout=subprocess.PIPE)
    assert delta.returncode == 1
    patch += delta.stdout
(OUT/"candidate.patch").write_bytes(patch)
(OUT/"candidate.stat").write_bytes(git("apply", "--stat", str(OUT/"candidate.patch")))
with tempfile.TemporaryDirectory(prefix="w3-patch-") as tmp:
    for name in tracked:
        path = Path(tmp)/name
        path.parent.mkdir(parents=True,exist_ok=True)
        path.write_bytes(git("show",BASE+":"+name))
        mode = git("ls-tree",BASE,"--",name).split()[0]
        path.chmod(int(mode,8) & 0o777)
    for opts in (["--check"], []):
        git("apply","--no-index","--whitespace=error-all",*opts,str(OUT/"candidate.patch"),cwd=tmp)
    sums=[]
    for name in files:
        assert (Path(tmp)/name).read_bytes() == (ROOT/name).read_bytes()
        assert ((Path(tmp)/name).stat().st_mode & 0o111) == ((ROOT/name).stat().st_mode & 0o111)
        sums.append(hashlib.sha256((ROOT/name).read_bytes()).hexdigest()+"  "+name)
    (OUT/"source.sha256").write_text("\n".join(sums)+"\n")
print("base="+BASE)
print(f"source paths={len(files)}, tracked={len(tracked)}, new={len(added)}")
print("fresh base files + candidate.patch reproduce every source byte and executable bit")
print("patch_sha256="+hashlib.sha256(patch).hexdigest())
