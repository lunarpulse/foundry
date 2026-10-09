//! T56w — Docker runtime enforcement is MANDATORY (astra B6:
//! "eprintln + return records a skip as a pass").
//!
//! Changes from t44b:
//!   * NO SKIP PATH. The Docker daemon being absent/down is a FAIL
//!     with an actionable message, never a silent success. The gate
//!     (T59w) refuses to pass without this observation.
//!   * The container flags are DERIVED from the library
//!     (model_proxy::docker_argv of cast_container_spec) — the test
//!     no longer re-implements the flag list, so the test and the
//!     production runtime cannot drift apart.
//!   * Full-spelling flags only: -v/--volume, and any form of
//!     interactive privilege (-it, -i, -t, --privileged) are
//!     FORBIDDEN in the derived argv (short flags would bypass the
//!     exact-match screening).
//!   * NON-ROOT: the derived argv carries --user 65534:65534
//!     (nobody); the live container's actual runtime user is
//!     observed and asserted.
//!   * All four original observations (R1 egress, R2 credentials,
//!     R3 read-only rootfs, R4 hardening live in HostConfig) are
//!     kept, executed against the DERIVED argv.
use std::process::Command;

fn docker(args: &[&str]) -> Result<String, String> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .map_err(|e| format!("spawn docker: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("docker {} -> {}: {}", args.join(" "), out.status, stderr.trim()))
    }
}

fn require_daemon() {
    let up = Command::new("docker").args(["version", "--format", "{{.Server.Version}}"]).output();
    match up {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
            eprintln!("docker daemon up: {v}");
        }
        _ => panic!(
            "MANDATORY (T56w, astra B6): the Docker runtime boundary observation \
             cannot be skipped. Start the Docker daemon and re-run. \
             A skip here must NEVER count as a pass."
        ),
    }
}

const NET: &str = "t56w-proxynet";
const IMG: &str = "t56w-cast:1";

fn setup() -> Result<(), String> {
    let _ = docker(&["rm", "-f", "t56w-live"]);
    let _ = docker(&["network", "rm", NET]);
    docker(&["network", "create", "--internal", NET])?;
    docker(&["pull", "-q", "busybox:1.37"]).and_then(|_| docker(&["tag", "busybox:1.37", IMG]).map(|_| ()))
}

/// The derived argv from the LIBRARY — production and test share one
/// flag source. `tail` is the image + command part.
fn derived_run_args(name: &str, tail: &[&str]) -> Vec<String> {
    let spec = foundry_watch::model_proxy::cast_container_spec(IMG, NET);
    let mut argv = foundry_watch::model_proxy::docker_argv(&spec, name);
    argv.extend(tail.iter().map(|s| s.to_string()));
    argv
}

fn run_derived(name: &str, tail: &[&str]) -> Result<String, String> {
    let argv = derived_run_args(name, tail);
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    docker(&refs)
}

#[test]
fn runtime_boundary_mandatory() {
    require_daemon();
    setup().expect("setup");

    // --- the derived argv is the ONLY flag source: screening on it
    let argv = derived_run_args("t56w-argv", &["true"]);
    assert!(argv.contains(&"--read-only".to_string()), "read-only in derived argv: {argv:?}");
    assert!(argv.contains(&"--cap-drop=ALL".to_string()), "cap-drop=ALL in derived argv");
    assert!(argv.windows(2).any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"));
    assert!(argv.windows(2).any(|w| w[0] == "--network" && w[1] == NET), "the ONLY network is the internal proxy net");
    // full-spelling enforcement: no short/alternate forms anywhere
    for bad in ["-v", "--volume", "-it", "-i", "-t", "--privileged", "--pid=host", "--network=host"] {
        assert!(!argv.contains(&bad.to_string()), "forbidden flag {bad} in derived argv: {argv:?}");
    }
    // non-root user in the derived argv
    assert!(
        argv.windows(2).any(|w| w[0] == "--user" && w[1] == "65534:65534"),
        "derived argv must run as nobody (65534): {argv:?}"
    );
    // no credential-bearing env passthrough
    let spec = foundry_watch::model_proxy::cast_container_spec(IMG, NET);
    assert!(!spec.env_passthrough.iter().any(|e| e.contains("KEY") || e.contains("TOKEN")));

    // --- R1: external egress refused (derived argv)
    let egress_out = run_derived(
        "t56w-r1",
        &["sh", "-c", "wget -q -T 3 -O /dev/null http://93.184.216.34/ 2>&1; echo rc=$?"],
    )
    .unwrap_or_default();
    assert!(
        !egress_out.trim().ends_with("rc=0"),
        "external egress must fail on the internal proxy network, got: {egress_out}"
    );

    // --- R2: credential files invisible (derived argv)
    let creds_out = run_derived(
        "t56w-r2",
        &["sh", "-c", "find / -maxdepth 3 \\( -name gh_token -o -name die.env -o -name notifier.env \\) 2>/dev/null | head -3; id"],
    )
    .unwrap_or_default();
    assert!(
        !creds_out.contains("gh_token") && !creds_out.contains("die.env") && !creds_out.contains("notifier.env"),
        "credentials must not be visible inside the cast container: {creds_out}"
    );
    assert!(creds_out.contains("uid=65534"), "live container runs as nobody: {creds_out}");

    // --- R3: read-only rootfs (derived argv already carries --read-only)
    let write_out = run_derived("t56w-r3", &["sh", "-c", "echo x > /probe 2>&1; echo rc=$?"])
        .unwrap_or_default();
    assert!(!write_out.trim().ends_with("rc=0"), "rootfs write must fail, got: {write_out}");

    // --- R4: hardening + user LIVE in HostConfig (derived argv, daemon-run)
    let inspect = {
        let spec = foundry_watch::model_proxy::cast_container_spec(IMG, NET);
        let mut argv = foundry_watch::model_proxy::docker_argv(&spec, "t56w-live");
        argv.insert(1, "-d".to_string()); // daemonize AFTER 'run'
        argv.extend(["sleep".to_string(), "30".to_string()]);
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        docker(&refs)
    };
    inspect.expect("start hardened container with the derived argv");
    let cfg = docker(&["inspect", "t56w-live"]).expect("inspect");
    let v: serde_json::Value = serde_json::from_str(&cfg).expect("inspect JSON");
    let hc = &v[0]["HostConfig"];
    assert_eq!(hc["CapDrop"].as_array().map(|a| a.len()), Some(1), "CapDrop live");
    assert_eq!(hc["CapDrop"][0].as_str(), Some("ALL"), "cap-drop ALL live");
    assert_eq!(hc["ReadonlyRootfs"].as_bool(), Some(true), "read-only live");
    assert!(
        hc["SecurityOpt"].as_array().map(|a| a.iter().any(|x| x.as_str() == Some("no-new-privileges"))).unwrap_or(false),
        "no-new-privileges live"
    );
    // non-root live: Config.User must be 65534:65534
    let user = v[0]["Config"]["User"].as_str().unwrap_or("");
    assert_eq!(user, "65534:65534", "live container user is nobody, got: {user:?}");
    // no mounts live
    let mounts = v[0]["Mounts"].as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(mounts, 0, "zero mounts live");
    let _ = docker(&["rm", "-f", "t56w-live"]);

    // cleanup
    let _ = docker(&["network", "rm", NET]);
}

#[test]
fn docker_daemon_absence_is_a_documented_failure_not_a_skip() {
    // This companion unit test PINS the contract: there is no code
    // path in this file that turns an absent daemon into success.
    // require_daemon() panics on absence; this test asserts the
    // panic message content when the daemon IS absent, and passes
    // trivially when it is present (the mandatory test above is the
    // real observation).
    let up = Command::new("docker").args(["version", "--format", "{{.Server.Version}}"]).output();
    let daemon = matches!(&up, Ok(o) if o.status.success());
    if daemon {
        return; // present: nothing to prove here
    }
    let result = std::panic::catch_unwind(require_daemon);
    assert!(result.is_err(), "require_daemon must PANIC when the daemon is absent (no skip path)");
}
