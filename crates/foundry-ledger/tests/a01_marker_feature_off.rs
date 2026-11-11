#![cfg(not(feature = "a01-harness-support"))]

use foundry_ledger::{ProductConfig, ProductError, ProductStore, SystemProductClock};
use rusqlite::Connection;
use std::{path::PathBuf, sync::Arc};

// Synthetic discriminator fixture; this does not exercise a historical binary.
#[test]
fn a01_discriminator_is_rejected_feature_off_before_mutation() {
    let dir = PathBuf::from("/home/cosmo/.hermes/cache/scratch").join(format!(
        "a01-marker-off-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("marker.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE product_meta_v1(singleton INTEGER PRIMARY KEY, schema_version INTEGER NOT NULL); INSERT INTO product_meta_v1 VALUES(1,2561);").unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let config = || ProductConfig { installation_id: "test-installation".into(), database_id: "test-database".into() };
    assert!(matches!(ProductStore::open(&path, config()), Err(ProductError::ContractMismatch)), "ordinary opener must reject A01");
    assert!(std::fs::read(&path).unwrap() == before, "database changed during rejection");
    assert!(matches!(ProductStore::open_with_clock(&path, config(), Arc::new(SystemProductClock)), Err(ProductError::ContractMismatch)), "clock opener must reject A01");
    assert!(std::fs::read(&path).unwrap() == before, "database changed during rejection");
    assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "directory changed during rejection");
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    b0_observer::dispatch();
}

// Process-global instrumentation is confined to the controlled evidence child.
mod b0_observer {
    use super::*;
    use rusqlite::ffi;
    use std::{ffi::{c_char, c_int, c_void, CString}, fs, os::unix::fs::{DirBuilderExt, MetadataExt}, path::{Path, PathBuf}, pin::Pin, ptr, sync::atomic::{AtomicUsize, Ordering}};

    #[derive(Default)]
    struct Counts { wr: AtomicUsize, ws: AtomicUsize, rr: AtomicUsize, rs: AtomicUsize, aux: AtomicUsize, del: AtomicUsize }
    #[derive(Clone, Copy)]
    struct Observed { wr: usize, ws: usize, rr: usize, rs: usize, aux: usize, del: usize }
    impl Counts {
        fn take(&self) -> Observed {
            let take = |n: &AtomicUsize| n.swap(0, Ordering::Relaxed);
            Observed { wr: take(&self.wr), ws: take(&self.ws), rr: take(&self.rr), rs: take(&self.rs), aux: take(&self.aux), del: take(&self.del) }
        }
    }
    #[repr(C)]
    struct Proxy { vfs: ffi::sqlite3_vfs, original: *mut ffi::sqlite3_vfs, counts: Counts, name: CString }
    // SQLite supplies the registered VFS pointer. The repr(C) first field makes
    // it also the proxy address; the pinned allocation outlives registration.
    unsafe fn proxy<'a>(v: *mut ffi::sqlite3_vfs) -> &'a Proxy {
        // SQLite passes the registered, pinned first field of Proxy.
        unsafe { &*(v as *const Proxy) }
    }
    unsafe extern "C" fn open(v: *mut ffi::sqlite3_vfs, name: ffi::sqlite3_filename, file: *mut ffi::sqlite3_file, flags: c_int, out: *mut c_int) -> c_int {
        // SQLite supplies valid callback arguments; installation checks the original callback.
        unsafe {
        let p = proxy(v);
        let main = flags & ffi::SQLITE_OPEN_MAIN_DB != 0;
        let writable = flags & ffi::SQLITE_OPEN_READWRITE != 0;
        let readonly = flags & ffi::SQLITE_OPEN_READONLY != 0;
        if main && writable { p.counts.wr.fetch_add(1, Ordering::Relaxed); }
        if main && readonly { p.counts.rr.fetch_add(1, Ordering::Relaxed); }
        if !main { p.counts.aux.fetch_add(1, Ordering::Relaxed); }
        let rc = ((*p.original).xOpen.unwrap_unchecked())(p.original, name, file, flags, out);
        if rc == ffi::SQLITE_OK && !out.is_null() && main {
            if writable && *out & ffi::SQLITE_OPEN_READWRITE != 0 { p.counts.ws.fetch_add(1, Ordering::Relaxed); }
            if readonly && *out & ffi::SQLITE_OPEN_READONLY != 0 { p.counts.rs.fetch_add(1, Ordering::Relaxed); }
        }
        rc
        }
    }
    unsafe extern "C" fn delete(v: *mut ffi::sqlite3_vfs, name: *const c_char, sync: c_int) -> c_int {
        // The registered proxy and checked original callback remain live until cleanup.
        unsafe {
            let p = proxy(v); p.counts.del.fetch_add(1, Ordering::Relaxed);
            ((*p.original).xDelete.unwrap_unchecked())(p.original, name, sync)
        }
    }
    macro_rules! forward {
        ($fn:ident, $field:ident, ($($arg:ident : $ty:ty),*) -> $ret:ty) => {
            unsafe extern "C" fn $fn(v: *mut ffi::sqlite3_vfs, $($arg: $ty),*) -> $ret {
                // Only callbacks present on the original are replaced; both VFSes stay live.
                unsafe { let p = proxy(v); ((*p.original).$field.unwrap_unchecked())(p.original, $($arg),*) }
            }
        };
    }
    forward!(access, xAccess, (n: *const c_char, f: c_int, o: *mut c_int) -> c_int);
    forward!(full, xFullPathname, (n: *const c_char, l: c_int, o: *mut c_char) -> c_int);
    forward!(dlopen, xDlOpen, (n: *const c_char) -> *mut c_void);
    forward!(dlerror, xDlError, (n: c_int, o: *mut c_char) -> ());
    forward!(dlsym, xDlSym, (h: *mut c_void, n: *const c_char) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)>);
    forward!(dlclose, xDlClose, (h: *mut c_void) -> ());
    forward!(random, xRandomness, (n: c_int, o: *mut c_char) -> c_int);
    forward!(sleep, xSleep, (n: c_int) -> c_int);
    forward!(time, xCurrentTime, (o: *mut f64) -> c_int);
    forward!(error, xGetLastError, (n: c_int, o: *mut c_char) -> c_int);
    forward!(time64, xCurrentTimeInt64, (o: *mut ffi::sqlite3_int64) -> c_int);
    forward!(setcall, xSetSystemCall, (n: *const c_char, f: ffi::sqlite3_syscall_ptr) -> c_int);
    forward!(getcall, xGetSystemCall, (n: *const c_char) -> ffi::sqlite3_syscall_ptr);
    forward!(nextcall, xNextSystemCall, (n: *const c_char) -> *const c_char);

    struct Guard { proxy: Option<Pin<Box<Proxy>>>, registered: bool }
    impl Guard {
        unsafe fn install() -> Result<Self, &'static str> {
            // The isolated caller excludes concurrent SQLite use; registry pointers remain live.
            unsafe {
            let original = ffi::sqlite3_vfs_find(ptr::null());
            if original.is_null() { return Err("B0_SETUP_VFS"); }
            let o = &*original;
            if o.xOpen.is_some_and(|f| std::ptr::fn_addr_eq(f, open as unsafe extern "C" fn(*mut ffi::sqlite3_vfs, ffi::sqlite3_filename, *mut ffi::sqlite3_file, c_int, *mut c_int) -> c_int)) { return Err("B0_SETUP_RECURSION"); }
            if o.iVersion != 3 || o.xOpen.is_none() || o.xDelete.is_none() || o.xAccess.is_none() || o.xFullPathname.is_none() || o.xRandomness.is_none() || o.xSleep.is_none() || o.xCurrentTime.is_none() { return Err("B0_SETUP_UNSUPPORTED_VFS"); }
            let name = CString::new(format!("b0-a01-observer-{}", std::process::id())).map_err(|_| "B0_SETUP_NAME")?;
            if !ffi::sqlite3_vfs_find(name.as_ptr()).is_null() { return Err("B0_SETUP_COLLISION"); }
            let mut vfs = *o;
            vfs.pNext = ptr::null_mut();
            vfs.xOpen = Some(open); vfs.xDelete = Some(delete);
            macro_rules! replace { ($($field:ident => $callback:ident),*) => { $(if o.$field.is_some() { vfs.$field = Some($callback); })* }; }
            replace!(xAccess => access, xFullPathname => full, xDlOpen => dlopen, xDlError => dlerror, xDlSym => dlsym, xDlClose => dlclose, xRandomness => random, xSleep => sleep, xCurrentTime => time, xGetLastError => error, xCurrentTimeInt64 => time64, xSetSystemCall => setcall, xGetSystemCall => getcall, xNextSystemCall => nextcall);
            let mut p = Box::pin(Proxy { vfs, original, counts: Counts::default(), name });
            let raw = &mut p.as_mut().get_unchecked_mut().vfs as *mut ffi::sqlite3_vfs;
            (*raw).zName = p.name.as_ptr();
            if raw == original { return Err("B0_SETUP_RECURSION"); }
            let mut guard = Guard { proxy: Some(p), registered: true };
            let rc = ffi::sqlite3_vfs_register(raw, 1);
            if rc != ffi::SQLITE_OK || ffi::sqlite3_vfs_find((*raw).zName) != raw || ffi::sqlite3_vfs_find(ptr::null()) != raw {
                if !guard.cleanup() { return Err("B0_CLEANUP_FAILURE"); }
                return Err("B0_SETUP_REGISTER");
            }
            Ok(guard)
            }
        }
        fn counts(&self) -> &Counts { &self.proxy.as_ref().unwrap().counts }
        unsafe fn cleanup(&mut self) -> bool {
            // All fixture handles are closed on the normal path; uncertain registry state is leaked.
            unsafe {
            if !self.registered { return true; }
            let p = self.proxy.as_mut().unwrap().as_mut().get_unchecked_mut();
            let raw = &mut p.vfs as *mut _;
            let restored = ffi::sqlite3_vfs_register(p.original, 1) == ffi::SQLITE_OK && ffi::sqlite3_vfs_find(ptr::null()) == p.original;
            let unregistered = ffi::sqlite3_vfs_unregister(raw) == ffi::SQLITE_OK;
            let verified = ffi::sqlite3_vfs_find(p.name.as_ptr()).is_null() && ffi::sqlite3_vfs_find(ptr::null()) == p.original;
            self.registered = false;
            if !(restored && unregistered && verified) {
                // Registry references may remain; never free potentially referenced state.
                std::mem::forget(self.proxy.take());
                return false;
            }
            true
            }
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                if !self.cleanup() {
                    unsafe extern "C" { fn write(fd: c_int, buf: *const c_void, count: usize) -> isize; }
                    let marker = b"B0_CLEANUP_FAILURE\n";
                    let _ = write(2, marker.as_ptr().cast(), marker.len());
                }
            }
        }
    }

    #[derive(PartialEq, Eq)]
    struct Entry { name: std::ffi::OsString, kind: u32, mode: u32, uid: u32, gid: u32, links: u64, size: u64, mtime: i64, mtime_ns: i64, bytes: Option<Vec<u8>> }
    fn snapshot(dir: &Path) -> std::io::Result<Vec<Entry>> {
        fn entry(path: &Path, name: std::ffi::OsString) -> std::io::Result<Entry> {
            let m = fs::symlink_metadata(path)?;
            let bytes = if m.is_file() { Some(fs::read(path)?) } else if m.file_type().is_symlink() { Some(fs::read_link(path)?.as_os_str().as_encoded_bytes().to_vec()) } else { None };
            Ok(Entry { name, kind: m.mode() & 0o170000, mode: m.mode() & 0o7777, uid: m.uid(), gid: m.gid(), links: m.nlink(), size: m.size(), mtime: m.mtime(), mtime_ns: m.mtime_nsec(), bytes })
        }
        let mut entries = vec![entry(dir, std::ffi::OsString::new())?];
        for item in fs::read_dir(dir)? { let item = item?; entries.push(entry(&item.path(), item.file_name())?); }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }
    fn setup() -> Result<(PathBuf, [PathBuf; 3]), &'static str> {
        use std::os::unix::fs::OpenOptionsExt;
        let root = std::env::var_os("B0_A01_FIXTURE_ROOT").ok_or("B0_SETUP_ROOT")?;
        let root = PathBuf::from(root);
        if !root.is_absolute() { return Err("B0_SETUP_ROOT"); }
        let metadata = fs::symlink_metadata(&root).map_err(|_| "B0_SETUP_ROOT")?;
        unsafe extern "C" { fn geteuid() -> u32; }
        if !metadata.is_dir() || metadata.uid() != unsafe { geteuid() } || metadata.mode() & 0o077 != 0 { return Err("B0_SETUP_ROOT"); }
        let dirs = [root.join("control"), root.join("ordinary"), root.join("clock")];
        for d in &dirs {
            fs::DirBuilder::new().mode(0o700).create(d).map_err(|_| "B0_SETUP_FIXTURE")?;
        }
        let paths = dirs.each_ref().map(|d| d.join("marker.db"));
        fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&paths[0]).map_err(|_| "B0_SETUP_FIXTURE")?;
        let conn = Connection::open(&paths[0]).map_err(|_| "B0_SETUP_FIXTURE")?;
        conn.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE product_meta_v1(singleton INTEGER PRIMARY KEY, schema_version INTEGER NOT NULL); INSERT INTO product_meta_v1 VALUES(1,2561);").map_err(|_| "B0_SETUP_FIXTURE")?;
        conn.close().map_err(|_| "B0_SETUP_FIXTURE")?;
        let bytes = fs::read(&paths[0]).map_err(|_| "B0_SETUP_FIXTURE")?;
        for path in &paths[1..] {
            use std::io::Write;
            let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|_| "B0_SETUP_FIXTURE")?;
            f.write_all(&bytes).map_err(|_| "B0_SETUP_FIXTURE")?;
        }
        for d in &dirs {
            let s = snapshot(d).map_err(|_| "B0_SETUP_SNAPSHOT")?;
            if s.len() != 2 || s[1].name != "marker.db" || s[1].bytes.as_deref() != Some(bytes.as_slice()) { return Err("B0_SETUP_FIXTURE"); }
        }
        Ok((root, paths))
    }
    struct Record { mismatch: bool, preserved: bool, valid_snapshot: bool, observed: Observed }
    fn record(path: &Path, counts: &Counts, clock: bool) -> Record {
        let dir = path.parent().unwrap();
        let before = snapshot(dir);
        counts.take();
        let config = ProductConfig { installation_id: "test-installation".into(), database_id: "test-database".into() };
        let result = if clock { ProductStore::open_with_clock(path, config, Arc::new(SystemProductClock)) } else { ProductStore::open(path, config) };
        let mismatch = matches!(&result, Err(ProductError::ContractMismatch));
        drop(result);
        let observed = counts.take();
        let after = snapshot(dir);
        let valid_snapshot = before.is_ok() && after.is_ok();
        let preserved = match (before, after) { (Ok(a), Ok(b)) => a == b, _ => false };
        Record { mismatch, preserved, valid_snapshot, observed }
    }
    // The parent controls the executable and harness arguments. The pipe distinguishes
    // its launch from an accidental child-marker environment variable; it is not an
    // authentication boundary against a caller deliberately forging the protocol.
    pub fn dispatch() {
        use std::io::Read;
        const OPT_IN: &str = "B0_A01_EVIDENCE";
        const CHILD: &str = "B0_A01_EVIDENCE_CHILD";
        const TEST: &str = "a01_discriminator_is_rejected_feature_off_before_mutation";
        const HANDSHAKE: &[u8] = b"B0_A01_ISOLATED_CHILD\n";
        let opt_in = std::env::var_os(OPT_IN);
        if opt_in.is_none() { return; }
        assert!(opt_in.as_deref() == Some(std::ffi::OsStr::new("1")), "B0_ISOLATION_FAILURE");
        assert!(std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")), "B0_ISOLATION_FAILURE");
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        assert!(args == [TEST, "--exact", "--test-threads=1", "--show-output"].map(std::ffi::OsString::from), "B0_ISOLATION_FAILURE");
        let mut handshake = Vec::new();
        std::io::stdin().take(64).read_to_end(&mut handshake).expect("B0_ISOLATION_FAILURE");
        assert!(handshake == HANDSHAKE, "B0_ISOLATION_FAILURE");
        // Linux libtest has its harness thread and this single test thread. Refuse
        // unavailable process information or any additional live thread.
        let threads = std::fs::read_dir("/proc/self/task").expect("B0_ISOLATION_FAILURE");
        let mut count = 0;
        for thread in threads { thread.expect("B0_ISOLATION_FAILURE"); count += 1; }
        assert!(count == 2, "B0_ISOLATION_FAILURE");
        run();
    }
    fn run() {
        let (_root, paths) = setup().unwrap_or_else(|e| panic!("{}", e));
        let mut guard = unsafe { Guard::install() }.unwrap_or_else(|e| panic!("{}", e));
        guard.counts().take();
        let control_ok = match Connection::open(&paths[0]) { Ok(c) => c.close().is_ok(), Err(_) => false };
        let control = guard.counts().take();
        let ordinary = record(&paths[1], guard.counts(), false);
        let clock = record(&paths[2], guard.counts(), true);
        let cleanup = unsafe { guard.cleanup() };
        assert!(cleanup, "B0_CLEANUP_FAILURE");
        use std::io::Write;
        let mut evidence = std::io::stderr().lock();
        writeln!(evidence, "B0_CLEANUP_OK").expect("B0_EVIDENCE_FAILURE");
        assert!(control_ok && control.wr > 0 && control.ws > 0, "B0_CONTROL_FAILURE");
        writeln!(evidence, "B0_CONTROL_OK").expect("B0_EVIDENCE_FAILURE");
        assert!(ordinary.valid_snapshot && clock.valid_snapshot, "B0_SNAPSHOT_FAILURE");
        assert!(ordinary.mismatch && clock.mismatch, "B0_RESULT_FAILURE");
        assert!(ordinary.preserved && clock.preserved, "B0_PRESERVATION_FAILURE");
        // Only boolean observations are emitted; no paths, contents, or identifiers.
        for (label, r) in [("ORDINARY", &ordinary), ("CLOCK", &clock)] {
            writeln!(evidence, "B0_{}_OK wr={} ws={} rr={} rs={} aux={} del={}", label, r.observed.wr > 0, r.observed.ws > 0, r.observed.rr > 0, r.observed.rs > 0, r.observed.aux > 0, r.observed.del > 0).expect("B0_EVIDENCE_FAILURE");
        }
        let a = ordinary.observed; let b = clock.observed;
        let all_zero = a.wr == 0 && a.ws == 0 && b.wr == 0 && b.ws == 0;
        let accepted_red = a.wr > 0 && a.ws > 0 && b.wr > 0 && b.ws > 0;
        assert!(all_zero || accepted_red, "B0_MIXED_OBSERVATION_SHAPE");
        assert!(all_zero, "B0_A01_RED__BOTH_OPENERS_REQUESTED_AND_SUCCEEDED_WRITABLE_MAIN");
    }
}


// This probe uses the single library artifact alongside this exact test binary.
// Separate feature target directories are required; ambiguous artifacts fail.
#[test]
fn a01_feature_api_is_absent_by_default() {
    fn require<T, E>(result: Result<T, E>) -> T {
        match result { Ok(value) => value, Err(_) => panic!("A01 API probe operation failed") }
    }
    struct ProbeDir(std::path::PathBuf);
    impl Drop for ProbeDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let executable = require(std::env::current_exe());
    let deps = match executable.parent() { Some(parent) => parent, None => panic!("A01 API probe artifact directory missing") };
    let mut artifacts = Vec::new();
    for entry in require(std::fs::read_dir(deps)) {
        let entry = require(entry);
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("libfoundry_ledger-") && name.ends_with(".rlib") { artifacts.push(entry.path()); }
    }
    assert!(artifacts.len() == 1, "A01 API probe needs one exact library artifact");
    let stamp = require(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos();
    let path = std::env::temp_dir().join(format!("foundry-a01-api-probe-{}-{stamp}", std::process::id()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        require(std::fs::DirBuilder::new().mode(0o700).create(&path));
    }
    #[cfg(not(unix))]
    require(std::fs::create_dir(&path));
    let dir = ProbeDir(path);
    let source = dir.0.join("probe.rs");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    for method in ["open", "create_a01_isolated_test_store", "reopen_a01_isolated_test_store",
        "create_a01_task_claim_test_store", "reopen_a01_task_claim_test_store", "authorize_a01_task",
        "revoke_a01_task", "read_a01_task_receipt", "read_a01_task_claim"] {
        require(std::fs::write(&source, format!("use foundry_ledger::ProductStore; pub fn probe() {{ let _ = ProductStore::{method}; }}")));
        let output = require(std::process::Command::new(&rustc)
            .arg("--edition=2024").arg("--crate-name=a01_api_probe").arg("--crate-type=lib")
            .arg("--emit=metadata").arg("--error-format=json")
            .arg("--extern").arg(format!("foundry_ledger={}", artifacts[0].display()))
            .arg("-L").arg(format!("dependency={}", deps.display()))
            .arg("--out-dir").arg(&dir.0).arg(&source).output());
        if method == "open" {
            assert!(output.status.success(), "A01 API probe compiling control failed");
        } else {
            assert!(!output.status.success(), "A01 feature API exposed by default");
            let stderr = require(std::str::from_utf8(&output.stderr));
            let diagnostics: Vec<serde_json::Value> = stderr.lines().map(|line| require(serde_json::from_str(line))).collect();
            let errors: Vec<_> = diagnostics.iter().filter(|d| d["level"] == "error" && !d["code"].is_null()).collect();
            assert!(errors.len() == 1 && errors[0]["code"]["code"] == "E0599", "A01 API probe failed for an unrelated reason");
            let message = errors[0]["message"].as_str().unwrap_or("");
            assert!(message.contains(method) && message.contains("ProductStore"), "A01 API probe missing-method diagnostic mismatch");
            assert!(diagnostics.iter().filter(|d| d["level"] == "error" && d["code"].is_null())
                .all(|d| d["message"].as_str().unwrap_or("").starts_with("aborting due to")), "A01 API probe had an unrelated uncoded error");
        }
    }
    // Import probes have a different failure shape from missing methods.
    // Preserve the control above and use exactly the already selected artifact.
    for dto in ["A01TaskStoreRef", "A01TaskAdmissionExpectation", "A01TaskOwnerExpectation",
        "A01TaskServiceExpectation", "A01AuthorizeTaskRequest", "A01RevokeTaskRequest",
        "A01TaskOperation", "A01TaskReceipt", "A01TaskClaimView", "A01TaskStoreIdentity"] {
        require(std::fs::write(&source,format!("use foundry_ledger::product::{dto}; pub fn probe(_: Option<{dto}>) {{}}")));
        let output=require(std::process::Command::new(&rustc)
            .arg("--edition=2024").arg("--crate-name=a01_api_probe").arg("--crate-type=lib")
            .arg("--emit=metadata").arg("--error-format=json")
            .arg("--extern").arg(format!("foundry_ledger={}",artifacts[0].display()))
            .arg("-L").arg(format!("dependency={}",deps.display()))
            .arg("--out-dir").arg(&dir.0).arg(&source).output());
        assert!(!output.status.success(),"A01 DTO exposed by default");
        let stderr=require(std::str::from_utf8(&output.stderr));
        let diagnostics:Vec<serde_json::Value>=stderr.lines().map(|line|require(serde_json::from_str(line))).collect();
        let errors:Vec<_>=diagnostics.iter().filter(|d|d["level"]=="error" && !d["code"].is_null()).collect();
        assert!(errors.len()==1 && errors[0]["code"]["code"]=="E0432","A01 DTO absence probe failed for an unrelated reason");
        let message=errors[0]["message"].as_str().unwrap_or("");
        assert!(message.contains(dto) && message.contains("foundry_ledger::product"),"A01 DTO missing-import diagnostic mismatch");
        assert!(diagnostics.iter().filter(|d|d["level"]=="error" && d["code"].is_null())
            .all(|d|d["message"].as_str().unwrap_or("").starts_with("aborting due to")),"A01 DTO probe had an unrelated uncoded error");
    }

}
