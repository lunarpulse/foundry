//! T57 — 오케스트레이션 로그: 멀티-오더 오케스트레이션의 진실원천.
//!
//! (M7 플랜 v9 T57) 계약:
//! - **해시체인 저장소**: 모든 행이 prev_hash 로 연결 — 변조 감지.
//! - **행 타입**: plan_declared / order_spawned / handoff_bound /
//!   preset_bound / refusal. refusal 은 체인에 포함되어 해시·검증되지만
//!   **선언 증거에서 배제**된다(선언 순수성 — 거부가 발행 자격을
//!   만들지 않는다).
//! - **선언 검증**: 디스패치 전 내구 선언(어떤 주문도 plan_declared
//!   없이 ready 될 수 없음)·사이클 거부·부재 선행 거부.
//! - **스트림 identity**: 스트림 = 프리셋 root의 활성화 인스턴스
//!   (안정 id). 같은 root의 재활성화는 별개 스트림.
//! - **핸드오프 매니페스트**: 고정폭(8자리) len-prefix 정규 인코딩,
//!   "fh1:" 가시 태그, 99,999,999 바이트 상한 — 구분자/길이스트림
//!   모호성 원천 불가 (astra r4-r6 폐쇄).

pub mod chainer;
pub mod refusal;
pub mod stream;

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;

/// 행 종류 — 타입드. 새 종류는 이 열거에 추가해야만 직렬화된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrchestrationKind {
    /// 에픽/프로젝트 선언 — 미래 주문 id·의존성 간선·스트림 배정.
    PlanDeclared {
        plan_id: String,
        /// (주문 id → 선행 주문 id 목록) — 사이클 없어야.
        edges: Vec<(String, Vec<String>)>,
        /// 주문별 스트림 배정 (주문 id → 스트림 id).
        streams: Vec<(String, String)>,
    },
    /// 체이너의 주문 발행 기록.
    OrderSpawned { order_id: String, plan_id: String, stream: String },
    /// 산출물 인계 (매니페스트 digest 는 이 행을 가리킨다).
    HandoffBound {
        source_order: String,
        dest_order: String,
        artifact_digest: String,
    },
    /// 사이즈 라우팅 — 인간 사이징 게이트 승인의 결속 (H절).
    PresetBound {
        sizing_order: String,
        selected_root: String,
        approved_digest: String,
        spawn_plan: String,
    },
    /// 거부 기록 (I절 — 증거에서 배제, 감사 전용).
    Refusal {
        refused_request: String,
        refusal_code: String,
        reason_text: String,
        acceptance_condition: String,
        remediation_proposal_id: Option<String>,
    },
}

/// 저장 행 — 체인 노드.
#[derive(Debug, Clone, PartialEq)]
pub struct OrchRow {
    pub seq: i64,
    pub kind: OrchestrationKind,
    pub prev_hash: String,
    pub hash: String,
}

const GENESIS: &str = "GENESIS";
const DOMAIN_LABEL: &[u8] = b"foundry-orch-chain-v1\0";

fn row_hash(seq: i64, prev: &str, kind_json: &str) -> String {
    let mut h = sha2::Sha256::new();
    h.update(DOMAIN_LABEL);
    // (astra r1 M3) seq 포함 — 재번호화·삭제 감지. 16자리 고정폭 hex.
    let seq_hex: String = seq.to_le_bytes().iter().map(|b| format!("{:02x}", b)).collect();
    h.update(format!("{:016}:{}", seq_hex.len(), "").as_bytes());
    h.update(seq_hex.as_bytes());
    h.update(format!("{:016}:{}", prev.len(), "").as_bytes());
    h.update(prev.as_bytes());
    h.update(format!("{:016}:{}", kind_json.len(), "").as_bytes());
    h.update(kind_json.as_bytes());
    format!("{:x}", h.finalize())
}

/// 핸드오프 매니페스트 다이제스트 (C′ 계약).
///
/// 인코딩: "fh1:" 가시 태그 + hex(H(label ‖ f(source) ‖ f(dest) ‖
/// f(artifact) ‖ f(seq_str))) where f(x) = format!("{:08}:", x.len()) ‖ x
/// — **고정폭 8자리** len-prefix: 길이 필드 경계가 항상 유일
/// (astra r5 "1"+"11111111111" 류 십진 스트림 충돌 원천 불가).
pub const HANDOFF_TAG: &str = "fh1:";
const HANDOFF_LABEL: &[u8] = b"foundry-handoff-v1\0";
const FIELD_CAP: usize = 99_999_999;

fn field(x: &str) -> Result<Vec<u8>, String> {
    if x.len() > FIELD_CAP {
        return Err(format!("field exceeds {FIELD_CAP} bytes"));
    }
    let mut out = format!("{:08}:", x.len()).into_bytes();
    out.extend_from_slice(x.as_bytes());
    Ok(out)
}

/// 매니페스트 digest 생성 — "fh1:<hex>" 형태.
pub fn handoff_manifest_digest(
    source: &str,
    dest: &str,
    artifact: &str,
    log_seq: i64,
) -> Result<String, String> {
    let mut h = sha2::Sha256::new();
    h.update(HANDOFF_LABEL);
    for part in [source, dest, artifact, &log_seq.to_string()] {
        h.update(field(part)?);
    }
    Ok(format!("{HANDOFF_TAG}{:x}", h.finalize()))
}

/// digest 가 핸드오프 매니페스트인지 (가시 태그 검사).
pub fn is_handoff_digest(digest: &str) -> bool {
    digest.starts_with(HANDOFF_TAG)
}

/// (astra r1 M5) 트랜잭션 가드 — Drop 시 활성 상태면 ROLLBACK.
struct TxGuard<'c> {
    conn: &'c rusqlite::Connection,
    active: bool,
}

impl<'c> TxGuard<'c> {
    fn begin(conn: &'c rusqlite::Connection) -> Result<Self, OrchError> {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| OrchError::Io(e.to_string()))?;
        Ok(Self { conn, active: true })
    }

    /// 커밋 — **성공 후에만** active 해제(astra r2 M5): COMMIT 실패
    /// (SQLITE_BUSY 등) 시 트랜잭션이 열려 있으므로 Drop 이 ROLLBACK
    /// 하도록 둔다.
    fn commit(mut self) -> Result<(), OrchError> {
        let r = self.conn.execute_batch("COMMIT").map_err(|e| OrchError::Io(e.to_string()));
        if r.is_ok() {
            self.active = false;
        }
        r
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

/// 로그 — sqlite 해시체인 저장소.
///
/// ## 쓰기 모델 (astra r3 조건 문서화)
/// 모든 공개 쓰기 진입점은 **오케스트레이션 코디네이터(단일 소유자)**
/// 를 통해서만 호출된다 — declare_plan/spawn_order/bind_handoff/
/// append_preset_bound 의 외부 검증(트랜잭션 밖 사전 검사)은 이
/// 단일-작성자 전제 하에 안전하다(선언은 불변·주문 id 전역 유일이라
/// 사전 검사와 append 사이에 무효화되는 전제가 없다). declare_plan 의
/// 유일성 재검증은 BEGIN IMMEDIATE 안에서 트랜잭션적으로 수행된다.
/// 다중 작성자가 필요해지면 모든 쓰기를 append_validated 경유로
/// 이관해야 한다.
pub struct OrchestrationLog {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

#[derive(Debug)]
pub enum OrchError {
    Io(String),
    Cycle { node: String },
    MissingPredecessor { order: String, missing: String },
    UndeclaredOrder { order: String },
    DuplicateDeclaration { plan_id: String },
    /// (astra r1 M1) 이미 선언된 주문 id 의 재선언 — 전역 유일 위반.
    DuplicateOrder { order: String, in_plan: String },
    /// (astra r1 M2) 발행 스트림이 선언 배정과 불일치.
    StreamMismatch { order: String, declared: String, claimed: String },
    ChainBroken { at_seq: i64 },
    /// 해당 seq 의 행이 없다 (포트 fail-closed 매핑용).
    NoRow { seq: i64 },
    FieldTooLarge,
}

impl std::fmt::Display for OrchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "orchestration io: {e}"),
            Self::Cycle { node } => write!(f, "declaration cycle through '{node}'"),
            Self::MissingPredecessor { order, missing } => {
                write!(f, "order '{order}' depends on undeclared '{missing}'")
            }
            Self::UndeclaredOrder { order } => {
                write!(f, "order '{order}' has no plan declaration — unsized/undeclared spawn")
            }
            Self::DuplicateDeclaration { plan_id } => {
                write!(f, "plan '{plan_id}' declared twice")
            }
            Self::DuplicateOrder { order, in_plan } => {
                write!(f, "order '{order}' already declared in plan '{in_plan}' — order ids are globally unique")
            }
            Self::StreamMismatch { order, declared, claimed } => {
                write!(f, "order '{order}' declared on stream '{declared}' but spawned claiming '{claimed}'")
            }
            Self::ChainBroken { at_seq } => write!(f, "chain broken at seq {at_seq}"),
            Self::NoRow { seq } => write!(f, "no row at seq {seq}"),
            Self::FieldTooLarge => write!(f, "field exceeds cap"),
        }
    }
}

impl OrchestrationLog {
    pub fn open(path: &Path) -> Result<Self, OrchError> {
        let conn = rusqlite::Connection::open(path).map_err(|e| OrchError::Io(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS orch (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 payload TEXT NOT NULL,
                 prev_hash TEXT NOT NULL,
                 hash TEXT NOT NULL
             );
             CREATE TRIGGER IF NOT EXISTS orch_no_update BEFORE UPDATE ON orch
                 BEGIN SELECT RAISE(ABORT, 'orchestration log is append-only'); END;
             CREATE TRIGGER IF NOT EXISTS orch_no_delete BEFORE DELETE ON orch
                 BEGIN SELECT RAISE(ABORT, 'orchestration log is append-only'); END;",
        )
        .map_err(|e| OrchError::Io(e.to_string()))?;
        Ok(Self { conn: std::sync::Mutex::new(conn) })
    }

    /// 행 append — 체인 연장. **비공개** (astra r1 M2): 검증 통과
    /// 진입점만 공개. RAAI 트랜잭션(astra r1 M5) — 오류/패닉 시 자동
    /// 롤백. hash 는 seq 포함이므로 INSERT(빈 해시) → seq 확보 → UPDATE
    /// 채움을 같은 트랜잭션에서 수행(원자).
    fn append(&self, kind: OrchestrationKind) -> Result<OrchRow, OrchError> {
        self.append_validated(kind, |_| Ok(()))
    }

    /// (T59 astra r6) **크레이트 내부 전용** — 체이너의 원자적 검증+append.
    /// 외부 공개 시 임의 행(예: 미선언 OrderSpawned)을 우회 append 할 수
    /// 있어 pub(crate)로 제한한다(외부 정문은 declare_plan/bind_handoff
    /// 뿐 — spawn_order/append_preset_bound 도 내부).
    pub(crate) fn append_with_validation<F>(
        &self,
        kind: OrchestrationKind,
        validate: F,
    ) -> Result<OrchRow, OrchError>
    where
        F: FnOnce(&[OrchRow]) -> Result<(), OrchError>,
    {
        self.append_validated(kind, validate)
    }

    /// (astra r2 M1) 검증+append 를 단일 BEGIN IMMEDIATE 안에서 — 쓰기
    /// 잠금 선점 후 검증이 직렬화되어 동시 선언의 유일성 검사 경쟁
    /// (TOCTOU)을 원천 차단한다. 검증 클로저는 잠긴 커넥션의 행
    /// 스냅샷을 받는다(뮤텍스 재진입 교착 방지).
    fn append_validated<F>(&self, kind: OrchestrationKind, validate: F) -> Result<OrchRow, OrchError>
    where
        F: FnOnce(&[OrchRow]) -> Result<(), OrchError>,
    {
        let conn = self.conn.lock().unwrap();
        let guard = TxGuard::begin(&conn)?;
        let rows_now = read_all_locked(&conn)?;
        validate(&rows_now)?;
        let prev: String = conn
            .query_row(
                "SELECT hash FROM orch ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| GENESIS.to_string());
        let payload = serde_json::to_string(&kind)
            .map_err(|e| OrchError::Io(e.to_string()))?;
        conn.execute_batch("DROP TRIGGER orch_no_update;")
            .map_err(|e| OrchError::Io(e.to_string()))?;
        conn.execute(
            "INSERT INTO orch (payload, prev_hash, hash) VALUES (?1, ?2, '')",
            rusqlite::params![payload, prev],
        )
        .map_err(|e| OrchError::Io(e.to_string()))?;
        let seq: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .map_err(|e| OrchError::Io(e.to_string()))?;
        let hash = row_hash(seq, &prev, &payload);
        conn.execute(
            "UPDATE orch SET hash = ?1 WHERE seq = ?2",
            rusqlite::params![hash, seq],
        )
        .map_err(|e| OrchError::Io(e.to_string()))?;
        conn.execute_batch(
            "CREATE TRIGGER orch_no_update BEFORE UPDATE ON orch
                 BEGIN SELECT RAISE(ABORT, 'orchestration log is append-only'); END;",
        )
        .map_err(|e| OrchError::Io(e.to_string()))?;
        guard.commit()?;
        Ok(OrchRow { seq, kind, prev_hash: prev, hash })
    }

    /// 전 행 조회 (체인 무결성 검증 포함).
    pub fn read_all(&self) -> Result<Vec<OrchRow>, OrchError> {
        let conn = self.conn.lock().unwrap();
        read_all_locked(&conn)
    }

    fn __removed_marker(&self) {}


    /// 체인 검증만 (read_all 가 내부 수행 — 명시적 진입점).
    pub fn verify_chain(&self) -> Result<bool, OrchError> {
        self.read_all().map(|_| true)
    }

    /// 에픽 선언 — 디스패치 전 내구. 사이클·부재 선행·중복 선언 거부.
    pub fn declare_plan(
        &self,
        plan_id: &str,
        edges: Vec<(String, Vec<String>)>,
        streams: Vec<(String, String)>,
    ) -> Result<OrchRow, OrchError> {
        // 중복 선언 거부.
        let existing = self.read_all()?;
        if existing.iter().any(|r| {
            matches!(&r.kind, OrchestrationKind::PlanDeclared { plan_id: p, .. } if p == plan_id)
        }) {
            return Err(OrchError::DuplicateDeclaration { plan_id: plan_id.into() });
        }
        // (astra r1 M1) 주문 id 전역 유일 — 기존 선언의 주문 재선언 거부.
        // 교차 플랜 간선 덮어쓰기로 사이클이 숨는 일이 없다.
        let prior_orders: std::collections::BTreeMap<String, String> = existing
            .iter()
            .flat_map(|r| match &r.kind {
                OrchestrationKind::PlanDeclared { plan_id, edges, .. } => edges
                    .iter()
                    .map(move |(o, _)| (o.clone(), plan_id.clone()))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        for (order, _) in &edges {
            if let Some(in_plan) = prior_orders.get(order) {
                return Err(OrchError::DuplicateOrder {
                    order: order.clone(),
                    in_plan: in_plan.clone(),
                });
            }
        }
        // 이번 선언 안 중복 키도 거부.
        let mut seen = std::collections::BTreeSet::new();
        for (order, _) in &edges {
            if !seen.insert(order.clone()) {
                return Err(OrchError::DuplicateOrder {
                    order: order.clone(),
                    in_plan: plan_id.into(),
                });
            }
        }
        // 선행 존재 검사 — 전역 선언 집합(기존+이번).
        let mut declared: std::collections::BTreeSet<String> =
            prior_orders.keys().cloned().collect();
        for (o, _) in &edges {
            declared.insert(o.clone());
        }
        // 부재 선행 거부.
        for (order, preds) in &edges {
            for p in preds {
                if !declared.contains(p) {
                    return Err(OrchError::MissingPredecessor {
                        order: order.clone(),
                        missing: p.clone(),
                    });
                }
            }
        }
        // 사이클 거부 — 전체 간선 병합 그래프에서 DFS.
        let mut all_edges: std::collections::BTreeMap<String, Vec<String>> =
            existing.iter().fold(Default::default(), |mut acc, r| {
                if let OrchestrationKind::PlanDeclared { edges, .. } = &r.kind {
                    for (o, preds) in edges {
                        acc.insert(o.clone(), preds.clone());
                    }
                }
                acc
            });
        for (o, preds) in edges.clone() {
            all_edges.insert(o, preds);
        }
        if let Some(cycle_node) = find_cycle(&all_edges) {
            return Err(OrchError::Cycle { node: cycle_node });
        }
        // 스트림 배정 검사 — 각 주문의 스트림이 선언되어야.
        for (order, _) in &edges {
            if !streams.iter().any(|(o, _)| o == order) {
                return Err(OrchError::Io(format!(
                    "order '{order}' has no stream assignment"
                )));
            }
        }
        // (astra r2 M1) 트랜잭션 내 최종 재검증 — 경쟁 창 제거.
        let plan_id_c = plan_id.to_string();
        let edges_c = edges.clone();
        self.append_validated(
            OrchestrationKind::PlanDeclared {
                plan_id: plan_id.into(),
                edges,
                streams,
            },
            move |now| {
                if now.iter().any(|r| {
                    matches!(&r.kind, OrchestrationKind::PlanDeclared { plan_id: p, .. } if *p == plan_id_c)
                }) {
                    return Err(OrchError::DuplicateDeclaration { plan_id: plan_id_c });
                }
                for (order, _) in &edges_c {
                    if now.iter().any(|r| {
                        matches!(&r.kind, OrchestrationKind::PlanDeclared { edges, .. }
                            if edges.iter().any(|(o, _)| o == order))
                    }) {
                        return Err(OrchError::DuplicateOrder {
                            order: order.clone(),
                            in_plan: plan_id_c.clone(),
                        });
                    }
                }
                Ok(())
            },
        )
    }

    /// 발행 기록 — 선언되지 않은 주문의 발행은 거부 (unsized-spawn 계약).
    /// (astra r10 P1) **크레이트 내부 전용** — 발행 기록의 유일 경로는
    /// StreamRuntime::dispatch (사이즈 바인딩 게이트 통과 후) 이다.
    /// 외부 직접 spawn_order 는 사이즈 검사 우회 append 이므로 차단.
    pub(crate) fn spawn_order(
        &self,
        order_id: &str,
        plan_id: &str,
        stream: &str,
    ) -> Result<OrchRow, OrchError> {
        let existing = self.read_all()?;
        let mut found: Option<(String, String)> = None; // (plan, stream)
        for r in &existing {
            if let OrchestrationKind::PlanDeclared { plan_id: p, edges, streams } = &r.kind {
                if edges.iter().any(|(o, _)| o == order_id) {
                    if let Some((_, s)) = streams.iter().find(|(o, _)| o == order_id) {
                        found = Some((p.clone(), s.clone()));
                    }
                }
            }
        }
        let (declared_plan, declared_s) = found
            .ok_or_else(|| OrchError::UndeclaredOrder { order: order_id.into() })?;
        if declared_plan != plan_id {
            return Err(OrchError::UndeclaredOrder { order: order_id.into() });
        }
        if declared_s != stream {
            return Err(OrchError::StreamMismatch {
                order: order_id.into(),
                declared: declared_s,
                claimed: stream.into(),
            });
        }
        self.append(OrchestrationKind::OrderSpawned {
            order_id: order_id.into(),
            plan_id: plan_id.into(),
            stream: stream.into(),
        })
    }

    /// 핸드오프 결속 — digest 는 이 행의 seq 로 매니페스트 계산.
    pub fn bind_handoff(
        &self,
        source_order: &str,
        dest_order: &str,
        artifact_digest: &str,
    ) -> Result<(OrchRow, String), OrchError> {
        let row = self.append(OrchestrationKind::HandoffBound {
            source_order: source_order.into(),
            dest_order: dest_order.into(),
            artifact_digest: artifact_digest.into(),
        })?;
        let manifest = handoff_manifest_digest(
            source_order,
            dest_order,
            artifact_digest,
            row.seq,
        )
        .map_err(|_| OrchError::FieldTooLarge)?;
        Ok((row, manifest))
    }

    /// 사이즈 결속 (H절 — 발행 권한의 근거 행). (astra r8) **크레이트
    /// 내부 전용** — 원장 CONSUMED 증거 검증은 chainer 의
    /// bind_preset_from_ledger 이 수행하고, 그 경유 append 만 허용된다.
    /// 외부 직접 append 는 임의 PresetBound 위조 경로이므로 차단.
    pub(crate) fn append_preset_bound(
        &self,
        sizing_order: &str,
        selected_root: &str,
        approved_digest: &str,
        spawn_plan: &str,
    ) -> Result<OrchRow, OrchError> {
        self.append(OrchestrationKind::PresetBound {
            sizing_order: sizing_order.into(),
            selected_root: selected_root.into(),
            approved_digest: approved_digest.into(),
            spawn_plan: spawn_plan.into(),
        })
    }

    /// 선언 증거 — refusal 행은 배제 (선언 순수성).
    pub fn declaration_rows(&self) -> Result<Vec<OrchRow>, OrchError> {
        Ok(self
            .read_all()?
            .into_iter()
            .filter(|r| !matches!(r.kind, OrchestrationKind::Refusal { .. }))
            .collect())
    }

    /// seq 로 행 조회 — HandoffLogPort 의 조회 기반 (fail-closed: 부재=Err).
    pub fn row_at(&self, seq: i64) -> Result<OrchRow, OrchError> {
        let rows = self.read_all()?;
        rows.into_iter()
            .find(|r| r.seq == seq)
            .ok_or(OrchError::NoRow { seq })
    }
}

/// 커넥션 직접 읽기 — 체인 무결성 검증 포함 (락 보유 중 호출용).
fn read_all_locked(conn: &rusqlite::Connection) -> Result<Vec<OrchRow>, OrchError> {
        let mut stmt = conn
            .prepare("SELECT seq, payload, prev_hash, hash FROM orch ORDER BY seq")
            .map_err(|e| OrchError::Io(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let seq: i64 = r.get(0)?;
                let payload: String = r.get(1)?;
                let prev_hash: String = r.get(2)?;
                let hash: String = r.get(3)?;
                Ok((seq, payload, prev_hash, hash))
            })
            .map_err(|e| OrchError::Io(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrchError::Io(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        let mut expect_prev = GENESIS.to_string();
        for (seq, payload, prev_hash, hash) in rows {
            let idx = out.len() as i64;
            if seq != idx + 1 || prev_hash != expect_prev || row_hash(seq, &prev_hash, &payload) != hash {
                return Err(OrchError::ChainBroken { at_seq: seq });
            }
            let kind = serde_json::from_str(&payload)
                .map_err(|e| OrchError::Io(format!("payload decode: {e}")))?;
            expect_prev = hash.clone();
            out.push(OrchRow { seq, kind, prev_hash, hash });
        }
        Ok(out)
}

/// DFS 사이클 검출 — 발견 시 사이클 노드 하나 반환.
fn find_cycle(
    edges: &std::collections::BTreeMap<String, Vec<String>>,
) -> Option<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Grey,
        Black,
    }
    let mut marks: std::collections::BTreeMap<String, Mark> =
        edges.keys().map(|k| (k.clone(), Mark::White)).collect();
    fn dfs(
        node: &str,
        edges: &std::collections::BTreeMap<String, Vec<String>>,
        marks: &mut std::collections::BTreeMap<String, Mark>,
    ) -> Option<String> {
        marks.insert(node.into(), Mark::Grey);
        for next in edges.get(node).into_iter().flatten() {
            match marks.get(next).copied().unwrap_or(Mark::White) {
                Mark::Grey => return Some(next.clone()),
                Mark::White => {
                    if let Some(c) = dfs(next, edges, marks) {
                        return Some(c);
                    }
                }
                Mark::Black => {}
            }
        }
        marks.insert(node.into(), Mark::Black);
        None
    }
    for node in edges.keys() {
        if matches!(marks.get(node), Some(Mark::White) | None) {
            marks.entry(node.clone()).or_insert(Mark::White);
            if let Some(c) = dfs(node, edges, &mut marks) {
                return Some(c);
            }
        }
    }
    None
}

/// T57 종단 재구축 — 행 집합에서 (선언·발행·인계·결속) 투영 재계산.
/// read_all 결과와 독립 계산이 일치해야 (재구축 동등 게이트의 대상).
pub struct Projection {
    /// (astra r1 M3) 스트림 배정까지 전 필드 보존.
    pub plans: Vec<(String, Vec<(String, Vec<String>)>, Vec<(String, String)>)>,
    pub spawned: Vec<(String, String, String)>,
    pub handoffs: Vec<(String, String, String, i64)>,
    pub preset_bounds: Vec<(String, String, String, String)>,
}

pub fn project(rows: &[OrchRow]) -> Projection {
    let mut p = Projection { plans: Vec::new(), spawned: Vec::new(), handoffs: Vec::new(), preset_bounds: Vec::new() };
    for r in rows {
        match &r.kind {
            OrchestrationKind::PlanDeclared { plan_id, edges, streams } => {
                p.plans.push((plan_id.clone(), edges.clone(), streams.clone()))
            }
            OrchestrationKind::OrderSpawned { order_id, plan_id, stream } => {
                p.spawned.push((order_id.clone(), plan_id.clone(), stream.clone()))
            }
            OrchestrationKind::HandoffBound { source_order, dest_order, artifact_digest } => {
                p.handoffs.push((source_order.clone(), dest_order.clone(), artifact_digest.clone(), r.seq))
            }
            OrchestrationKind::PresetBound { sizing_order, selected_root, approved_digest, spawn_plan } => {
                p.preset_bounds.push((sizing_order.clone(), selected_root.clone(), approved_digest.clone(), spawn_plan.clone()))
            }
            OrchestrationKind::Refusal { .. } => {
                // 선언 순수성: 거부 행은 투영에 기여하지 않는다.
            }
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("foundry-orch-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn chain_appends_and_verifies() {
        let log = OrchestrationLog::open(&tmp("chain")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "s-1".into())],
        )
        .unwrap();
        log.spawn_order("o-a", "p1", "s-1").unwrap();
        assert!(log.verify_chain().unwrap());
        assert_eq!(log.read_all().unwrap().len(), 2);
    }

    #[test]
    fn cycle_rejected() {
        let log = OrchestrationLog::open(&tmp("cycle")).unwrap();
        let e = log
            .declare_plan(
                "p1",
                vec![
                    ("o-a".into(), vec!["o-b".into()]),
                    ("o-b".into(), vec!["o-a".into()]),
                ],
                vec![
                    ("o-a".into(), "s".into()),
                    ("o-b".into(), "s".into()),
                ],
            )
            .unwrap_err();
        assert!(matches!(e, OrchError::Cycle { .. }), "{e:?}");
    }

    #[test]
    fn missing_predecessor_rejected() {
        let log = OrchestrationLog::open(&tmp("missing")).unwrap();
        let e = log
            .declare_plan(
                "p1",
                vec![("o-a".into(), vec!["o-ghost".into()])],
                vec![("o-a".into(), "s".into())],
            )
            .unwrap_err();
        assert!(matches!(e, OrchError::MissingPredecessor { .. }));
    }

    #[test]
    fn undeclared_spawn_rejected() {
        let log = OrchestrationLog::open(&tmp("undeclared")).unwrap();
        let e = log.spawn_order("o-never", "p-none", "s").unwrap_err();
        assert!(matches!(e, OrchError::UndeclaredOrder { .. }));
    }

    #[test]
    fn refusal_rows_excluded_from_evidence() {
        let log = OrchestrationLog::open(&tmp("refusal")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "s".into())],
        )
        .unwrap();
        log.append(OrchestrationKind::Refusal {
            refused_request: "req-1".into(),
            refusal_code: "unsized_spawn".into(),
            reason_text: "사이징 승인 없는 발행".into(),
            acceptance_condition: "사이징 게이트에서 스케일 확정".into(),
            remediation_proposal_id: Some("prop-1".into()),
        })
        .unwrap();
        // 체인은 거부 행까지 포함해 검증.
        assert!(log.verify_chain().unwrap());
        assert_eq!(log.read_all().unwrap().len(), 2);
        // 증거는 배제.
        assert_eq!(log.declaration_rows().unwrap().len(), 1);
        let p = project(&log.read_all().unwrap());
        assert!(p.plans.len() == 1 && p.spawned.is_empty());
    }

    #[test]
    fn tampering_detected() {
        let path = tmp("tamper");
        {
            let log = OrchestrationLog::open(&path).unwrap();
            log.declare_plan(
                "p1",
                vec![("o-a".into(), vec![])],
                vec![("o-a".into(), "s".into())],
            )
            .unwrap();
        }
        // 직접 SQL 로 payload 변조 (트리거는 UPDATE 차단 — 테이블 재생성 우회).
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER orch_no_update;").unwrap();
        conn.execute(
            "UPDATE orch SET payload = '{\"kind\":\"refusal\",\"refused_request\":\"x\",\"refusal_code\":\"y\",\"reason_text\":\"z\",\"acceptance_condition\":\"w\"}' WHERE seq = 1",
            [],
        )
        .unwrap();
        drop(conn);
        let log = OrchestrationLog::open(&path).unwrap();
        assert!(matches!(
            log.verify_chain(),
            Err(OrchError::ChainBroken { .. })
        ));
    }

    #[test]
    fn rebuild_projection_equivalence() {
        let path = tmp("rebuild");
        let log = OrchestrationLog::open(&path).unwrap();
        log.declare_plan(
            "p1",
            vec![
                ("o-a".into(), vec![]),
                ("o-b".into(), vec!["o-a".into()]),
            ],
            vec![
                ("o-a".into(), "s-1".into()),
                ("o-b".into(), "s-1".into()),
            ],
        )
        .unwrap();
        log.spawn_order("o-a", "p1", "s-1").unwrap();
        let (row, manifest) = log.bind_handoff("o-a", "o-b", "dig-artifact").unwrap();
        assert!(is_handoff_digest(&manifest));
        // 매니페스트는 행 결속 — 재계산 일치 (3자 일치의 기반).
        let again = handoff_manifest_digest("o-a", "o-b", "dig-artifact", row.seq).unwrap();
        assert_eq!(manifest, again);
        // 재구축 동등: 재오픈해서 투영이 동일.
        let reopened = OrchestrationLog::open(&path).unwrap();
        let p1 = project(&log.read_all().unwrap());
        let p2 = project(&reopened.read_all().unwrap());
        assert_eq!(p1.plans, p2.plans);
        assert_eq!(p1.spawned, p2.spawned);
        assert_eq!(p1.handoffs, p2.handoffs);
    }

    /// (astra r4-r6) 인코딩 모호성 회귀 — 구분자 포함 필드·충돌 쌍·초장.
    #[test]
    fn manifest_encoding_unambiguous() {
        // 파이프/콜론 포함 필드도 유일 디코딩 (길이로만 경계 결정).
        let a = handoff_manifest_digest("a|b", "c", "d", 1).unwrap();
        let b = handoff_manifest_digest("a", "b|c", "d", 1).unwrap();
        assert_ne!(a, b, "delimiter-bearing fields must not collide");
        // r5 충돌 쌍: "1"+"11111111111" 십진 스트림 — 고정폭으로 불가.
        let c = handoff_manifest_digest("1", "11111111111", "x", 1).unwrap();
        let d = handoff_manifest_digest("11111111111", "1", "x", 1).unwrap();
        assert_ne!(c, d, "fixed-width lengths must defeat decimal-stream collisions");
        // 초장 필드 거부.
        let big = "x".repeat(100_000_000);
        assert!(handoff_manifest_digest(&big, "a", "b", 1).is_err());
    }

    /// row_at fail-closed — 부재 seq 는 Err.
    #[test]
    fn row_at_missing_fails_closed() {
        let log = OrchestrationLog::open(&tmp("rowat")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "s".into())],
        )
        .unwrap();
        assert!(log.row_at(99).is_err());
        assert!(log.row_at(1).is_ok());
    }

    /// 스트림 identity — 같은 root 재활성화는 별개 스트림 id 로 선언 가능.
    /// (astra r1 M1 회귀) 교차 플랜 덮어쓰기 사이클 — 주문 재선언 거부로 은닉 불가.
    #[test]
    fn cross_plan_overwrite_cycle_rejected() {
        let log = OrchestrationLog::open(&tmp("cross")).unwrap();
        log.declare_plan(
            "A",
            vec![("o-b".into(), vec!["o-c".into()]), ("o-c".into(), vec![])],
            vec![("o-b".into(), "s".into()), ("o-c".into(), "s".into())],
        )
        .unwrap();
        // o-c 재선언 시도 — DuplicateOrder 거부 (B={c→b} 가 A의 간선을 덮어써
        // 사이클을 숨기는 공격 불가).
        let e = log
            .declare_plan(
                "B",
                vec![("o-c".into(), vec!["o-b".into()])],
                vec![("o-c".into(), "s".into())],
            )
            .unwrap_err();
        assert!(matches!(e, OrchError::DuplicateOrder { .. }), "{e:?}");
        // 합법 교차 참조: 새 주문이 A의 주문을 선행으로.
        log.declare_plan(
            "B2",
            vec![("o-d".into(), vec!["o-b".into(), "o-c".into()])],
            vec![("o-d".into(), "s".into())],
        )
        .unwrap();
    }

    /// (astra r1 M2 회귀) 발행 스트림 불일치 거부 + 선언 플랜 불일치 거부.
    #[test]
    fn spawn_stream_and_plan_mismatch_rejected() {
        let log = OrchestrationLog::open(&tmp("streammm")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "s-declared".into())],
        )
        .unwrap();
        let e = log.spawn_order("o-a", "p1", "s-other").unwrap_err();
        assert!(matches!(e, OrchError::StreamMismatch { .. }), "{e:?}");
        let e2 = log.spawn_order("o-a", "p-wrong", "s-declared").unwrap_err();
        assert!(matches!(e2, OrchError::UndeclaredOrder { .. }), "{e2:?}");
        log.spawn_order("o-a", "p1", "s-declared").unwrap();
    }

    /// (astra r1 M3 회귀) seq 재번호화 — 행 삭제로 시퀀스가 당겨지면 감지.
    #[test]
    fn seq_renumbering_detected() {
        let path = tmp("renumber");
        {
            let log = OrchestrationLog::open(&path).unwrap();
            log.declare_plan("p1", vec![("o-a".into(), vec![])], vec![("o-a".into(), "s".into())]).unwrap();
            log.declare_plan("p2", vec![("o-b".into(), vec![])], vec![("o-b".into(), "s".into())]).unwrap();
        }
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER orch_no_update; DROP TRIGGER orch_no_delete;").unwrap();
        conn.execute("DELETE FROM orch WHERE seq = 1", []).unwrap();
        drop(conn);
        let log = OrchestrationLog::open(&path).unwrap();
        assert!(matches!(log.read_all(), Err(OrchError::ChainBroken { .. })));
    }

    /// (astra r1 게이트7) 거부 행이 섞인 재오픈 동등 + 거부 행 변조 감지.
    #[test]
    fn mixed_refusal_reopen_and_tamper() {
        let path = tmp("mixed");
        {
            let log = OrchestrationLog::open(&path).unwrap();
            log.declare_plan("p1", vec![("o-a".into(), vec![])], vec![("o-a".into(), "s".into())]).unwrap();
            crate::refusal::record_refusal(
                &log,
                crate::refusal::RefusalCode::HandoffMismatch,
                "req-9",
                None,
            )
            .unwrap();
            log.spawn_order("o-a", "p1", "s").unwrap();
        }
        let reopened = OrchestrationLog::open(&path).unwrap();
        let p1 = project(&reopened.read_all().unwrap());
        assert_eq!(p1.plans.len(), 1);
        assert_eq!(p1.spawned.len(), 1, "refusal row must not break projection");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER orch_no_update;").unwrap();
        conn.execute(
            "UPDATE orch SET payload = replace(payload, '매니페스트', 'TAMPERED') WHERE seq = 2",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(matches!(reopened.verify_chain(), Err(OrchError::ChainBroken { .. })));
    }

    /// (astra r1 M5 회귀) 검증 실패 후에도 로그 정상 사용 가능 (RAAI).
    #[test]
    fn failed_validation_leaves_log_usable() {
        let log = OrchestrationLog::open(&tmp("raai")).unwrap();
        log.declare_plan("p1", vec![("o-a".into(), vec![])], vec![("o-a".into(), "s".into())]).unwrap();
        let _ = log.declare_plan("p1", vec![("o-z".into(), vec![])], vec![("o-z".into(), "s".into())]);
        let _ = log.spawn_order("o-ghost", "p1", "s");
        let _ = log.spawn_order("o-a", "p1", "s-wrong");
        log.spawn_order("o-a", "p1", "s").unwrap();
        assert!(log.verify_chain().unwrap());
        assert_eq!(log.read_all().unwrap().len(), 2);
    }

    /// (astra r2 M1 회귀) 동시 declare_plan — 검증이 BEGIN IMMEDIATE 안에서
    /// 직렬화되어 경쟁 창이 없다. 단일 Mutex 커넥션이 물리적 직렬화를
    /// 제공하므로 스레드 경쟁에서도 유일성이 유지됨을 단정.
    #[test]
    fn concurrent_declarations_stay_unique() {
        let path = tmp("concurrent");
        let log = std::sync::Arc::new(OrchestrationLog::open(&path).unwrap());
        let mut handles = Vec::new();
        for i in 0..4 {
            let l = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                l.declare_plan(
                    &format!("p{i}"),
                    vec![(format!("o-t{i}"), vec![])],
                    vec![(format!("o-t{i}"), "s".into())],
                )
            }));
        }
        let ok = handles
            .into_iter()
            .map(|h| h.join().unwrap().is_ok())
            .filter(|r| *r)
            .count();
        assert_eq!(ok, 4, "distinct plans must all succeed");
        let rows = log.read_all().unwrap();
        assert_eq!(rows.len(), 4);
        // 같은 주문 id 로 경쟁 — 정확히 1개만 성공.
        let mut handles = Vec::new();
        for i in 0..4 {
            let l = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                l.declare_plan(
                    &format!("p-dup{i}"),
                    vec![("o-race".into(), vec![])],
                    vec![("o-race".into(), "s".into())],
                )
            }));
        }
        let ok2 = handles
            .into_iter()
            .map(|h| h.join().unwrap().is_ok())
            .filter(|r| *r)
            .count();
        assert_eq!(ok2, 1, "only one racer may claim a globally-unique order id");
        assert!(log.verify_chain().unwrap());
    }

    /// (astra r2 M5 회귀) preset_bound 공개 진입점 + 투영 4필드 보존.
    #[test]
    fn preset_bound_projection_preserves_all_fields() {
        let log = OrchestrationLog::open(&tmp("pb")).unwrap();
        log.declare_plan("p1", vec![("o-a".into(), vec![])], vec![("o-a".into(), "s".into())]).unwrap();
        log.append_preset_bound("o-size", "root-x", "dig-abc", "plan-epic-1").unwrap();
        let p = project(&log.read_all().unwrap());
        assert_eq!(p.preset_bounds.len(), 1);
        assert_eq!(
            p.preset_bounds[0],
            ("o-size".to_string(), "root-x".to_string(), "dig-abc".to_string(), "plan-epic-1".to_string())
        );
    }

    #[test]
    fn same_root_distinct_streams_allowed() {
        let log = OrchestrationLog::open(&tmp("streams")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![])],
            vec![("o-a".into(), "root-x#act-1".into())],
        )
        .unwrap();
        log.declare_plan(
            "p2",
            vec![("o-b".into(), vec![])],
            vec![("o-b".into(), "root-x#act-2".into())],
        )
        .unwrap();
        let p = project(&log.read_all().unwrap());
        assert_eq!(p.plans.len(), 2);
    }
}


// ── HandoffLogPort 구현 (M7 T57 — foundry-workflow 포트의 orch 측 어댑터) ──

/// foundry-workflow 가 오케스트레이션 로그를 보는 창 — 포트 계약 이행.
pub struct HandoffLogAdapter<'a>(pub &'a OrchestrationLog);

impl foundry_workflow::replay::HandoffLogPort for HandoffLogAdapter<'_> {
    fn handoff_row(
        &self,
        seq: i64,
    ) -> Result<foundry_workflow::replay::HandoffRow, foundry_workflow::replay::HandoffLookupError> {
        use foundry_workflow::replay::{HandoffLookupError as E, HandoffRow as R};
        let row = self.0.row_at(seq).map_err(|e| match e {
            OrchError::ChainBroken { at_seq } => E::ChainBroken { at_seq },
            OrchError::NoRow { seq } => E::NoRow { seq },
            other => E::Io(other.to_string()),
        })?;
        match row.kind {
            OrchestrationKind::HandoffBound { source_order, dest_order, artifact_digest } => {
                Ok(R { seq: row.seq, source_order, dest_order, artifact_digest })
            }
            _ => Err(E::NotAHandoff { seq: row.seq }),
        }
    }

    fn log_intact(&self) -> Result<bool, foundry_workflow::replay::HandoffLookupError> {
        use foundry_workflow::replay::HandoffLookupError as E;
        self.0.verify_chain().map_err(|e| match e {
            OrchError::ChainBroken { at_seq } => E::ChainBroken { at_seq },
            other => E::Io(other.to_string()),
        })
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;
    use foundry_workflow::replay::HandoffLogPort as _;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("foundry-orch-port-{name}-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// 포트 조회 — 행 있으면 전 필드 반환.
    #[test]
    fn port_returns_handoff_row() {
        let log = OrchestrationLog::open(&tmp("ok")).unwrap();
        log.declare_plan(
            "p1",
            vec![("o-a".into(), vec![]), ("o-b".into(), vec!["o-a".into()])],
            vec![("o-a".into(), "s".into()), ("o-b".into(), "s".into())],
        )
        .unwrap();
        let (row, _manifest) = log.bind_handoff("o-a", "o-b", "dig-art").unwrap();
        let adapter = HandoffLogAdapter(&log);
        let got = adapter.handoff_row(row.seq).unwrap();
        assert_eq!(got.source_order, "o-a");
        assert_eq!(got.dest_order, "o-b");
        assert_eq!(got.artifact_digest, "dig-art");
        assert!(adapter.log_intact().unwrap());
    }

    /// 포트 fail-closed — 부재 seq 는 NoRow, 비핸드오프 행은 NotAHandoff.
    #[test]
    fn port_fails_closed() {
        let log = OrchestrationLog::open(&tmp("fc")).unwrap();
        let decl = log
            .declare_plan(
                "p1",
                vec![("o-a".into(), vec![])],
                vec![("o-a".into(), "s".into())],
            )
            .unwrap();
        let adapter = HandoffLogAdapter(&log);
        use foundry_workflow::replay::HandoffLookupError as E;
        assert_eq!(adapter.handoff_row(99), Err(E::NoRow { seq: 99 }));
        assert_eq!(adapter.handoff_row(decl.seq), Err(E::NotAHandoff { seq: decl.seq }));
    }
}


/// (T58 테스트 지원) 저널 행에서 신뢰 커서 발급 — 게이트 common 의
/// replay_ctx_for 등가. foundry-workflow 공개 API 만 사용.
pub mod test_support {
    /// 터미널 커밋먼트 산출 후 **주문별 마지막 행 위치**로 커서 발급
    /// (다주문 저널: 전역 max_seq 가 요청 주문의 행이 아닐 수 있다 —
    /// T56 r3 커서 위치 핀 원칙).
    pub fn replay_ctx(
        order_id: &str,
        rows: &[foundry_ledger::LedgerRow],
    ) -> foundry_workflow::replay::ReplayContext {
        use foundry_ledger::LedgerRow;
        let global_max = rows.iter().map(|r| r.seq).max().unwrap_or(0);
        let chain_head = rows
            .iter()
            .find(|r| r.seq == global_max)
            .map(|r| r.hash.clone())
            .unwrap_or_else(|| "GENESIS".into());
        let count = rows.len() as i64;
        let commit = foundry_workflow::replay::TerminalCommitment {
            max_seq: global_max,
            count,
            chain_head,
        };
        // 이 주문의 마지막 행 — 없으면 GENESIS 부트스트랩.
        let my_last = rows
            .iter()
            .filter(|r| r.order_id == order_id)
            .map(|r| r.seq)
            .max();
        let cursor = match my_last {
            None => foundry_workflow::replay::VerifiedReplayCursor::genesis(order_id),
            Some(seq) => foundry_workflow::replay::issue_replay_cursor(rows, &commit, order_id, seq)
                .expect("cursor issuance"),
        };
        foundry_workflow::replay::ReplayContext { cursor }
    }
}
