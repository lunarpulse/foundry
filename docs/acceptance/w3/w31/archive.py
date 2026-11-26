"""Read retained OCI bytes without importing, mounting or executing the image."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[4]


def sha(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def inspect(path):
    with tarfile.open(path, "r:") as archive:
        def blob(desc):
            data = archive.extractfile("blobs/" + desc["digest"].replace(":", "/")).read()
            assert len(data) == desc["size"] and sha(data) == desc["digest"]
            return data
        index = archive.extractfile("index.json").read()
        desc, = json.loads(index)["manifests"]
        manifest = json.loads(blob(desc))
        cfg = json.loads(blob(manifest["config"]))
        for layer in manifest["layers"]:
            entry = archive.getmember("blobs/" + layer["digest"].replace(":", "/"))
            assert entry.size == layer["size"]
            assert "sha256:" + hashlib.file_digest(archive.extractfile(entry), "sha256").hexdigest() == layer["digest"]
        files = {}
        if path.name == "product-oci.tar":
            # The two COPY layers and WORKDIR are the final three product layers.
            for layer, diff in zip(manifest["layers"][-3:], cfg["rootfs"]["diff_ids"][-3:]):
                data = gzip.decompress(blob(layer))
                assert sha(data) == diff
                with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as fs:
                    for entry in fs:
                        name = entry.name
                        if entry.isdir():
                            continue
                        assert entry.isreg() and name in ("opt/foundry/worker.py","opt/foundry/model-client")
                        code = fs.extractfile(entry).read()
                        local = ROOT / "deploy" / ("model-client.py" if name.endswith("client") else "worker.py")
                        assert code == local.read_bytes() and entry.mode == 0o555 and entry.uid == entry.gid == 0
                        files[name] = sha(code)
            assert len(files) == 2
        return {"archive":str(path),"index_sha256":sha(index),"manifest":desc["digest"],
                "config_digest":manifest["config"]["digest"],"layers_verified":len(manifest["layers"]),
                "diff_ids":cfg["rootfs"]["diff_ids"],"platform":desc["platform"],
                "image_config":{k:cfg["config"].get(k) for k in ("User","WorkingDir","Entrypoint","Cmd")},
                "env_keys":[v.split("=",1)[0] for v in cfg["config"]["Env"]],"copy_files":files}


if __name__ == "__main__":
    archive_dir = Path(sys.argv[1])
    base, product = (inspect(archive_dir / (n+"-oci.tar")) for n in ("base","product"))
    assert base["manifest"] == "sha256:c353734f0b9194bbb18395c7c17041ec1b47811cfd8647369b1ce61823af0289"
    assert product["diff_ids"][:-3] == base["diff_ids"]
    print(json.dumps({"base":base,"product":product},indent=2))
