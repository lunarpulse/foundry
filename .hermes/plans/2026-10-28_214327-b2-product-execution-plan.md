# B2 제품 실행·다중 팩·격리 구현 계획 v0 (검토 전 초안)

> 실행 절차: 읽기 전용 현황 조사 → 이 계획의 독립 설계 심사(astra) → 부모가 원문 대조·인수행과 계약 결정 정제 → 작업별 검증·W 단위 통합 인수 및 커밋에서 정지. 일반적인 구현 판단은 기존 일괄 권고 승인 범위에서 진행한다. 외부 저장소 쓰기, 실제 자격증명, 파괴적 작업, 기존 계약·스키마 의미 변경은 범위·효과·대안 확인 전 실행하지 않는다.

**목표:** B1의 인가·증거·승인 경계를 보존하면서 제품 주문마다 검증된 팩과 실제 Die를 연결하고 재시작 가능한 격리 실행 경로를 확립한다.

**현행 근거:** 기준 HEAD `971240703aa004347c674b5c8ff1702435908b4d`. `.specs/m9-plan.md:361-407`의 B2와 `docs/architecture.md:84-88`가 원래 목표다. `crates/foundry-line/src/review_host.rs:606-703`는 단일 `REVIEW_PACK_DIGEST` 및 스토어 결속을 확인하고, `crates/foundry/src/main.rs:660-713`은 전역 환경 설정과 조건부 실제 Die/Unavailable executor를 구성한다. `docs/design/b1-scope-architecture.md:184-189`: 같은 OS 사용자 프로세스에는 DB 파일 격리가 보장되지 않는다. B1 P1 실 intake→waiting_unsupported와 P2 명시적 시험 입력→genuine host는 일반 issue→task 자동 실행의 증거가 아니다. 이전 W7 수동 GitHub 게시와 production reconciler live NOT-PERFORMED는 별개다.

**사전 조건:** `graphify-out/GRAPH_REPORT.md`와 wiki는 이 작업 환경에 없어 필수 그래프 지도를 읽지 못했다. 원본 파일을 직접 조사한 한계로 기록한다. 해당 단계의 인수 루브릭·실행 예산은 해당 단계 구현 전에 확정하고 후행 심사의 신규 기준과 구분한다. 외부 라이브/실 운영 DB 변경은 별도 효과 패킷을 준비한다. `cargo --locked --offline`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, scratch는 `/home/cosmo/.hermes/cache/scratch`; clippy/fix 자동 수정 금지.

## W0 — 계약·위협 모델·기준선 (첫 작업, 외부 쓰기 없음)
1. `.specs/m9-plan.md`, `.hermes/plans/2026-10-18_081331-b1-closure.md`, `docs/architecture.md`, `docs/design/b1-product-contract.md`, `docs/design/b1-scope-architecture.md`를 현재 코드/시험과 대조. 기존 frozen 계약과 historical 표기/실제 동작을 분리한다.
2. 기존 `foundry-pack`, `foundry-line`, `foundry-harness-cli`, `foundry-ledger`, `foundry-serve`, `foundry-watch`의 selection, persisted binding, claim/fence, resume, subprocess, publish 경계를 심볼·호출자·테스트로 목록화한다. 변경 파일 및 테스트는 조사 후 확정하며 현 단계에 자동 스키마 변경을 승인하지 않는다.
3. 팩/업무 입력 권위, 등록·선택 주체, (order, lifecycle, stream, run, attempt, pack digest) 불변 결속, 이전 레코드 재개/호환, digest 검증 시점, 취소·revoke·ABA·크래시, 세션/아티팩트 경로 격리, 직접 egress와 프록시, 같은 UID 및 Docker daemon 신뢰에 관한 교차 단계 안전·권한 의미와 열린 실패 정책을 정한다; 정확한 계약은 해당 단계 코드 전에 동결한다. 검증 가능한 허용 입력 grammar가 결정되기 전 임의 issue 본문을 shell 또는 실행 task로 해석하지 않는다.
4. 원 계획의 제품 Die/팩/배포 및 B1 issue→task 유예의 관계를 정하고, W별 필수 인수행 소유·관측 층위·미실시 정책·종료 및 단계별 동결 게이트를 정한다. 정확한 시나리오와 테스트 대상은 각 단계 코드 전에 동결한다. 숫자 한도는 측정/계약 근거 없으면 임의로 정하지 않는다.
5. `git status --porcelain`, 환경/디스크, Docker daemon·이미지 가용성 및 테스트 선택 목록을 실제 확인한다. Docker 부재는 SKIP/NOT-PERFORMED이고 PASS가 아니다. 기존 테스트 baseline은 실행 결과별 수집하며 현재 초안은 합격을 주장하지 않는다.

## W1 — 제품 주문별 팩 선택과 내구 pin
- 후보: `crates/foundry-line/src/review_host.rs`, `crates/foundry-pack`, `crates/foundry-ledger`의 실제 binding/serialization 경로 및 해당 테스트. 변경 목록은 W1 코드 전에 동결하고 필요한 범위 결정을 거친다.
- 두 승인된 서로 다른 팩을 동일 제품 바이너리에서 서로 다른 주문/stream에 선택; 실제 읽은 팩 바이트 digest가 pinned digest와 일치해야 시작. restart·hold/resume은 최초 pin을 유지하며 설정 변경이 기존 주문을 바꾸지 않는다.
- 변조/다른 digest·권한 없는 팩·부재·잘못된 tuple·다른 주문 session·동시 두 주문·재시작/복구 및 pin 바꿔치기를 별도 반례로 증명. 저장/마이그레이션 코드는 해당 단계 전에 호환·구 writer·복구 계약과 필요한 범위 결정을 확정한 후에만 변경. 기존 팩 바이트 및 역사 receipt 보존.

## W2 — 실제 제품 실행과 업무 입력 연결
- 후보: `crates/foundry/src/main.rs`, `crates/foundry-serve/src/lib.rs`, `crates/foundry-line/src/die_executor.rs`, `crates/foundry-harness-cli/src/lib.rs`, 제품 claim/authority 경로, 제품 인수 테스트.
- 실 product entrypoint → 실제 권한·claim/fence → 명시적 검증 task → 실제 Die 프로세스 → host → 결과 commit/tuple → 인간 승인까지 연결한다. fake adapter, synthetic fixture, 실제 Die binary와 실제 외부 GitHub 경로를 증거상 구분한다.
- 지원 밖 입력은 내구 `waiting_unsupported`/명시적 거부; 악의적 issue 본문이 명령이 되지 않음, 재할당/revoke/stale 및 종료·timeout 후 오래된 결과 저장 금지, 승인 digest=생성 commit/tuple을 실측한다. 타 사용자·타 주문·경로/secret 누출 반례 포함.

## W3 — 컨테이너 배포·격리
- 후보 `deploy/Dockerfile`, `deploy/compose.yaml`, `scripts/check-deploy.py`, `crates/foundry/src/deploy_profile.rs` 및 관련 통합 시험. 현재 파일 존재·기존 M5 proxy/container 기능을 W0에서 검사하고 재사용한다.
- serve에서 Die 실행을 당장 유지할지 worker로 분리할지 trust boundary, credential/DB 접근, 공격자·UID, volume 권한, proxy 네트워크를 명시한 뒤 결정. 구조도와 표만으로 격리 합격 처리하지 않는다.
- 렌더된 compose 및 override 검사 + 실제 실행 관측: cast가 credential을 읽거나 serve 권한 API 호출/타 산출물 수정 불가, 정상 proxy 성공, 직접 egress/예산 초과 거부, non-root·read-only 등 선택한 보안 속성 관측. 실행 전 필요한 Docker 이미지/daemon을 확인; 없으면 NOT-PERFORMED 기록.

## W4 — 통합 인수 및 W 종료
- W1–W3 각 frozen 행의 긍정·부정·재시작 실측 및 기존 B1 권한/tuple/hold/revoke/privacy 회귀를 제품 경로에서 확인. 시험 개수만으로 인수하지 않는다.
- M9 기본 검증 후보: `cargo test --locked --offline -p foundry-pack --test plain_pack`; `cargo test --locked --offline -p foundry-line --test die_executor --test review_host_live --test review_evidence`; `cargo test --locked --offline -p foundry-harness-cli`; `cargo test --locked --offline -p foundry-exec -p foundry-workflow -p foundry-orch`; Docker `compose config`, deploy 검사, `foundry-watch --test t56w_docker_mandatory`. 실행 가능 여부·테스트명은 실제 확인 후 고정. B1 회귀 및 신규 제품 E2E를 별도 추가한다.
- 전체 변경 목록·schema/contract 차이·실제 시험 결과·미연결 경로·원격 영향 및 비용을 부모가 검증; 독립 리뷰는 frozen 인수행에 한정한다. 사용자 승인된 외부 효과 외에는 네트워크 쓰기 금지. W 수준 완료 커밋에서 정지·보고; B3는 범위 밖.

## 설계 심사 질문
1. per-order pack selection과 B1의 전역 product backend binding·resume/authority 간 실제 충돌과 안전한 migration 경계는 무엇인가?
2. issue→task의 허용 범위와 실제 Die 생산 경로, 보호해야 할 기존 B1 transaction/fence는?
3. 초기 topology와 container/proxy의 실제 방어 보장·한계 및 테스트 충족 조건은?
4. 위험별 row-level 인수행과 최소 회귀 테스트, 외부 효과/미실시의 표시가 충분한가?
5. B2에 필수인 work와 B3/추후 기능을 구분하고 근거 없는 가정·불가능한 순서·누락 계약을 지적하라.

---

## v1 정제 — astra REFINE 및 부모 대조 (2026-10-28)

독립 심사 `/home/cosmo/.hermes/cache/scratch/b2-astra-plan-review-bounded.log`: `REFINE`, MUST 6·SHOULD 1. 지정 여섯 파일만 읽은 정적 검토이며 테스트·네트워크 실행은 없다. 아래는 **계획 보완 및 제안**이지 아직 스키마 변경·인수 통과의 선언이 아니다. 이 절이 v0와 충돌하면 우선한다.

1. **기존 배포 결정 보존**: M9 및 `docs/architecture.md:88`에 따라 초기 orchestration은 serve 내에 유지한다. 별도의 독립 worker 서비스 전환은 후속 독립 배치다. 단 serve가 실행을 지휘한다는 것과 Die가 같은 UID·같은 마운트·같은 권한으로 실행된다는 것은 다르다. W0에서 subprocess/container의 교차 단계 신뢰 의미를 정하고, W2 코드 전에 UID, DB 및 credential mount/접근, env, Git/네트워크, 입출력과 관리자/daemon 신뢰 모델을 동결한다. 실제 Die 또는 코드를 실행하는 inspector 전 같은 유효 경계에서 격리와 proxy를 실측해야 한다. 같은 UID 직접 접근을 차단한다고 가정하지 않는다. W3에서는 렌더링 및 실제 배포·격리 관측까지 완성한다.
2. **제품 pin 정합성의 실제 경계**: `product.rs:1115-1148`의 digest는 ProductStore 인스턴스의 `review_config`이고, artifact root는 `:1151-1199`에서 별도 내구 pin된다. `product/review.rs:544-553,573-593`는 activation이 configured digest에 일치하는지 확인하고 같은 트랜잭션에서 source/claim을 검증한다. 따라서 '전역 digest가 DB에 저장된다'거나 특정 migration이 이미 필요하다고 단정하지 않는다. 주문별 선택 권한, 최초 pin과 admission/claim의 원자적 결속, 상충 동시 요청, 재시작·hold·다른 handle의 동일한 pin, 구 레코드/구 바이너리 호환 및 실패 정책을 조사하고 frozen 변경안으로 먼저 제안한다. 결과·tuple·권한/claim/fence·과거 receipt는 보존한다. 스키마 및 의미 변화는 근거·영향 확인 전 구현하지 않는다.
3. **검증→실행 바이트 동일성**: `review_host.rs:926-940`은 `load_pack` snapshot 검증 후 같은 snapshot으로 GraphEngine을 구성한다. `foundry-pack/src/lib.rs:30-40,87-88`은 파일 바이트를 포함하는 snapshot을 제공한다. 경로 재읽기가 있으나 현재 실제 취약점이라고 단정하지 않는다. 변경 이후 실행 그래프·입력·persisted snapshot·resume이 검증된 digest의 동일 바이트를 사용하거나 재검증 후 거부하는 계약을 작성하고, 검사 직후 경로 교체·재시작 시 변조 및 두 주문의 교차 혼용을 각각 인수 시험한다.
4. **M9 파일 범위**: `.specs/m9-plan.md:367-378`의 원 B2 허용 목록에 `foundry-ledger`·`foundry-pack` 구현은 없으며 v0의 후보 언급은 수정 승인으로 해석하지 않는다. W0에서 범위 차이와 열린 선택을 기록하고, 적용 단계 코드 전에 실제 충돌·필수 변경 파일·기존 공개 API/와이어·마이그레이션 영향과 대안을 구체화한다. 범위 확장이 필요하면 별도 결정 전 해당 파일을 수정하지 않는다. 읽기 전용 조사 및 관련 테스트 목록화는 가능하다.
5. **Docker/실측 인수**: `m9-plan.md:395-403`의 실제 부정 관측과 정상 프록시 경로는 B2 필수다. 로컬 이미지/daemon에 의존하는 fixture의 준비와 필요 조건을 명시하고 외부 pull 없이 재현 가능한지 검증한다. daemon·이미지 부족이나 시험 미실행은 NOT-PERFORMED이자 B2 전체 미완료이며, 렌더된 설정·mock·정적 검사만으로 대체하지 않는다.
6. **설정 불량 제품 경로**: `main.rs:665-696`의 현재 미설정 host/Unavailable executor/timeout 기본값을 기준선으로 기록한다. 누락 pack, Die/repo 일부 누락, invalid timeout, 죽은 프로세스 및 재설정의 실패/내구 상태·진단·복구를 W2 코드 전에 정의하고 W2의 개별 인수행에 보존한다. 현 동작의 안전성은 아직 시험하지 않았다.
7. **초기 환경 관측**: 2026-10-28 로컬 `docker --version` 29.4.3, daemon 29.4.3, Compose v5.1.3; `t44b-cast:1`, `t56w-cast:1`, `busybox:1.37` 등 이미지 목록에 존재. 루트 파일시스템 여유 27G (`df -h .`). 이는 가용성 목록일 뿐 실제 격리 시험·해당 이미지 적합성·프록시 완성의 증거가 아니다. 작업 트리는 이 계획 파일만 untracked.

### W0 출구와 단계별 게이트 — READY FOR DOCUMENTATION COMMIT

W0는 교차 단계 안전·권한 의미, 행 소유와 필수 증거 층위, W0 → W2a → W1 → W2 → W3 → W4 순서 및 각 동결 게이트를 정한다. 부모 검증과 W0 로컬 커밋 시 이 교차 단계 정책을 동결하며 그 전에는 W0 미완료다. [현행 W0 결정 기록](b2-w0-progressive-freeze-revision.md)이 이전 초안의 W0 timing/status보다 우선한다. 각 후속 단계의 정확한 계약은 결정·시험될 때까지 DRAFT/BLOCKED이며 26개 제품 인수행은 DRAFT/BLOCKED/NOT-RUN이다. 각 후속 단계는 자체 코드 전에 정확한 계약, 적용 API/schema/파일 범위, 시험 이름·명령·입력·단언·증거를 동결한다. [인수 매트릭스](b2-acceptance-matrix-draft.md)의 단계별 표와 모든 원 행이 정본이며, 상세의 이연은 증거 축소가 아니다. W2a는 A01의 고정 입력 test-only fake seam에 한정하고 Held/Failed/B1을 약화하지 않는다. 실제 Die 또는 코드를 실행하는 inspector 전에 같은 유효 경계의 격리·proxy를 실측하며 W3 I01–I06은 모두 별도로 필수다.

schema 코드 전 migration/open, 이미 열린 구 writer fence, quiescence, backup, crash/rollback 및 새 durable write 이후 reconciliation, 필요 범위 결정을 동결한다. schema 불필요는 검증되지 않은 선택지이지 의무가 아니다. M9 제외 파일의 제안된 ledger 예외는 승인되지 않았으며 실제 schema/wire 및 frozen 계약 변경과 함께 별도 명시적 변경 통제가 필요하다. 파괴적 작업, 운영 migration 및 실제 외부 효과도 별도 통제한다. 일상적 계획 권고를 채택하는 상설 지시는 정확한 예외 승인으로 해석하지 않는다. 각 W는 로컬 완료 커밋에서 정지·보고한다. 이번 범위에서 허용 목록 밖 편집과 migration, 운영 및 원격 효과는 승인되지 않았다. 제품 코드 정책은 Codex CLI/omp gpt-6.1-terra high다. 이 문서는 구체 schema/허용 목록 예외, 구현 또는 마이그레이션의 승인이 아니다.
