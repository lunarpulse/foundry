# W30 제품 실행 격리 경계

기준 커밋은 `a4e07fd56646503fd06ba5b846130a1d2189c9f0`이다. 아래 기존 소스의 줄 번호는 이 커밋 기준이다. W30 설계 → W30b 제품 launcher와 인증 listener 연결 → W31 파일·수명 및 W32 통신·비용 실측 → W23 승인된 실작업 → W33 격리 인수 순서로 진행한다. W30b의 종료 조건과 후속 I행의 종료 조건은 다르다. [개정 로드맵 §6](/home/cosmo/foundry/.hermes/plans/2026-11-22_102736-w1-revised-roadmap.md#L130) [W30–W33 원문](/home/cosmo/foundry/.hermes/plans/2026-11-22_102736-w1-revised-roadmap.md#L137)

## 권한과 실행 경로

제품 컨트롤러는 호스트의 `serve --product`와 그 안의 PackReviewHost다. 이 프로세스가 원 admission, task, claim, lease, fence, 승인된 pack 및 현재 stage를 소유한다. 자식에게 DB, WAL/SHM, 원 저장소 경로, artifact/session/claim root, host HOME, watch 설정, 서비스·사람 토큰, provider 키, Docker socket을 주지 않는다. 결과 승인·finalize·publication 권한도 전달하지 않는다. 과거 deploy-profile의 단일 UID와 cast ledger 읽기는 이 경계의 근거로 사용할 수 없다. [serve 조립](../../crates/foundry/src/main.rs#L2221) [기존 프로필](../../crates/foundry/src/deploy_profile.rs#L40) [상태 소유권 목록](../../crates/foundry-watch/src/inventory.rs#L44)

W2가 수락한 프로필은 model none, 호출·비용·fix 0, 고정 Git 검사다. W30b는 해당 권한 한도를 넓히지 않는다. reserve 전에 원 pack/base와 현재 권한, seal/collect 직전에 worker·epoch·fence·lease를 재검사한다. 불확실한 예약은 복구 대상으로 남긴다. sealed Git 바이트를 수집하는 자식과 ledger의 collected 전이를 구분한다. 후자는 항상 컨트롤러 트랜잭션이다. [W2 계약](../acceptance/w2/contract.md#L3) [교정 후 판정](../acceptance/w2/adjudication.md#L8) [예약](../../crates/foundry-ledger/src/product/code_change.rs#L522) [seal과 collect](../../crates/foundry-ledger/src/product/code_change.rs#L582) [offline pack 제한](../../crates/foundry-line/src/review_host.rs#L1046)

제품 stage 호출은 원 pack host의 초기 실행 및 hold 재개 양쪽에서 journal run과 intent를 launcher로 전달한다. producer/구현, fixer/redrive, reviewer/verify, inspector/lens가 같은 경계를 쓴다. 실제 이미지의 Die CLI 계약인 profile/task/base/fix-context와 구조화된 stage/lens 입력을 보존한다. 자식의 임의 셸 명령을 조립하지 않는다. 생성 commit을 검사하는 Git과 수집하는 Git도 같은 launcher가 새 자식으로 실행한다. 컨트롤러는 명시된 immutable base의 Git 객체와 승인된 선행 산출물만 전달하며 출력 크기를 제한한다. 다음 stage에는 검사한 head와 결속된 객체만 보낸다. [초기 실행](../../crates/foundry-line/src/review_host.rs#L1275) [재개](../../crates/foundry-line/src/review_host.rs#L1973) [기존 Die 계약](../../crates/foundry-line/src/die_executor.rs#L212) [기존 직접 spawn](../../crates/foundry-harness-cli/src/lib.rs#L130) [고정 검사와 패치](../../crates/foundry-ledger/src/product/code_change/retained_git.rs#L61)

## 배포와 관측

제안하는 `deploy/Dockerfile`은 사전 적재된 digest 고정 base의 Python·Git·실제 Die 및 고정 worker entrypoint를 사용하고 UID:GID 65534:65534로 끝난다. 이미지 가용성·도구 포함 여부·UID 매핑은 미검증이다. `deploy/compose.yaml`은 자식 프로필 한 개를 기술한다. serve는 관리 호스트에서 로컬 Unix Docker socket을 소유하고 동적 자식만 만든다. serve 자체를 이 자식 서비스로 시작하지 않는다. rootfs 읽기 전용, ALL capability drop, no-new-privileges, network none, private tmpfs workspace, 모델용 단일 Unix socket 외 host bind 금지다. 입력은 stdin, 응답은 길이 제한 stdout이다. 동일 이름 컨테이너 재사용, 이미지 pull, tag 해석, 전역 cleanup을 금지한다. 소유 ID의 inspect 결과가 render와 다르면 start 전 중단한다. 종료/timeout에서 그 ID를 제거하고 회수가 불확실하면 산출물을 채택하지 않는다.

허용 설정은 운영자가 선택한 로컬 sha256 이미지 ID, 검증된 원 저장소와 전용 상태 디렉터리, 1–3600초 timeout이다. digest·timeout 변경은 새 후보이고 이전 실측을 승계하지 않는다. run 이름과 socket 호스트 경로는 컨트롤러가 생성한다. UID, 추가 mount, namespace, capability, rootfs, command, 원격 daemon 및 임의 Compose override는 허용하지 않는다. `scripts/check-deploy.py`는 정확한 render와 금지된 변경을 검사하고 daemon 접근과 분리하여 실행할 수 있어야 한다. 런타임 관측에는 child ID, 실제 image ID, Config.User, Mounts 및 Tmpfs, CapDrop, SecurityOpt, ReadonlyRootfs, NetworkMode를 보관한다. 이 값은 Docker 설정 관측이다. 실제 프로세스의 UID·cap·namespace 실측은 I06에서 결합한다.

## 인증과 예산

네트워크가 없는 child에는 그 실행의 Unix socket과 단기 무작위 capability만 준다. capability를 표준 로그, inspect 환경, argv나 보관된 요청에 넣지 않는다. controller가 고른 run/provider/budget scope와 요청을 정확 비교하고, 만료/호출 수를 확인한 뒤 기존 ModelProxy의 validate, screen_egress 및 check_response_size를 적용한다. 목적지는 운영자가 고정한 upstream transport다. 응답/요청은 제한하고 오류는 일반화한다. 운영 경로의 허용 호출·비용은 0이다. 유료 정책, provider 자격 증명 및 실모델은 이번 인수에 포함되지 않는다. 테스트 upstream과 그 카운터는 별도로 표시한다. [정책 타입과 검증](../../crates/foundry-watch/src/model_proxy.rs#L62) [목적지 검사](../../crates/foundry-watch/src/model_proxy.rs#L115) [응답 한도](../../crates/foundry-watch/src/model_proxy.rs#L159)

## I01–I06 인수 정의

모든 행의 join key는 후보 소스 지문, 배포 render SHA-256, 실제 image ID, order/epoch/fence, journal run, stage와 그 **제품 자식** ID다. 아래 경로는 후속 실측의 지정 증거원이며 현재 존재한다는 뜻은 아니다. 증거 루트는 `docs/acceptance/w3/`다.

| 행 | 판정 조건 | 원본 증거 |
|---|---|---|
| I01 | 기본 및 각 허용 설정의 render가 실제 생성된 제품 자식 inspect와 일치. 약화된 설정은 start 0 | render.json, 후속 I01/overrides.jsonl, 제품 launcher의 각 stage runtime 영수증 |
| I02 | 해당 자식의 DB/WAL/SHM, HOME·credential·관리 socket 읽기/쓰기 거부, controller 파일 digest와 DB 행 전후 동일 | I02/child-open.jsonl, I02/controller-before.json 및 after.json |
| I03 | 자기 workspace 쓰기와 재읽기 성공, 타 주문·같은 주문 watch 및 privileged serve 접근 거부 | I03/workspaces.jsonl, I03/serve-audit.jsonl, 두 주문의 canary 전후 digest |
| I04 | 인증된 listener에서 승인된 run/provider/scope 1회만 전달. 불일치 거절. 요청 ID로 upstream 호출, 비용, 반환 결과를 결합 | I04/listener.jsonl, I04/test-upstream.jsonl, I04/charges.jsonl, I04/results.jsonl |
| I05 | 지원 IPv4/IPv6, DNS/IP 직접 통신, redirect, 예산 초과 각각의 결과와 실제 upstream/비용/result 증분 확인. 지원 여부를 별도 표기 | I05/cases.jsonl, I05/upstream.jsonl, I05/charges.jsonl, I05/results.jsonl |
| I06 | 실제 프로세스 UID/GID·mountinfo·cap·NoNewPrivs·rootfs·network와 해당 inspect 연결. 소유 자식 timeout 및 회수 뒤 두 차례 관찰에서 출력 변화 0 | I06/proc-self.json, I06/runtime.json, I06/reap.json, I06/output-hashes.jsonl |

환경 가정은 모두 미검증 상태에서 동결한다. 로컬 daemon 접근, 사전 적재된 승인 image, socket bind, user namespace, tmpfs 쓰기, 검증 가능한 upstream, 동일 경계에서의 격리·종료 모두 해당한다. 인터넷 접근·pull·실제 Die나 모델 호출 없이 가용한 항목만 W30b에서 관측한다. 과거 별도 probe 성공은 이 표에 전용할 수 없다.
