use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{exit, Stdio};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::time::{interval, sleep, timeout};
use walkdir::WalkDir;

const WORKSPACE_DIR: &str = "/home/user/.openclaw";
const STATE_FILE: &str = ".hf-sync-state.json";
const FINAL_WAIT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for HF to become reachable at startup before giving up
/// and continuing with an empty / local workspace.
const NETWORK_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct Config {
    token: String,
    dataset_id: String,
    sync_interval: Duration,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RepoLookup {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Serialize)]
struct CreateRepoRequest {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization: Option<String>,
    private: bool,
    #[serde(rename = "type")]
    repo_type: String,
}

#[derive(Debug, Serialize)]
struct CommitRequest {
    commit: String,
    operations: Vec<CommitOperation>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "op")]
enum CommitOperation {
    #[serde(rename = "addOrUpdate")]
    AddOrUpdate {
        path: String,
        encoding: String,
        content: String,
    },
    #[serde(rename = "delete")]
    Delete { path: String },
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SyncState {
    files: HashMap<String, FileState>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileState {
    md5: String,
    size: u64,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("entrypoint fatal error: {err:#}");
        exit(1);
    }
}

async fn run() -> Result<()> {
    let cfg = load_config()?;
    tokio::fs::create_dir_all(&cfg.workspace)
        .await
        .context("failed to create workspace directory")?;

    let client = build_client(&cfg)?;

    // ── Startup sync: wait for network, then pull. Non-fatal on failure. ────────
    // HF Space containers sometimes take 10-30s for DNS/routing to become
    // available after the container starts. We wait up to 60s, then proceed.
    eprintln!("waiting for network connectivity to huggingface.co…");
    match wait_for_network(&client).await {
        true => {
            eprintln!("network is up — proceeding with startup sync");
            if let Err(err) = startup_sync(&client, &cfg).await {
                eprintln!("startup sync failed (continuing with local workspace): {err:#}");
            }
        }
        false => {
            eprintln!(
                "huggingface.co unreachable after {:?} — starting with local workspace",
                NETWORK_STARTUP_TIMEOUT
            );
        }
    }

    rebuild_sync_state(&cfg.workspace).await?;

    // ── Spawn child process ──────────────────────────────────────────────────────
    let mut child = spawn_child_from_args()?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut ticker = interval(cfg.sync_interval);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(err) = push_workspace(&client, &cfg).await {
                    eprintln!("periodic push failed: {err:#}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("received SIGTERM — forwarding and running final sync");
                forward_sigterm(&mut child);
                if let Err(err) = push_workspace(&client, &cfg).await {
                    eprintln!("final push failed: {err:#}");
                }
                wait_for_child_shutdown(&mut child).await;
                break;
            }
            status = child.wait() => {
                match status {
                    Ok(s) => {
                        eprintln!("child exited: {s}");
                        if let Err(err) = push_workspace(&client, &cfg).await {
                            eprintln!("final push after child exit failed: {err:#}");
                        }
                        exit(s.code().unwrap_or(1));
                    }
                    Err(err) => {
                        eprintln!("error waiting for child: {err:#}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Poll HF until reachable or timeout. Returns true if network came up.
async fn wait_for_network(client: &reqwest::Client) -> bool {
    let probe_url = "https://huggingface.co/api/whoami-v2";
    let deadline = tokio::time::Instant::now() + NETWORK_STARTUP_TIMEOUT;

    loop {
        // Use a short per-attempt timeout so we fail fast and retry
        let attempt = timeout(
            Duration::from_secs(8),
            client.get(probe_url).send(),
        )
        .await;

        match attempt {
            // Any HTTP response (even 401) means DNS + routing is working
            Ok(Ok(_)) => return true,
            Ok(Err(err)) => eprintln!("network probe error: {err}"),
            Err(_) => eprintln!("network probe timed out"),
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        eprintln!("retrying in {:?}…", NETWORK_RETRY_INTERVAL);
        sleep(NETWORK_RETRY_INTERVAL).await;
    }
}

/// Pull workspace from HF dataset on first boot.
async fn startup_sync(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    ensure_dataset_exists(client, cfg).await?;
    pull_workspace(client, cfg).await
}

fn load_config() -> Result<Config> {
    let token = env::var("HF_TOKEN").context("HF_TOKEN is required")?;

    let dataset_id = env::var("OPENCLAW_DATASET_REPO")
        .or_else(|_| env::var("HF_DATASET_ID"))
        .context("OPENCLAW_DATASET_REPO (or HF_DATASET_ID) is required")?;

    validate_dataset_id(&dataset_id)?;

    let sync_interval_secs: u64 = env::var("SYNC_INTERVAL")
        .unwrap_or_else(|_| "60".to_string())
        .parse()
        .context("SYNC_INTERVAL must be an integer")?;

    if sync_interval_secs == 0 {
        return Err(anyhow!("SYNC_INTERVAL must be > 0"));
    }

    Ok(Config {
        token,
        dataset_id,
        sync_interval: Duration::from_secs(sync_interval_secs),
        workspace: PathBuf::from(WORKSPACE_DIR),
    })
}

fn validate_dataset_id(id: &str) -> Result<()> {
    let mut parts = id.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(anyhow!("OPENCLAW_DATASET_REPO must be owner/name"));
    }
    Ok(())
}

fn build_client(cfg: &Config) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", cfg.token)
            .parse()
            .context("invalid HF_TOKEN")?,
    );
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());

    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

async fn ensure_dataset_exists(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let url = format!("https://huggingface.co/api/datasets/{}", cfg.dataset_id);
    let resp = client.get(&url).send().await?;

    match resp.status() {
        StatusCode::OK => {
            let repo: RepoLookup = resp.json().await.unwrap_or(RepoLookup { id: None });
            eprintln!("dataset ok: {}", repo.id.unwrap_or(cfg.dataset_id.clone()));
            Ok(())
        }
        StatusCode::NOT_FOUND => {
            let auto = env::var("AUTO_CREATE_DATASET")
                .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false);
            if auto {
                eprintln!("dataset not found — AUTO_CREATE_DATASET=true, creating…");
                create_private_dataset(client, cfg).await
            } else {
                Err(anyhow!(
                    "dataset {} not found. Create it at huggingface.co/new-dataset \
                     or set AUTO_CREATE_DATASET=true.",
                    cfg.dataset_id
                ))
            }
        }
        s => Err(anyhow!(
            "dataset lookup failed ({s}): {}",
            resp.text().await.unwrap_or_default()
        )),
    }
}

async fn create_private_dataset(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let (owner, name) = cfg.dataset_id.split_once('/').unwrap();

    let username = client
        .get("https://huggingface.co/api/whoami-v2")
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let req = CreateRepoRequest {
        name: name.to_string(),
        organization: if owner == username { None } else { Some(owner.to_string()) },
        private: true,
        repo_type: "dataset".to_string(),
    };

    let resp = client
        .post("https://huggingface.co/api/repos/create")
        .json(&req)
        .send()
        .await?;

    if resp.status().is_success() || resp.status() == StatusCode::CONFLICT {
        Ok(())
    } else {
        Err(anyhow!(
            "create dataset failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ))
    }
}

async fn pull_workspace(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    eprintln!("pulling workspace from {}", cfg.dataset_id);
    let files = list_remote_files(client, cfg).await?;

    for file in &files {
        let url = format!(
            "https://huggingface.co/datasets/{}/resolve/main/{}",
            cfg.dataset_id, file
        );
        let bytes = client.get(url).send().await?.error_for_status()?.bytes().await?;
        let target = cfg.workspace.join(file);
        if let Some(p) = target.parent() {
            tokio::fs::create_dir_all(p).await?;
        }
        let mut f = tokio::fs::File::create(&target).await?;
        f.write_all(&bytes).await?;
    }

    eprintln!("pull complete: {} files", files.len());
    Ok(())
}

async fn list_remote_files(client: &reqwest::Client, cfg: &Config) -> Result<Vec<String>> {
    let url = format!(
        "https://huggingface.co/api/datasets/{}/tree/main?recursive=true",
        cfg.dataset_id
    );
    let resp = client.get(url).send().await?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    let entries: Vec<TreeEntry> = resp.error_for_status()?.json().await?;
    Ok(entries.into_iter().filter(|e| e.kind == "file").map(|e| e.path).collect())
}

async fn push_workspace(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let state_path = cfg.workspace.join(STATE_FILE);
    let mut state = load_state(&state_path).await?;

    let mut current = HashMap::<String, FileState>::new();
    let mut operations = Vec::<CommitOperation>::new();

    for entry in WalkDir::new(&cfg.workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let full = entry.path();
        if full == state_path {
            continue;
        }
        let rel = full
            .strip_prefix(&cfg.workspace)?
            .to_string_lossy()
            .replace('\\', "/");

        let bytes = tokio::fs::read(full).await?;
        let md5 = format!("{:x}", md5::compute(&bytes));
        let size = bytes.len() as u64;

        let changed = state
            .files
            .get(&rel)
            .map(|s| s.md5 != md5 || s.size != size)
            .unwrap_or(true);

        if changed {
            operations.push(CommitOperation::AddOrUpdate {
                path: rel.clone(),
                encoding: "base64".to_string(),
                content: base64::engine::general_purpose::STANDARD.encode(&bytes),
            });
        }
        current.insert(rel, FileState { md5, size });
    }

    let old: HashSet<_> = state.files.keys().cloned().collect();
    let new: HashSet<_> = current.keys().cloned().collect();
    for removed in old.difference(&new) {
        operations.push(CommitOperation::Delete { path: removed.clone() });
    }

    if operations.is_empty() {
        eprintln!("no workspace changes");
        return Ok(());
    }

    let req = CommitRequest {
        commit: format!("sync {} files", operations.len()),
        operations,
    };

    let url = format!(
        "https://huggingface.co/api/datasets/{}/commit/main",
        cfg.dataset_id
    );
    let resp = client.post(url).json(&req).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "push failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    state.files = current;
    save_state(&state_path, &state).await?;
    eprintln!("push complete");
    Ok(())
}

async fn rebuild_sync_state(workspace: &Path) -> Result<()> {
    let state_path = workspace.join(STATE_FILE);
    let mut files = HashMap::new();

    for entry in WalkDir::new(workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let full = entry.path();
        if full == state_path {
            continue;
        }
        let rel = full
            .strip_prefix(workspace)?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = tokio::fs::read(full).await?;
        files.insert(rel, FileState {
            md5: format!("{:x}", md5::compute(&bytes)),
            size: bytes.len() as u64,
        });
    }

    save_state(&state_path, &SyncState { files }).await
}

async fn load_state(path: &Path) -> Result<SyncState> {
    if !path.exists() {
        return Ok(SyncState::default());
    }
    let raw = tokio::fs::read(path).await?;
    Ok(serde_json::from_slice(&raw).context("bad sync state")?)
}

async fn save_state(path: &Path, state: &SyncState) -> Result<()> {
    tokio::fs::write(path, serde_json::to_vec_pretty(state)?).await?;
    Ok(())
}

fn spawn_child_from_args() -> Result<Child> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Err(anyhow!("no child command provided"));
    }
    Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn child process")
}

fn forward_sigterm(child: &mut Child) {
    if let Some(id) = child.id() {
        // SAFETY: valid pid from live child
        let ret = unsafe { libc::kill(id as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            eprintln!("SIGTERM forward failed: {}", std::io::Error::last_os_error());
        }
    }
}

async fn wait_for_child_shutdown(child: &mut Child) {
    match timeout(FINAL_WAIT_TIMEOUT, child.wait()).await {
        Ok(Ok(s)) => eprintln!("child exited after SIGTERM: {s}"),
        Ok(Err(e)) => eprintln!("wait error: {e:#}"),
        Err(_) => {
            eprintln!("child timeout — sending SIGKILL");
            if let Some(id) = child.id() {
                // SAFETY: valid pid from live child
                unsafe { libc::kill(id as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
}