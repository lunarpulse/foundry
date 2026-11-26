# W31 host measurement

Run these blocks in order in one Bash shell on the parent host. The sole image operand is the local product ID from [the B5 host record](base-image.md). The [frozen W31 assertions](w31-oracle.md) govern interpretation. This procedure exercises the producer handler used by product serve, on two fresh private v4 fixtures. Each invocation creates a real claim, invokes the Rust ProductLauncher, starts its AF_UNIX listener, and runs the binary's embedded dispatch, create, fingerprint, start and owned-ID cleanup code. No provider request is sent. The test binary paces the stage input and observes the verified child. The normal binary has no witness switch.

## 1. Establish the candidate and private output directory

```bash
set -euo pipefail
umask 077
cd /home/cosmo/worktrees/foundry-w3-integration
W=docs/acceptance/w3/w31
H=docs/acceptance/w3/w31-host
export FOUNDRY_W3_IMAGE=sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60
mkdir "$H"
git rev-parse HEAD > "$H/base-commit.txt"
test "$(cat "$H/base-commit.txt")" = 7ce910d47e563d0ae1fb4cfc2b56f7fe96cbc9ee
sha256sum -c docs/acceptance/w3/w31-freeze.sha256 "$W/source.sha256" > "$H/source-check.txt"
sha256sum "$W/source.sha256" > "$H/source-manifest.sha256"
git diff --no-ext-diff --no-textconv --no-color --binary --full-index HEAD -- Cargo.toml Cargo.lock crates deploy scripts > "$H/tracked.patch"
cmp docs/acceptance/w3/w31-local/tracked.patch "$H/tracked.patch"
```

Use a fresh evidence directory. A stopped run's partial receipts belong to that attempt. Expect every named file to match, including the four frozen deploy inputs, host-side launcher and fixture code. The SHA-256 of the source manifest is the join's source fingerprint.

## 2. Confirm the local daemon and selected image

```bash
D=(/usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent FOUNDRY_W3_IMAGE="$FOUNDRY_W3_IMAGE" /usr/bin/docker --host unix:///var/run/docker.sock)
"${D[@]}" version --format '{{.Server.Version}}' > "$H/server-version.txt" 2>"$H/server-version.stderr"
"${D[@]}" image inspect --format '{"id":{{json .Id}},"user":{{json .Config.User}},"entrypoint":{{json .Config.Entrypoint}},"cmd":{{json .Config.Cmd}},"workdir":{{json .Config.WorkingDir}},"layers":{{json .RootFS.Layers}}}' "$FOUNDRY_W3_IMAGE" > "$H/image.json" 2>"$H/image.stderr"
/usr/bin/python3 -I -c 'import json,os,sys; d=json.load(open(sys.argv[1])); assert (d["id"],d["user"],d["entrypoint"],d["cmd"],d["workdir"]) == (os.environ["FOUNDRY_W3_IMAGE"],"65534:65534",["/usr/bin/python3","-I","/opt/foundry/worker.py"],None,"/work"); print("selected local product image verified")' "$H/image.json" > "$H/image-check.txt"
```

The parent previously observed server 29.4.3. Record the current server version. An unavailable ID or mismatched profile stops the run. The product OCI archive has separate manifest and config digests. Neither is an image operand here.

## 3. Build the controller and fixture offline

```bash
/usr/bin/env -i HOME="$HOME" PATH="$HOME/.cargo/bin:/usr/bin:/bin" FOUNDRY_W3_IMAGE="$FOUNDRY_W3_IMAGE" \
  /bin/bash "$W/cargo" "$H/build.log" -p foundry --features p2-test-support,product-pack-pins --bin foundry --test w2 --no-run --message-format=json
mkdir "$H/I01"
/usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent FOUNDRY_W3_IMAGE="$FOUNDRY_W3_IMAGE" \
  /usr/bin/python3 -I "$W/offline.py" "$H/I01/overrides.jsonl" "$PWD/target/w31/debug/foundry" > "$H/components.log" 2>&1
```

The wrapper makes a mode-0700 temporary directory for this Cargo invocation, checks rustfmt and git diff, then runs locked, offline, single-job Cargo with incremental compilation disabled. Its exact arguments and exit are in `build.log`. The component check writes `I01/render-static.json` and 16 early refusals to `I01/overrides.jsonl`. Expect zero command invocations on those refusals. The static render's example socket differs from each measured stage's socket.

## 4. Observe the default and timeout cases

```bash
/usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent FOUNDRY_W3_IMAGE="$FOUNDRY_W3_IMAGE" \
  /usr/bin/python3 -I "$W/host_case.py" "$H" default
/usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent FOUNDRY_W3_IMAGE="$FOUNDRY_W3_IMAGE" \
  /usr/bin/python3 -I "$W/host_case.py" "$H" timeout
```

Each command creates exactly one case directory, `docs/acceptance/w3/w31-host/default/` or `docs/acceptance/w3/w31-host/timeout/`. It reads the executables from Cargo's build messages and checks both exact test names. `invocation.json` records the executable digests, argv and complete outer test environment. `test.log` captures stdout, stderr and exit. The outer argv is the compiled w2 test followed by `--exact w31_private_fixture --ignored --nocapture`. The inner argv is the compiled foundry unit test followed by `--exact isolation::dispatch::w31::owned_producer --ignored --nocapture`.

The inner controller clears its environment. It sets PATH to /usr/bin:/bin, HOME and TMPDIR to the new private fixture, W31_FIXTURE to that fixture, W31_WITNESS_DIR to the case directory, FOUNDRY_W3_IMAGE to the exact ID above, FOUNDRY_PRODUCT_REVIEW_SOURCE to b2-w1-source, FOUNDRY_REPO_PATH to the fixture's repo, FOUNDRY_REVIEW_PACK to its verify-pack, and FOUNDRY_PRODUCT_{ARTIFACT,SESSION,CLAIM}_ROOT to its artifacts, sessions and claims directories. The default unsets FOUNDRY_DIE_TIMEOUT (600 seconds, clamped by the task's 120-second lease). The timeout case sets it to 20 seconds. `controller-config.json` records the actual operands. The two-order fixture uses inert Git data, zero-cost authority and a base containing a tracked die-output.json. The frozen worker checks this name before it can execute Die. The witness independently checks the Git blob before dispatch.

Both cases use the single container created by that stage. While its unchanged PID 1 waits for EOF, the host runs an observer with the container's configured user. The observer reads its own and PID 1's proc files and exercises the filesystem. In the default case the controller delivers the guarded stage input. Expect worker rejection, removal and controller state uncertain. In the timeout case the witness holds the input incomplete across the actual launch deadline. A second process in that same container writes sequential lines to its tmpfs and mirrors them to a retained stdout stream. Expect growth immediately before the deadline, a real timeout, removal, a specific no-such-container response for that ID and two unchanged post-reap samples. The writer's process status immediately before removal is retained. These samples concern the retained output of that writer.

### Docker argv and custody of the outputs

The two private producer preflights each use `/usr/bin/timeout 5 /usr/bin/docker --host unix:///var/run/docker.sock image inspect --format '{{.Id}}' IMAGE` with HOME=/nonexistent and PATH=/usr/bin:/bin. The selected image in the stage receipt must agree. For the launcher and its observers the fixed executable prefix is `/usr/bin/docker --host unix:///var/run/docker.sock`. The complete client environment is PATH=/usr/bin:/bin, HOME=/nonexistent, GIT_CONFIG_NOSYSTEM=1, GIT_CONFIG_GLOBAL=/dev/null, GIT_NO_LAZY_FETCH=1, GIT_NO_REPLACE_OBJECTS=1 and GIT_ALLOW_PROTOCOL set to the empty string. The image travels by argv. Below, OWNER, SOCKET and CID are generated/verified by the launcher. Do not choose these manually. The parent invokes the two host commands above once each. The following internal commands are shown for audit.

| Internal argv after the prefix | Evidence within that case |
|---|---|
| `create --label foundry.w3.owner=OWNER --name foundry-w3-OWNER --env LANG --env GPG_KEY --env PYTHON_VERSION --env PYTHON_SHA256 --pull=never --interactive --read-only --network=none --user 65534:65534 --cap-drop=ALL --security-opt=no-new-privileges:true --no-healthcheck --ipc=private --log-driver=none --pids-limit=64 --memory=512m --workdir=/work --tmpfs /work:rw,nosuid,nodev,size=256m,uid=65534,gid=65534,mode=0700 --mount type=bind,source=SOCKET,target=/run/foundry-model.sock,readonly --entrypoint /usr/bin/python3 IMAGE -I /opt/foundry/worker.py` | `docker-argv.jsonl`, `render.json`, `binding.json`, `runtime-stage.json`. The four bare env operands request removal of inherited Python image metadata. |
| `container inspect CID`, then `cp CID:/opt/foundry/worker.py -` and `cp CID:/opt/foundry/model-client -` | `I06/runtime.json` retains an allowlisted inspect projection. `runtime-stage.json` retains verified file digests. The trace records the order of the two file reads from the embedded map. Inspect/profile or file disagreement must stop before start. Actual env values and tar streams are omitted. |
| `start --attach --interactive CID`. On a zero client exit the launcher also issues `inspect --format '{{json .State}}' CID`. The observer uses `container inspect --format '{{json .State}}' CID` to wait for running state and again before removal. | `docker-argv.jsonl`, `runtime-stage.json`, `I01/overrides.jsonl`, `I06/reap.json`. Stage stdin remains private. |
| `exec -i CID /usr/bin/python3 -I -c CHILD_SOURCE` with the exact UTF-8 contents of w31/child.py as one argv operand and fixture paths as JSON stdin | `I06/proc-self.json`, `I02/child-open.jsonl`, `I03/workspaces.jsonl`, `I03/serve-audit.jsonl`. The controller brackets the access window with `I02/controller-before.json` and `I02/controller-after.json`, and writes `I03/canaries.json`. |
| Timeout only, `exec CID /usr/bin/python3 -I -u -c WRITER` with the exact WRITER string in w31/witness.py | `I06/heartbeat` is actual stdout. `I06/active-hashes.jsonl` retains the two pre-deadline samples. |
| `rm --force --volumes CID`, then `container inspect CID`. On loss of create ownership information the production recovery lookup is `inspect foundry-w3-OWNER`. | `runtime-stage.json`, `I06/reap.json` and, for timeout, `I06/output-hashes.jsonl`. The post-reap check requires the daemon's specific absence diagnostic. An unknown cleanup result leaves the row open. |
| Product controller response and fixture HTTP counter | `controller-result.json` must carry the same order, operation, epoch and fence, state uncertain and no result or artifact. `serve-count.json` must count the single host control. `observed.json` records the case join. |

The v4 product store uses DELETE journaling. A separate live fixture SQLite database supplies actual WAL and SHM files. Each must exist on the host across the child attempts. The same-order watch canary is the snapshot actually read by product-intake. The other order has a real Git worktree at its authorized receipt base. Workspace and protected-open checks run in the same-ID observer. The privileged HTTP surface is the real product router on a fixture-only AF_UNIX listener. Its unauthenticated host control to GET /api/product/sources must return 401. The child must fail to connect and the route counter must stay at one. Ordinary serve's TCP transport and a live watch daemon are outside this observation.

## 5. Verify the joins and seal the observations

```bash
/usr/bin/python3 -I "$W/verify.py" "$H" > "$H/verification.log" 2>&1
sha256sum -c docs/acceptance/w3/w31-freeze.sha256 "$W/source.sha256" > "$H/source-check-after.txt"
find "$H" -type f ! -name SHA256SUMS -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > "$H/SHA256SUMS"
sha256sum --quiet -c "$H/SHA256SUMS"
```

Expect two distinct IDs and render hashes, live observations joined to their respective reservations and no mismatched comparison. A failure or absent file is a partial measurement. Preserve the case directory and test log for adjudication. I04 and I05 require W32 evidence. W23 remains gated on the same-boundary measurements and separate task approval.
