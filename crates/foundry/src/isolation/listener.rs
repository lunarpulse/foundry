//! One stage, one short-lived socket and one controller-issued capability.
//! Frames are a big-endian u32 length followed by UTF-8 JSON.
use foundry_watch::model_proxy::{ModelProxy, ProviderEndpoint, ProxyRequest};
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX: usize = 256 * 1024;

fn rejected() -> io::Error {
    io::Error::other("proxy operation rejected")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    token: String,
    request: ProxyRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderReply {
    status: u16,
    body: String,
}

struct Grant {
    token_hash: [u8; 32],
    run: String,
    provider: String,
    scope: Vec<String>,
    remaining: u32,
    deadline: Instant,
    upstream: PathBuf,
}

fn read_frame(stream: &mut UnixStream, deadline: Instant, limit: usize) -> io::Result<Vec<u8>> {
    fn exact(s: &mut UnixStream, mut dst: &mut [u8], until: Instant) -> io::Result<()> {
        while !dst.is_empty() {
            s.set_read_timeout(Some(
                until
                    .checked_duration_since(Instant::now())
                    .ok_or_else(rejected)?,
            ))?;
            let n = s.read(dst)?;
            if n == 0 {
                return Err(rejected());
            }
            dst = &mut dst[n..];
        }
        Ok(())
    }
    let mut size = [0; 4];
    exact(stream, &mut size, deadline)?;
    let n = u32::from_be_bytes(size) as usize;
    if n > limit {
        return Err(rejected());
    }
    let mut data = vec![0; n];
    exact(stream, &mut data, deadline)?;
    Ok(data)
}

fn write_frame(stream: &mut UnixStream, v: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(v)?;
    if bytes.len() > MAX {
        return Err(rejected());
    }
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)
}

impl Grant {
    fn reserve(&mut self, wire: &[u8], policy: &ModelProxy) -> io::Result<(ProxyRequest, usize)> {
        let envelope: Envelope = serde_json::from_slice(wire)?;
        let req = envelope.request;
        let hash: [u8; 32] = Sha256::digest(envelope.token.as_bytes()).into();
        if hash != self.token_hash
            || req.run != self.run
            || req.provider != self.provider
            || req.budget_scope != self.scope
            || self.remaining == 0
            || Instant::now() >= self.deadline
        {
            return Err(rejected());
        }
        let provider = policy.validate(&req).map_err(|_| rejected())?;
        let actual = format!("unix://{}", self.upstream.display());
        policy
            .screen_egress(&req.provider, &actual)
            .map_err(|_| rejected())?;
        // Reserve the call before sending. An uncertain send cannot be retried for free.
        self.remaining -= 1;
        Ok((req, provider.max_response_bytes.min(MAX as u64) as usize))
    }

    fn forward(&mut self, wire: &[u8], policy: &ModelProxy) -> io::Result<Value> {
        let (req, limit) = self.reserve(wire, policy)?;
        let until = self.deadline.min(Instant::now() + Duration::from_secs(1));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()?;
        let mut upstream = runtime
            .block_on(async {
                tokio::time::timeout_at(
                    until.into(),
                    tokio::net::UnixStream::connect(&self.upstream),
                )
                .await
                .map_err(|_| rejected())?
            })?
            .into_std()?;
        upstream.set_nonblocking(false)?;
        let id = policy.request_id(&req);
        write_frame(&mut upstream, &json!({"request_id":id,"request":req}))?;
        let raw = read_frame(&mut upstream, until, MAX)?;
        let reply: ProviderReply = serde_json::from_slice(&raw)?;
        if !(200..300).contains(&reply.status) || reply.body.len() > limit {
            return Err(rejected());
        }
        let body = reply.body;
        policy
            .check_response_size(&req.provider, body.len() as u64)
            .map_err(|_| rejected())?;
        Ok(json!({"request_id":id,"body":body,
                  "run":req.run,"provider":req.provider,"budget_scope":req.budget_scope}))
    }
}

pub(super) struct Listener {
    pub token: String,
    pub path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Listener {
    pub fn offline(path: PathBuf, run: &str, scope: &[String], until: Instant) -> io::Result<Self> {
        Self::start(
            path,
            run,
            "none",
            scope,
            until,
            0,
            Path::new("/run/foundry/providers/none.sock"),
        )
    }

    fn start(
        path: PathBuf,
        run: &str,
        provider: &str,
        scope: &[String],
        until: Instant,
        remaining: u32,
        upstream: &Path,
    ) -> io::Result<Self> {
        let mut secret = [0; 32];
        OsRng.try_fill_bytes(&mut secret).map_err(|_| rejected())?;
        let token = hex::encode(secret);
        let mut grant = Grant {
            token_hash: Sha256::digest(token.as_bytes()).into(),
            run: run.into(),
            provider: provider.into(),
            scope: scope.into(),
            deadline: until,
            remaining,
            upstream: upstream.into(),
        };
        let listener = UnixListener::bind(&path)?;
        // Parent is controller-private. The sole child bind-mounts the socket inode.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        let policy = ModelProxy::new(
            vec![ProviderEndpoint {
                name: provider.into(),
                origin: format!("unix://{}", upstream.display()),
                env_key_name: String::new(),
                max_response_bytes: 64 * 1024,
            }],
            move |_| {
                if Instant::now() < until {
                    Ok(())
                } else {
                    Err("expired".into())
                }
            },
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) && Instant::now() < until {
                match listener.accept() {
                    Ok((mut client, _)) => {
                        let deadline = until.min(Instant::now() + Duration::from_secs(1));
                        let response = read_frame(&mut client, deadline, MAX)
                            .and_then(|wire| grant.forward(&wire, &policy))
                            .unwrap_or_else(|_| json!({"error":"denied"}));
                        let _ = write_frame(&mut client, &response);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            token,
            path,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_and_budget_precede_transport() {
        let until = Instant::now() + Duration::from_secs(30);
        let scope = vec!["order".into(), "7".into(), "journal".into()];
        let mut grant = Grant {
            token_hash: Sha256::digest(b"fixture-token").into(),
            run: "journal".into(),
            provider: "fixture".into(),
            scope: scope.clone(),
            deadline: until,
            remaining: 1,
            upstream: "/fixture.sock".into(),
        };
        let policy = ModelProxy::new(
            vec![ProviderEndpoint {
                name: "fixture".into(),
                origin: "unix:///fixture.sock".into(),
                env_key_name: String::new(),
                max_response_bytes: 512,
            }],
            |_| Ok(()),
        );
        let valid = json!({"token":"fixture-token","request":{"run":"journal","provider":"fixture","budget_scope":scope,"body":"test"}});
        for (field, value) in [
            ("/token", json!("other")),
            ("/request/run", json!("other")),
            ("/request/provider", json!("other")),
            ("/request/budget_scope", json!([])),
        ] {
            let mut bad = valid.clone();
            *bad.pointer_mut(field).unwrap() = value;
            assert!(
                grant
                    .reserve(&serde_json::to_vec(&bad).unwrap(), &policy)
                    .is_err()
            );
        }
        let mut extra = valid.clone();
        extra["request"]["url"] = json!("forbidden");
        assert!(
            grant
                .reserve(&serde_json::to_vec(&extra).unwrap(), &policy)
                .is_err()
        );
        let wire = serde_json::to_vec(&valid).unwrap();
        assert_eq!(grant.reserve(&wire, &policy).unwrap().1, 512);
        assert!(grant.reserve(&wire, &policy).is_err());
    }

    #[test]
    fn authenticated_listener_uses_bound_test_upstream() {
        let root = std::env::temp_dir().join(format!("w3-wire-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let address = root.join("up.sock");
        let upstream = UnixListener::bind(&address).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let scope = vec!["fixture-budget".to_owned()];
        let gate = Listener::start(
            root.join("model.sock"),
            "fixture-run",
            "test",
            &scope,
            until,
            1,
            &address,
        )
        .unwrap();
        let call = |token: &str, run: &str, provider: &str, scope: &[String]| {
            let mut client = UnixStream::connect(&gate.path).unwrap();
            write_frame(&mut client,&json!({"token":token,"request":{"provider":provider,"run":run,"budget_scope":scope,"body":"inert"}})).unwrap();
            serde_json::from_slice::<Value>(&read_frame(&mut client, until, MAX).unwrap()).unwrap()
        };
        for reply in [
            call("wrong", "fixture-run", "test", &scope),
            call(&gate.token, "other", "test", &scope),
            call(&gate.token, "fixture-run", "other", &scope),
            call(&gate.token, "fixture-run", "test", &[]),
        ] {
            assert_eq!(reply, json!({"error":"denied"}));
        }
        let server = thread::spawn(move || {
            upstream.set_nonblocking(true).unwrap();
            let mut conn = loop {
                if let Ok((stream, _)) = upstream.accept() {
                    break stream;
                }
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(5));
            };
            let bytes = read_frame(&mut conn, until, MAX).unwrap();
            let frame: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(frame["request"]["body"], "inert");
            write_frame(&mut conn, &json!({"status":200,"body":"test"})).unwrap();
            frame["request_id"].clone()
        });
        let response = call(&gate.token, "fixture-run", "test", &scope);
        assert_eq!(response["body"], "test");
        assert_eq!(response["request_id"], server.join().unwrap());
        assert_eq!(
            call(&gate.token, "fixture-run", "test", &scope),
            json!({"error":"denied"})
        );
        drop(gate);
        fs::remove_file(address).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
