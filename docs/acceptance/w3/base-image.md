# B5 관문 해소 — 승인된 digest 고정 base 이미지 확보 기록 (2026-11-26)

호스트에서 측정한 사실만 기록한다. 네트워크 사용은 사용자가 명시적으로 승인한
**단 한 번의 `docker pull`**뿐이다 (승인 Discord message id `1575335679360114849`,
base 로컬 빌드 승인 id `1575460787504414821`).

## 1. 유일한 네트워크 연산

```
docker pull python:3.12
→ Digest: sha256:9dd21fafb95870b99f9faa703a947d08feeb673048ad556e92ce503d0c086a95
```

정식 이미지가 buildpack-deps 기반이라 Git이 포함된다. 실측:
Python 3.12.16(`/usr/local/bin/python3`), git 2.47.3, curl 8.14.1, bash 5.2.37.
Docker Hub 익명 manifest metadata 조회가 빌드 해석 중 일어날 수 있으나
레이어 전송은 이 pull이 유일하다.

## 2. base 이미지 (Python + Git + 실제 Die)

빌드 컨텍스트: `/home/cosmo/.hermes/cache/scratch/w3-base-build` (스크래치 — 대장은
`/home/cosmo/foundry/.hermes/evidence/w30b-base-image-20261126/`에 보관).

```dockerfile
FROM python:3.12@sha256:9dd21fafb95870b99f9faa703a947d08feeb673048ad556e92ce503d0c086a95
COPY --chmod=0555 foundry-die /opt/foundry/die
COPY --chmod=0555 rustain /opt/foundry/bin/rustain
COPY --chmod=0555 rustain /die-home/.foundry/bin/rustain
```

입력 파일 SHA-256:
- `~/.foundry/bin/foundry-die` = `b91f8cc4c0bcda9592c2fd53c378d1dd4766ea64b1bfb4a7f633e848c58876ba`
- `~/.foundry/bin/rustain` = `6f8059718232653cdd8918bea4c2aa0121ce572263c8db931a893269f15e8b60`
  (rustain 0.1.2, build 2026-07-09, commit 1fc15e0, rust 1.91)

`/die-home/.foundry/bin/rustain`은 Die 스크립트가 `exec "$HOME/.foundry/bin/rustain"`
을 호출하는 경로 계약용 사본이다. base 이미지는 ENV를 오염하지 않는다
(Env는 python 베이스 기본값만). 실제 Die **실행**(die.env, cargo, 네트워크 clone)은
W23의 본래 목적이며 이 기록에 포함되지 않는다.

결과:
- `foundry-base:w3` image ID(=RepoDigests) = `sha256:0e9902aecfe4e59359719fbcb14692b87d4e63e05297d5e7485e54b25e8ca90f`
- OCI layout manifest digest = `sha256:c353734f0b9194bbb18395c7c17041ec1b47811cfd8647369b1ce61823af0289` (blob SHA-256 대조 일치)

## 3. 제품 이미지 (동결된 deploy/Dockerfile 사용)

```
docker build -f deploy/Dockerfile \
  --build-arg BASE_REPO=foundry-base \
  --build-arg BASE_SHA256=0e9902aecfe4e59359719fbcb14692b87d4e63e05297d5e7485e54b25e8ca90f \
  -t foundry-product:w3b5 deploy/
```

BuildKit은 `name@sha256:` 참조를 registry에서 먼저 해석하려 하므로 로컬 빌드 이미지는
먼저 `docker buildx build --output type=oci`로 OCI layout을 만들고 `docker load`한
뒤, image ID digest(`0e9902…`)를 `BASE_SHA256`으로 넘겨야 로컬 해석된다
(manifest digest `c3537…`로는 registry 조회 실패). classic builder
(`DOCKER_BUILDKIT=0`)는 로컬 digest 해석을 확인하는 진단용으로만 사용했다
(동결 Dockerfile의 `--chmod`는 BuildKit 전용).

- `foundry-product:w3b5` image ID = `sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60`
- Base 해석이 로컬임의 증거: `foundry-base`는 docker.io에 존재하지 않아 registry
  풀이면 반드시 실패한다. 빌드는 exit 0.

## 4. 제품 이미지 실측 지문

- Config.User = `65534:65534` (실측 `id` = uid=65534(nobody) gid=65534(nogroup))
- Entrypoint = `["/usr/bin/python3","-I","/opt/foundry/worker.py"]`, Cmd = null
- WorkingDir = `/work`
- 이미지 내 파일(전부 0555, root 소유): `/opt/foundry/{die,worker.py,model-client,bin/rustain}`, `/die-home/.foundry/bin/rustain`
- `/usr/bin/python3` = Python 3.13.5 (distro 트리시). `/usr/local/bin/python3` = 3.12.16.
  entrypoint는 distro Python에서 실행된다.
- git 2.47.3, Die 어댑터 스크립트·rustain 포함 (내용은 §2와 동일 바이트)

## 5. 오프라인 재현 절차

1. `docker load -i base-oci.tar` (또는 product-oci.tar를 load하면 제품 이미지까지 복원)
2. §3 명령 그대로 빌드 (네트워크 불필요)
3. `FOUNDRY_W3_IMAGE=sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60`

대장 보관: `/home/cosmo/foundry/.hermes/evidence/w30b-base-image-20261126/`
(0700 디렉터리, base-oci.tar·product-oci.tar 409MB씩, Dockerfile.base, 본 기록 사본).

## 6. W30b 오라클 B5 행에 대한 영향

B5의 동결 전제 “승인된 digest 고정 image operand 부재 — 외부 전제, 제품 결함 아님”은
해소되었다. 실제 런타임 지문 측정(render↔inspect 결합, 소유 자식 시작·회수,
UID/mount/cap/rootfs/network 관측)은 W31/W32 경계 실측의 지정 증거원
(`render.json`, `runtime-stage.json`, `I06/*`)을 통해 수행한다. 이 문서는
이미지 확보 사실과 그 지문만 기록하며, 인수 판정은 별도 adjudication이 한다.
