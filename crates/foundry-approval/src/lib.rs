//! T53a — 승인 원장 (M6 플랜 v6).
//!
//! **스토어 분리 원칙**: GrantRecord·활성화 토큰은 이 별도 원장에 저장한다.
//! 주문 저널(foundry-ledger)에는 새 테이블·마이그레이션·신규 이벤트가
//! 전부 없다 — 레거시 재생 바이트는 **부재로** 보존된다 (억제가 아님).
//!
//! **인가 프로토콜 (플랜 v6 A′)**:
//! * `reserve_grant` — 원자적 **예약** (유일한 소비 경로 진입). fence_token
//!   (원장 단조 증가) 부여. 멱등: 동일 (grant, exec, seq) 재호출은 같은
//!   결과; 다른 키 충돌은 거부.
//! * 엔진 검증 체인 — 예약 보유 상태에서 잔여 검사. 실패 시
//!   `release_reservation` (멱등).
//! * `finalize_grant` — 도메인 전이 적용 **직후**. PENDING→CONSUMED +
//!   소비 기록. 멱등.
//! * `recovery_scan` — 원장 쓰기 잠금 하에 PENDING 예약 정리: 확정 흔적
//!   (finalize record) 있으면 CONSUMED 확정, 없으면 fence 무효화+릴리즈.
//!   회복-활성 디스패처 경쟁은 fence로 차단 (릴리즈된 예약의 옛 소유자
//!   확정 시도 = fence 불일치 거부, same-key 재시도 포함).
//!
//! **1토큰 1grant (D′)**: token_id PRIMARY KEY — 스키마 수준 강제.
//! grant 스코프는 scope_json(정규 JSON 동결) 전체 비교 — 부분 매칭 없음.
//!
//! **활성화 (B4)**: 활성화 토큰은 문서 Merkle root + 활성화 의도에 결속.
//! 같은 root·다른 의도 / 다른 root·같은 의도 / 주문 스토브 토큰 재사용
//! 전부 거부 (G13 4케이스).

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 정규 JSON — 키 정렬·결정적 직렬화 (scope_json 동결용).
pub fn canonical_scope_json(v: &serde_json::Value) -> String {
    // serde_json::Value는 이미 BTree 순서로 직렬화된다 (Map 기본 preserve_order
    // feature off) — 결정성 보장.
    serde_json::to_string(v).expect("value is serializable")
}

/// 승인 원장 오류.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerError {
    /// token_id 중복 발급 — 1토큰 1grant 스키마 강제.
    TokenAlreadyIssued { token_id: String },
    /// grant 없음 / 매칭 실패 (스코프 전체 비교).
    GrantNotFound { grant_id: String },
    /// 예약 충돌 — 그 grant는 다른 (exec, seq)에 예약됨.
    ReservationConflict { grant_id: String, held_by: (String, u64) },
    /// fence 불일치 — 릴리즈된(회복된) 예약의 옛 소유자 확정 시도.
    FenceStale { grant_id: String, your_fence: u64, current_fence: u64 },
    /// grant가 소비됨 (PENDING 아님).
    AlreadyConsumed { grant_id: String },
    /// 활성화 토큰 거부 (B4 4케이스).
    ActivationRefused { reason: String },
    /// 원장 I/O.
    Db(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::TokenAlreadyIssued { token_id } => write!(f, "token {token_id} already has a grant (1 token = 1 grant)"),
            LedgerError::GrantNotFound { grant_id } => write!(f, "no grant {grant_id} matching the full scope"),
            LedgerError::ReservationConflict { grant_id, held_by } => write!(f, "grant {grant_id} reserved by ({}, {})", held_by.0, held_by.1),
            LedgerError::FenceStale { grant_id, your_fence, current_fence } => write!(f, "grant {grant_id}: your fence {your_fence} is stale (current {current_fence}) — reservation was recovered"),
            LedgerError::AlreadyConsumed { grant_id } => write!(f, "grant {grant_id} already consumed"),
            LedgerError::ActivationRefused { reason } => write!(f, "activation refused: {reason}"),
            LedgerError::Db(e) => write!(f, "db: {e}"),
        }
    }
}
impl std::error::Error for LedgerError {}

/// GrantRecord — 승인 1건의 전 계약 (플랜 v6 D′).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantRecord {
    /// 원장 발급 불투명 식별자 (외부 문자열 정규화: NFKC·전후 공백 제거).
    pub grant_id: String,
    /// 인간 ApprovalToken id — PRIMARY KEY (1토큰 1grant).
    pub token_id: String,
    /// 스코프 (동결 정규 JSON: order_id·intent·artifact_digest·root_scope).
    pub scope_json: String,
    /// 1회용 — 소비 시 카운터 진행.
    pub nonce: u64,
    pub issued_at: DateTime<Utc>,
    pub state: GrantState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    /// 발급됨 — 예약 가능.
    Issued,
    /// 예약됨 — 소비 진행 중 (fence_token 보유).
    Pending,
    /// 소비 완료 — 재사용 불가.
    Consumed,
}

/// 예약 조회 결과.
#[derive(Debug, Clone, PartialEq)]
pub struct Reservation {
    pub grant_id: String,
    pub exec_id: String,
    pub dispatch_seq: u64,
    pub fence_token: u64,
}

/// **승인 원장** — SQLite. 모든 변경은 단일 트랜잭션 (fence 검사는
/// 트랜잭션 안 상태 변이 **앞에** — astra implementation-sized 지침).
pub struct ApprovalLedger {
    conn: Connection,
}

impl ApprovalLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let conn = Connection::open(path).map_err(|e| LedgerError::Db(e.to_string()))?;
        Self::init(conn)
    }

    /// 인메모리 원장 — 테스트·임베디드 진단용 (프로덕션은 open).
    pub fn open_in_memory() -> Result<Self, LedgerError> {
        let conn = Connection::open_in_memory().map_err(|e| LedgerError::Db(e.to_string()))?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, LedgerError> {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS grants (
                 grant_id   TEXT PRIMARY KEY,
                 token_id   TEXT NOT NULL UNIQUE,
                 scope_json TEXT NOT NULL,
                 nonce      INTEGER NOT NULL DEFAULT 0,
                 issued_at  TEXT NOT NULL,
                 state      TEXT NOT NULL DEFAULT 'issued',
                 fence_token INTEGER NOT NULL DEFAULT 0,
                 reserved_exec TEXT,
                 reserved_seq INTEGER
             );
             CREATE TABLE IF NOT EXISTS reservations (
                 grant_id TEXT PRIMARY KEY,
                 exec_id  TEXT NOT NULL,
                 dispatch_seq INTEGER NOT NULL,
                 fence_token INTEGER NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS finalize_records (
                 grant_id TEXT PRIMARY KEY,
                 exec_id  TEXT NOT NULL,
                 dispatch_seq INTEGER NOT NULL,
                 fence_token INTEGER NOT NULL,
                 finalized_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS activations (
                 root TEXT NOT NULL,
                 intent TEXT NOT NULL,
                 token_id TEXT NOT NULL UNIQUE,
                 activated_at TEXT NOT NULL,
                 PRIMARY KEY (root, intent)
             );
             CREATE TABLE IF NOT EXISTS fence_counter (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 next INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO fence_counter (id, next) VALUES (1, 1);
             COMMIT;",
        )
        .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(Self { conn })
    }

    // ------------------------------------------------------------------
    // 발급
    // ------------------------------------------------------------------

    /// grant 발급 — token_id UNIQUE가 1토큰 1grant를 스키마 수준에서
    /// 강제한다 (스코프 변경 우회 불가 — D′). 외부 token_id는 정규화 후 저장.
    pub fn issue_grant(
        &mut self,
        grant_id: &str,
        token_id: &str,
        scope: &serde_json::Value,
    ) -> Result<GrantRecord, LedgerError> {
        let token_id = Self::normalize_token_id(token_id);
        let scope_json = canonical_scope_json(scope);
        let now = Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO grants (grant_id, token_id, scope_json, issued_at, state)
                 VALUES (?1, ?2, ?3, ?4, 'issued')",
                params![grant_id, token_id, scope_json, now],
            )
            .map_err(|e| match &e {
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    LedgerError::TokenAlreadyIssued { token_id: token_id.clone() }
                }
                other => LedgerError::Db(other.to_string()),
            })?;
        self.get_grant(grant_id)
    }

    /// 외부 token_id 정규화 — NFKC·전후 공백 제거 (동일 표기 이중 등록 차단).
    /// astra T53-r1 MAJOR: trim만으론 'tok-A'/'tok-Ａ'(전각)가 별 토큰이
    /// 된다 — 계약대로 NFKC 정규화 후 저장.
    fn normalize_token_id(s: &str) -> String {
        use unicode_normalization::UnicodeNormalization as _;
        s.trim().nfkc().collect::<String>()
    }

    pub fn get_grant(&self, grant_id: &str) -> Result<GrantRecord, LedgerError> {
        let row = self
            .conn
            .query_row(
                "SELECT grant_id, token_id, scope_json, nonce, issued_at, state FROM grants WHERE grant_id = ?1",
                params![grant_id],
                |r| {
                    Ok(GrantRecord {
                        grant_id: r.get(0)?,
                        token_id: r.get(1)?,
                        scope_json: r.get(2)?,
                        nonce: r.get::<_, i64>(3)? as u64,
                        issued_at: DateTime::parse_from_rfc3339(&r.get::<_, String>(4)?).unwrap().with_timezone(&Utc),
                        state: match r.get::<_, String>(5)?.as_str() {
                            "issued" => GrantState::Issued,
                            "pending" => GrantState::Pending,
                            "consumed" => GrantState::Consumed,
                            _ => unreachable!(),
                        },
                    })
                },
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        row.ok_or_else(|| LedgerError::GrantNotFound { grant_id: grant_id.into() })
    }

    // ------------------------------------------------------------------
    // 예약 / 확정 / 회복 (플랜 v6 A′)
    // ------------------------------------------------------------------

    /// **1단계: 예약** — 유일한 소비 경로 진입. 원자적.
    /// * 전 스코프 매칭 (scope_json 문자열 동등 — 정규 JSON 전체 비교).
    /// * state=Issued 필요 (Pending/Consumed는 거부 — 경쟁 직렬화).
    /// * 멱등: 동일 (grant, exec, seq) 재호출 = 같은 fence 반환.
    /// * fence_token 단조 증가 부여.
    pub fn reserve_grant(
        &mut self,
        grant_id: &str,
        scope: &serde_json::Value,
        exec_id: &str,
        dispatch_seq: u64,
    ) -> Result<Reservation, LedgerError> {
        let scope_json = canonical_scope_json(scope);
        let tx = self.conn.transaction().map_err(|e| LedgerError::Db(e.to_string()))?;

        // fence 검사와 상태 변이가 같은 트랜잭션 — 검사가 먼저.
        let g: (String, String, String) = tx
            .query_row(
                "SELECT grant_id, scope_json, state FROM grants WHERE grant_id = ?1",
                params![grant_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?
            .ok_or_else(|| LedgerError::GrantNotFound { grant_id: grant_id.into() })?;

        if g.1 != scope_json {
            return Err(LedgerError::GrantNotFound { grant_id: grant_id.into() });
        }
        if g.2 == "consumed" {
            return Err(LedgerError::AlreadyConsumed { grant_id: grant_id.into() });
        }

        // 기존 예약 조회 — 멱등 vs 충돌.
        let existing: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT exec_id, dispatch_seq, fence_token FROM reservations WHERE grant_id = ?1",
                params![grant_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;

        let fence = match existing {
            Some((exec, seq, fence)) if exec == exec_id && seq as u64 == dispatch_seq => {
                // 멱등 재호출 — 같은 fence.
                fence as u64
            }
            Some((exec, seq, _)) => {
                return Err(LedgerError::ReservationConflict {
                    grant_id: grant_id.into(),
                    held_by: (exec, seq as u64),
                });
            }
            None => {
                // 신규 예약 — fence 발급.
                let fence: u64 = tx
                    .query_row("UPDATE fence_counter SET next = next + 1 WHERE id = 1 RETURNING next", [], |r| r.get::<_, i64>(0))
                    .map_err(|e| LedgerError::Db(e.to_string()))? as u64;
                let now = Utc::now().to_rfc3339();
                tx.execute(
                    "INSERT INTO reservations (grant_id, exec_id, dispatch_seq, fence_token, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![grant_id, exec_id, dispatch_seq as i64, fence as i64, now],
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
                tx.execute(
                    "UPDATE grants SET state = 'pending', reserved_exec = ?1, reserved_seq = ?2, fence_token = ?3 WHERE grant_id = ?4",
                    params![exec_id, dispatch_seq as i64, fence as i64, grant_id],
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
                fence
            }
        };

        tx.commit().map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(Reservation { grant_id: grant_id.into(), exec_id: exec_id.into(), dispatch_seq, fence_token: fence })
    }

    /// **예약 재검증 (T53a-r1 BLOCKER 보조)** — apply 직전 호출. 예약 행이
    /// 여전히 (grant, exec, seq, fence)로 유효한가 — 회복이 릴리즈했거나
    /// 다른 소유자가 잡았으면 Err. 예약→apply 사이 창을 닫는다.
    pub fn verify_reservation(
        &self,
        grant_id: &str,
        exec_id: &str,
        dispatch_seq: u64,
        fence_token: u64,
    ) -> Result<(), LedgerError> {
        let rf: Option<i64> = self
            .conn
            .query_row(
                "SELECT fence_token FROM reservations WHERE grant_id = ?1 AND exec_id = ?2 AND dispatch_seq = ?3",
                params![grant_id, exec_id, dispatch_seq as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        match rf {
            Some(f) if f == fence_token as i64 => Ok(()),
            _ => Err(LedgerError::FenceStale {
                grant_id: grant_id.into(),
                your_fence: fence_token,
                current_fence: rf.unwrap_or(0) as u64,
            }),
        }
    }

    /// **2단계 보조: 예약 해제** — 엔진 검증 실패 시. 멱등 (이미 없으면 Ok).
    /// fence 무효화: 예약 행 삭제 + grants.fence_token 증가 (옛 fence는
    /// 영구히 무효 — 회복과 동일 규율).
    pub fn release_reservation(&mut self, grant_id: &str, exec_id: &str, dispatch_seq: u64) -> Result<(), LedgerError> {
        let tx = self.conn.transaction().map_err(|e| LedgerError::Db(e.to_string()))?;
        Self::release_inner(&tx, grant_id, exec_id, dispatch_seq)?;
        tx.commit().map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(())
    }

    fn release_inner(tx: &rusqlite::Transaction, grant_id: &str, exec_id: &str, dispatch_seq: u64) -> Result<(), LedgerError> {
        let owned: Option<i64> = tx
            .query_row(
                "SELECT fence_token FROM reservations WHERE grant_id = ?1 AND exec_id = ?2 AND dispatch_seq = ?3",
                params![grant_id, exec_id, dispatch_seq as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        match owned {
            None => Ok(()), // 멱등 — 이미 없음.
            Some(_) => {
                // fence 무효화 (옛 소유자의 확정 시도 영구 거부) + 상태 복귀.
                tx.execute(
                    "UPDATE fence_counter SET next = next + 1 WHERE id = 1",
                    [],
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
                tx.execute(
                    "UPDATE grants SET fence_token = (SELECT next FROM fence_counter WHERE id = 1), state = 'issued', reserved_exec = NULL, reserved_seq = NULL WHERE grant_id = ?1",
                    params![grant_id],
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
                tx.execute("DELETE FROM reservations WHERE grant_id = ?1", params![grant_id])
                    .map_err(|e| LedgerError::Db(e.to_string()))?;
                Ok(())
            }
        }
    }

    /// **3단계: 확정** — 도메인 전이 적용 직후 호출. 멱등 (동일 키 재호출 성공).
    /// fence 검사: 자기 fence가 grants.fence_token과 일치해야 (회복된 예약은
    /// 이미 fence가 증가 — 거부).
    pub fn finalize_grant(
        &mut self,
        grant_id: &str,
        exec_id: &str,
        dispatch_seq: u64,
        fence_token: u64,
    ) -> Result<(), LedgerError> {
        let tx = self.conn.transaction().map_err(|e| LedgerError::Db(e.to_string()))?;

        // 멱등: 이미 확정 기록 있으면 성공.
        let done: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM finalize_records WHERE grant_id = ?1 AND exec_id = ?2 AND dispatch_seq = ?3",
                params![grant_id, exec_id, dispatch_seq as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        if done.is_some() {
            tx.commit().map_err(|e| LedgerError::Db(e.to_string()))?;
            return Ok(());
        }

        // 예약 소유권 검사 (astra T53-r1 MAJOR: fence 0 초기값 우회 폐쇄) —
        // (grant, exec, seq) 예약 행이 존재하고 그 fence가 인자와 일치해야.
        let reserved_fence: Option<i64> = tx
            .query_row(
                "SELECT fence_token FROM reservations WHERE grant_id = ?1 AND exec_id = ?2 AND dispatch_seq = ?3",
                params![grant_id, exec_id, dispatch_seq as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        let current_fence: i64 = tx
            .query_row("SELECT fence_token FROM grants WHERE grant_id = ?1", params![grant_id], |r| r.get(0))
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?
            .ok_or_else(|| LedgerError::GrantNotFound { grant_id: grant_id.into() })?;
        match reserved_fence {
            // 예약 없이 fence 0을 제시하는 우회 — 명시적 거부.
            None if fence_token == 0 => {
                return Err(LedgerError::ReservationConflict {
                    grant_id: grant_id.into(),
                    held_by: (String::new(), 0),
                });
            }
            Some(rf) if rf == fence_token as i64 => { /* 소유권 확인 — 진행 */ }
            _ => {
                return Err(LedgerError::FenceStale {
                    grant_id: grant_id.into(),
                    your_fence: fence_token,
                    current_fence: current_fence as u64,
                });
            }
        }
        if current_fence != fence_token as i64 {
            return Err(LedgerError::FenceStale {
                grant_id: grant_id.into(),
                your_fence: fence_token,
                current_fence: current_fence as u64,
            });
        }

        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO finalize_records (grant_id, exec_id, dispatch_seq, fence_token, finalized_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![grant_id, exec_id, dispatch_seq as i64, fence_token as i64, now],
        )
        .map_err(|e| LedgerError::Db(e.to_string()))?;
        tx.execute(
            "UPDATE grants SET state = 'consumed', nonce = nonce + 1, reserved_exec = NULL, reserved_seq = NULL WHERE grant_id = ?1",
            params![grant_id],
        )
        .map_err(|e| LedgerError::Db(e.to_string()))?;
        tx.execute("DELETE FROM reservations WHERE grant_id = ?1", params![grant_id])
            .map_err(|e| LedgerError::Db(e.to_string()))?;

        tx.commit().map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(())
    }

    /// **회복 스캔 (A′)** — 원장 쓰기 잠금(트랜잭션) 하에 PENDING 예약 정리.
    /// * finalize_records에 확정 흔적 → CONSUMED 확정 (apply 중복 없음 —
    ///   확정은 fence 하에 1회).
    /// * 흔적 없음 → fence 무효화 + 릴리즈 (재소비 가능).
    /// 회복-활성 디스패처 경쟁: 회복 커밋과 디스패처 예약/확정은 같은
    /// SQLite 쓰기 잠금으로 직렬화 — 회복이 릴리즈한 뒤 옛 fence로 오는
    /// 확정은 FenceStale.
    pub fn recovery_scan(&mut self) -> Result<Vec<String>, LedgerError> {
        // 즉시 쓰기 잠금 — 회복 시작부터 커밋까지 다른 쓰기(디스패처 예약/
        // 확정)가 끼지 못한다 (astra T53-r1 BLOCKER: deferred 트랜잭션은
        // 잠금을 읽기에서 시작해 경쟁 창을 남긴다).
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        let pending: Vec<(String, String, i64)> = {
            let mut stmt = tx
                .prepare("SELECT grant_id, exec_id, dispatch_seq FROM reservations")
                .map_err(|e| LedgerError::Db(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(|e| LedgerError::Db(e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| LedgerError::Db(e.to_string()))?;
            rows
        };

        let mut finalized = Vec::new();
        let mut released = Vec::new();
        for (grant_id, exec_id, seq) in pending {
            let has_evidence: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM finalize_records WHERE grant_id = ?1 AND exec_id = ?2 AND dispatch_seq = ?3",
                    params![grant_id, exec_id, seq],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| LedgerError::Db(e.to_string()))?;
            if has_evidence.is_some() {
                // 증거 있음 — CONSUMED로 확정 정리.
                tx.execute(
                    "UPDATE grants SET state = 'consumed', reserved_exec = NULL, reserved_seq = NULL WHERE grant_id = ?1",
                    params![grant_id],
                )
                .map_err(|e| LedgerError::Db(e.to_string()))?;
                tx.execute("DELETE FROM reservations WHERE grant_id = ?1", params![grant_id])
                    .map_err(|e| LedgerError::Db(e.to_string()))?;
                finalized.push(grant_id);
            } else {
                // 증거 없음 — 릴리즈 (fence 무효화 포함).
                Self::release_inner(&tx, &grant_id, &exec_id, seq as u64)?;
                released.push(grant_id);
            }
        }
        tx.commit().map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(released)
    }

    // ------------------------------------------------------------------
    // 활성화 (B4 — 플랜 v6: 문서 root + 활성화 의도 결속)
    // ------------------------------------------------------------------

    /// 워크플로 활성화 — 인간 토큰이 (root, intent)에 정확히 결속됐는지.
    /// **거부 케이스 (G13 4종)**: 무토큰 / 다른 root / 다른 의도 /
    /// **주문 스코프 토큰 재사용** (주문 승인용 token_id는 activations에
    /// 못 들어간다 — 별도 결속 요구, D′: 신규 토큰은 id 필수).
    pub fn activate_workflow(
        &mut self,
        root: &str,
        intent: &str,
        token_id: &str,
    ) -> Result<(), LedgerError> {
        let token_id = Self::normalize_token_id(token_id);
        if token_id.is_empty() {
            return Err(LedgerError::ActivationRefused { reason: "no activation token (B4: activation requires a human ApprovalToken)".into() });
        }
        // 주문 스토브 토큰 재사용 차단: 그 token_id가 이미 grant로 쓰였으면
        // 활성화 결속으로 재사용 불가 (결속 스코프가 다르다).
        let used_as_grant: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM grants WHERE token_id = ?1", params![token_id], |r| r.get(0))
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        if used_as_grant.is_some() {
            return Err(LedgerError::ActivationRefused {
                reason: format!("token {token_id} is order-scoped (used as a grant) — activation requires a root+intent-bound token"),
            });
        }
        let now = Utc::now().to_rfc3339();
        match self.conn.execute(
            "INSERT INTO activations (root, intent, token_id, activated_at) VALUES (?1, ?2, ?3, ?4)",
            params![root, intent, token_id, now],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                // token_id UNIQUE 위반 — 두 가능성: 같은 결속 재실행(멱등 성공)
                // 또는 같은 토큰의 다른 (root,intent) 결속(거부).
                let already_same: Option<String> = self
                    .conn
                    .query_row(
                        "SELECT token_id FROM activations WHERE root = ?1 AND intent = ?2",
                        params![root, intent],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| LedgerError::Db(e.to_string()))?;
                if already_same.as_deref() == Some(token_id.as_str()) {
                    Ok(())
                } else {
                    Err(LedgerError::ActivationRefused {
                        reason: format!("token {token_id} is already bound to a different (root, intent) activation — one token, one binding"),
                    })
                }
            }
            Err(other) => Err(LedgerError::Db(other.to_string())),
        }
    }

    /// 활성화 여부 — (root, intent) 결속 확인.
    pub fn is_activated(&self, root: &str, intent: &str) -> Result<bool, LedgerError> {
        let row: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM activations WHERE root = ?1 AND intent = ?2",
                params![root, intent],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        Ok(row.is_some())
    }

    /// 활성화 검증 — 호출자가 주장하는 (root, intent, token)이 원장 기록과
    /// 정확히 일치하는가 (다른 root·다른 의도 = 거부).
    pub fn verify_activation(&self, root: &str, intent: &str, token_id: &str) -> Result<(), LedgerError> {
        let token_id = Self::normalize_token_id(token_id);
        let recorded: Option<String> = self
            .conn
            .query_row(
                "SELECT token_id FROM activations WHERE root = ?1 AND intent = ?2",
                params![root, intent],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LedgerError::Db(e.to_string()))?;
        match recorded {
            Some(t) if t == token_id => Ok(()),
            Some(_) => Err(LedgerError::ActivationRefused {
                reason: format!("activation of ({root}, {intent}) is bound to a DIFFERENT token"),
            }),
            None => Err(LedgerError::ActivationRefused {
                reason: format!("({root}, {intent}) is not activated — no human token on record"),
            }),
        }
    }
}

/// grant 스코프 값 편의 생성자 (정규 JSON — 전 스코프 비교).
pub fn grant_scope(order_id: &str, intent: &str, artifact_digest: &str, root_scope: &str) -> serde_json::Value {
    serde_json::json!({
        "order_id": order_id,
        "intent": intent,
        "artifact_digest": artifact_digest,
        "root_scope": root_scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> ApprovalLedger {
        ApprovalLedger::open_in_memory().unwrap()
    }

    fn scope_a() -> serde_json::Value {
        grant_scope("o-1", "ship", "dig-abc", "wf-root-1")
    }

    // ------------------------------------------------------------------
    // G3 실행 케이스 ②: 무grant 승인 → AuthorizationRefused (원장 관점:
    // 매칭 실패). ③: 유효하지만 다른 스코프의 grant → 거부.
    // ------------------------------------------------------------------

    #[test]
    fn g3_case2_no_grant_matching_fails() {
        let mut l = ledger();
        let err = l.reserve_grant("g-ghost", &scope_a(), "ex-1", 1).unwrap_err();
        assert!(matches!(err, LedgerError::GrantNotFound { .. }));
    }

    #[test]
    fn g3_case3_wrong_scope_grant_refused() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        // 다른 스코프(타 root)로 예약 시도 — 전체 비교 실패.
        let wrong = grant_scope("o-1", "ship", "dig-abc", "wf-root-OTHER");
        let err = l.reserve_grant("g-1", &wrong, "ex-1", 1).unwrap_err();
        assert!(matches!(err, LedgerError::GrantNotFound { .. }), "{err:?}");
        // 다른 order도 거부.
        let wrong2 = grant_scope("o-OTHER", "ship", "dig-abc", "wf-root-1");
        assert!(matches!(l.reserve_grant("g-1", &wrong2, "ex-1", 1), Err(LedgerError::GrantNotFound { .. })));
    }

    // 1토큰 1grant — 스키마 강제 (D′).
    #[test]
    fn d_prime_one_token_one_grant() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        // 같은 토큰으로 다른 grant 발급 — UNIQUE 위반.
        let err = l.issue_grant("g-2", "tok-1", &grant_scope("o-2", "ship", "d", "r")).unwrap_err();
        assert!(matches!(err, LedgerError::TokenAlreadyIssued { .. }));
        // token_id 정규화: " tok-1 "도 동일 토큰으로 인식 (이중 등록 차단).
        let err = l.issue_grant("g-3", "  tok-1  ", &grant_scope("o-3", "s", "d", "r")).unwrap_err();
        assert!(matches!(err, LedgerError::TokenAlreadyIssued { .. }));
    }

    // 예약/확정 라이프사이클 — 멱등성.
    #[test]
    fn reserve_finalize_idempotent() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        let r1 = l.reserve_grant("g-1", &scope_a(), "ex-1", 7).unwrap();
        let r2 = l.reserve_grant("g-1", &scope_a(), "ex-1", 7).unwrap();
        assert_eq!(r1, r2, "same-key reservation retry must return the SAME fence");
        // 다른 키 충돌.
        assert!(matches!(
            l.reserve_grant("g-1", &scope_a(), "ex-OTHER", 7),
            Err(LedgerError::ReservationConflict { .. })
        ));
        // 확정 — 멱등.
        l.finalize_grant("g-1", "ex-1", 7, r1.fence_token).unwrap();
        l.finalize_grant("g-1", "ex-1", 7, r1.fence_token).unwrap();
        // 소비 후 재예약 거부.
        assert!(matches!(
            l.reserve_grant("g-1", &scope_a(), "ex-2", 1),
            Err(LedgerError::AlreadyConsumed { .. })
        ));
    }

    // 회복-디스패처 경쟁 — fence가 옛 소유자를 차단 (A′ BLOCKER 폐쇄).
    #[test]
    fn recovery_races_dispatcher_fence_blocks_stale_owner() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        // X 예약 → 회복이 릴리즈 → Y 예약 → X의 확정 시도는 fence 거부.
        let x = l.reserve_grant("g-1", &scope_a(), "ex-X", 1).unwrap();
        l.release_reservation("g-1", "ex-X", 1).unwrap();
        let y = l.reserve_grant("g-1", &scope_a(), "ex-Y", 1).unwrap();
        assert_ne!(x.fence_token, y.fence_token, "new reservation must get a NEW fence");
        let err = l.finalize_grant("g-1", "ex-X", 1, x.fence_token).unwrap_err();
        assert!(
            matches!(&err, LedgerError::FenceStale { your_fence, current_fence, .. }
                if *your_fence == x.fence_token && *current_fence != x.fence_token),
            "stale owner must be fence-refused: {err:?}"
        );
        // same-key가 아닌 재예약 시도 (Y가 보유 중) — 충돌 거부.
        assert!(matches!(
            l.reserve_grant("g-1", &scope_a(), "ex-X", 2),
            Err(LedgerError::ReservationConflict { .. })
        ));
    }

    // 회복 스캔 — 증거 있으면 CONSUMED, 없으면 릴리즈.
    #[test]
    fn recovery_scan_evidence_semantics() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        l.issue_grant("g-2", "tok-2", &grant_scope("o-2", "ship", "d2", "r2")).unwrap();
        // g-1: 예약+확정 (전이 완료). g-2: 예약만 (크래시 — 증거 없음).
        let r1 = l.reserve_grant("g-1", &scope_a(), "ex-1", 1).unwrap();
        l.finalize_grant("g-1", "ex-1", 1, r1.fence_token).unwrap();
        l.reserve_grant("g-2", &grant_scope("o-2", "ship", "d2", "r2"), "ex-1", 1).unwrap();
        let released = l.recovery_scan().unwrap();
        assert_eq!(released, vec!["g-2".to_string()], "only the evidence-less reservation is released");
        assert_eq!(l.get_grant("g-1").unwrap().state, GrantState::Consumed);
        assert_eq!(l.get_grant("g-2").unwrap().state, GrantState::Issued, "released grant returns to Issued (re-reservable)");
        // 재예약 가능.
        assert!(l.reserve_grant("g-2", &grant_scope("o-2", "ship", "d2", "r2"), "ex-9", 1).is_ok());
    }

    // release 멱등성 — 이중 해제 무해.
    #[test]
    fn release_is_idempotent() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        l.reserve_grant("g-1", &scope_a(), "ex-1", 1).unwrap();
        l.release_reservation("g-1", "ex-1", 1).unwrap();
        l.release_reservation("g-1", "ex-1", 1).unwrap(); // 멱등
        assert_eq!(l.get_grant("g-1").unwrap().state, GrantState::Issued);
    }

    // G13 4케이스 — 활성화.
    #[test]
    fn g13_activation_four_cases() {
        let mut l = ledger();
        // ① 무토큰 거부.
        assert!(matches!(l.activate_workflow("root-1", "activate", ""), Err(LedgerError::ActivationRefused { .. })));
        // ② 정상 — root+intent 결속 토큰.
        l.activate_workflow("root-1", "activate", "tok-act-1").unwrap();
        // ③ 다른 root / 다른 의도 — 미활성화.
        assert!(!l.is_activated("root-OTHER", "activate").unwrap());
        assert!(!l.is_activated("root-1", "other-intent").unwrap());
        // 검증: 정확 토큰만 통과.
        l.verify_activation("root-1", "activate", "tok-act-1").unwrap();
        assert!(matches!(
            l.verify_activation("root-1", "activate", "tok-act-2"),
            Err(LedgerError::ActivationRefused { .. })
        ));
        // ④ 주문 스코프 토큰 재사용 — grant로 쓰인 토큰은 활성화 불가.
        l.issue_grant("g-order", "tok-order-1", &scope_a()).unwrap();
        assert!(matches!(
            l.activate_workflow("root-2", "activate", "tok-order-1"),
            Err(LedgerError::ActivationRefused { .. })
        ));
    }

    // (astra T53-r1 MAJOR 6) NFKC — 전각/반각 동일 토큰 이중 등록 차단.
    #[test]
    fn t53r1_nfkc_normalization() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-A", &scope_a()).unwrap();
        // 전각 Ａ (U+FF21) — NFKC 후 'A'와 동일 → 이중 등록 거부.
        let err = l.issue_grant("g-2", "tok-Ａ", &grant_scope("o-2", "s", "d", "r")).unwrap_err();
        assert!(matches!(err, LedgerError::TokenAlreadyIssued { .. }), "{err:?}");
    }

    // (astra T53-r1 MAJOR 6) 1토큰 1활성화 결속 — 같은 토큰이 다른
    // (root, intent)에 재결속 시도 거부. 같은 결속 재실행은 멱등.
    #[test]
    fn t53r1_activation_token_unique_binding() {
        let mut l = ledger();
        l.activate_workflow("root-1", "intent-1", "tok-act").unwrap();
        // 같은 결속 재실행 — 멱드 성공.
        l.activate_workflow("root-1", "intent-1", "tok-act").unwrap();
        // 같은 토큰, 다른 결속 — 거부.
        let err = l.activate_workflow("root-2", "intent-2", "tok-act").unwrap_err();
        assert!(matches!(err, LedgerError::ActivationRefused { .. }), "{err:?}");
        // 다른 (root,intent)는 미활성화.
        assert!(!l.is_activated("root-2", "intent-2").unwrap());
    }

    // (astra T53-r1 MAJOR 2) finalize 우회 폐쇄 — 예약 없는 (grant, exec,
    // seq) + fence 0 제시로 CONSUMED 만들 수 없다.
    #[test]
    fn t53r1_finalize_requires_reservation_ownership() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        // 예약 없이 즉시 finalize — 거부.
        let err = l.finalize_grant("g-1", "arbitrary-exec", 1, 0).unwrap_err();
        assert!(
            matches!(err, LedgerError::ReservationConflict { .. } | LedgerError::FenceStale { .. }),
            "{err:?}"
        );
        assert_eq!(l.get_grant("g-1").unwrap().state, GrantState::Issued, "bypass attempt must NOT consume");
    }

    // (T53a-r1 BLOCKER 보조) verify_reservation — 해제된 예약은 apply 직전 검증 실패.
    #[test]
    fn t53r1_verify_reservation_catches_release() {
        let mut l = ledger();
        l.issue_grant("g-1", "tok-1", &scope_a()).unwrap();
        let r = l.reserve_grant("g-1", &scope_a(), "ex-1", 5).unwrap();
        // 아직 유효.
        l.verify_reservation("g-1", "ex-1", 5, r.fence_token).unwrap();
        // 해제 후 — 실패.
        l.release_reservation("g-1", "ex-1", 5).unwrap();
        assert!(l.verify_reservation("g-1", "ex-1", 5, r.fence_token).is_err());
    }

    // 정규 스코프 — 키 순서 무관 동결 비교.
    #[test]
    fn canonical_scope_is_order_insensitive() {
        let v1: serde_json::Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let v2: serde_json::Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(canonical_scope_json(&v1), canonical_scope_json(&v2));
    }
}
