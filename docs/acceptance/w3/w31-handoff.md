# W31 handoff · 26 November 2026

**The W31 candidate is ready for parent-host measurement. I01, I02, I03 and I06 remain open.** The approved local product image resolves B5's external image prerequisite. This sandbox produced no product-container receipt. W32 owns I04 and I05. W23 remains behind same-boundary W31/W32 evidence and separate task approval. W33 owns aggregate adjudication. [Host commands and output paths](host-measure-w31.md) · [B5 provenance](base-image.md) · [Design](../../design/w3-isolation-boundary.md)

The row assertions were saved and hashed before the W31 edits and verifications. Worktree HEAD is 7ce910d47e563d0ae1fb4cfc2b56f7fe96cbc9ee. The frozen oracle SHA-256 is 507ede6fad6549b373117923c314c8daff2d5603253858fab4f94898a1a82764. The candidate source manifest SHA-256 is 9e05a2b619094b44bd431c7289fea7f163f4865fad6644d1d62bc3e6ab9b8251. Its entries include the untracked witness/test files, runbook and tracked source diff. All W31 edits remain uncommitted on w3-integration. [Oracle](w31-oracle.md) · [Freeze receipt](w31-freeze.sha256) · [Sources](w31/source.sha256) · [Final source check](w31-local/source-check.log)

| Row | Available evidence | Remaining host observation |
|---|---|---|
| I01 | Offline production render with the approved ID and a placeholder socket. Twelve weakened profiles, three invalid images and one unsafe socket operand were refused before the command port. The original stage/Git and ownership-control regression also ran with a recording client. | Join each actual render and start to the selected image, inspect, controls, file hashes and removal receipt. The host exercises the default and one 20-second override, with distinct fixture roots, sockets and claims. No other image or unexercised setting inherits this evidence. [Static render](w31-local/I01/render-static.json) · [Early refusals](w31-local/I01/overrides.jsonl) |
| I02 | The local fixture builds a private v4 store, owner/service credentials and two authorized orders. | Same-ID opens, physical digests and logical DB row digest before/after. The product store uses DELETE journaling. A second live SQLite fixture supplies the actual WAL and SHM specimens. [Fixture run](w31-local/fixture.log) |
| I03 | The private fixture reuses the input file actually read by product-intake for the subject and creates a Git worktree at the other order's authorized base. | Own-workspace write/reread, denied access to both host paths, unchanged canaries and privileged-route audit. The host fixture serves the real product HTTP router over AF_UNIX. Ordinary TCP serve and a running watch daemon remain unexercised by this procedure. |
| I04/I05 | The earlier parent-host B3 component observation retains its original scope. | **Deferred to W32.** Same-child authenticated forwarding, binding denials, egress cases and upstream/charge/result deltas require their designated sources. [Earlier adjudication](adjudication.md) |
| I06 | The local recorder/reader rehearsal exercised a real subprocess deadline with a simulated container transport. It detected a swapped fence, a changed post-reap stream and an invalid denial errno. All synthetic row files were temporary. | Actual inspect and PID 1/observer fingerprints, active output close to the deadline, exact-ID absence and two post-reap hashes. The output belongs to a writer in the same container. Its tmpfs stream is mirrored to host stdout for observation after removal. [Rehearsal log](w31-local/rehearsal.log) |

## Candidate and image

The approved operand is sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60. A local read of the retained OCI archives verified the compressed layer digests, the base manifest, the shared base rootfs prefix and the final three product layer diff IDs. The archived worker and model client matched the worktree byte-for-byte with mode 0555 and root ownership. The archive config carries PATH, LANG, GPG_KEY, PYTHON_VERSION and PYTHON_SHA256. The OCI manifest/config digests and the parent's Docker ID are recorded at their respective layers. [Archive observations](w31-local/archives.json) · [Reader](w31/archive.py)

The launcher requests removal of the four Python metadata values using its clean Docker client environment. It verifies stdin/TTY settings and rejects unexpected environment assignments before start. Runtime receipts persist the owned ID, start request, available exit state, deadline observation and confirmed removal. The witness wraps the existing stdin writer only in the Rust test build. The approved worker, client, Dockerfile and Compose template keep their W30b bytes. [Launcher](../../../deploy/launcher.py) · [Rust entry](../../../crates/foundry/src/isolation.rs) · [Tracked diff](w31-local/tracked.patch)

The host test enters the real authenticated execution handler in process. Its claim and producer operation are created by the product store. The recorded join preserves the claim epoch and fence separately. On this producer path the execution intent uses the fence as its epoch and the producer operation key as its run. File and socket probes execute in an additional process in the verified container. Proc evidence identifies both that process and the waiting worker PID 1. The handler should return uncertain across both inert runs. There is no authorized output to collect. [Host fixture](../../../crates/foundry/src/isolation/w31_test.rs) · [Recorder](w31/witness.py) · [Independent reader](w31/verify.py)

## Sandbox verification

All Cargo invocations used the private-TMPDIR wrapper. Each invocation logged rustfmt and git diff checks before locked, offline single-job Cargo, with CARGO_BUILD_JOBS=1 and CARGO_INCREMENTAL=0. Counts below belong to individual invocations.

| Invocation | Result | Log |
|---|---|---|
| Host fixture and test controller, pin and test-support features | Built both exact ignored host entry points, exit 0 | [Build messages](w31-local/build.log) |
| W31 fixture-only path | 1 passed, 4 filtered, exit 0 | [Fixture](w31-local/fixture.log) |
| W20–22 and W30b reserved input | 4 passed, 1 host fixture ignored, exit 0 | [Rows](w31-local/rows.log) |
| Selected-review normal CLI | 1 passed, exit 0 | [CLI](w31-local/selected.log) |
| Pin-enabled isolation units, named wire test excluded | 2 passed, 1 host test ignored, 10 filtered, exit 0 | [Units](w31-local/unit.log) |
| Feature-off binding and budget | 1 passed, 10 filtered, exit 0 | [Default feature set](w31-local/default.log) |
| Named Unix listener and test upstream | **1 failed, exit 101.** The sandbox returned EPERM on the fixture's first AF_UNIX bind. | [Actual failure](w31-local/wire.log) |

The offline components also reproduced the 181-byte Git patch, the three normal CLI configuration refusals and the six recording-client ownership/timeout paths. Python and runbook shell syntax, the named host entry points, private file-open controls, formatting, whitespace and evidence hashes were checked. Graphify's AST update and its exit are recorded locally. [Components](w31-local/components.log) · [Checks and binary fingerprints](w31-local/verification.log) · [Graph update](w31-local/graph-update.log) · [Evidence checksums](w31-local/SHA256SUMS)

The sandbox used private fixtures and read the specified archives. Docker execution, image acquisition/build, Die, provider traffic, production data and external publication were outside this run. The parent owns the host captures and later W31 adjudication.
