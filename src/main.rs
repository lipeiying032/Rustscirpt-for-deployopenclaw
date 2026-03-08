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
const NETWORK_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_RETRY_INTERVAL: Duration = Duration::from_secs(5);

// ── Config ────────────────────────────────────────────────────────────────────

/// Sync config — optional: if HF_TOKEN or OPENCLAW_DATASET_REPO are absent,
/// syncing is simply disabled and OpenClaw still starts normally.
#[derive(Debug, Clone)]
struct SyncConfig {
    token: String,
    dataset_id: String,
    sync_interval: Duration,
    workspace: PathBuf,
}

impl SyncConfig {
    /// Returns None (with a warning) instead of crashing when secrets are missing.
    fn load() -> Option<Self> {
        let token = match env::var("HF_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                eprintln!("[sync] HF_TOKEN not set — HF dataset sync disabled");
                return None;
            }
        };

        let dataset_id = env::var("OPENCLAW_DATASET_REPO")
            .or_else(|_| env::var("HF_DATASET_ID"))
            .unwrap_or_default();

        if dataset_id.is_empty() || dataset_id.split('/').count() != 2 {
            eprintln!("[sync] OPENCLAW_DATASET_REPO not set or invalid — HF dataset sync disabled");
            return None;
        }

        let sync_interval_secs: u64 = env::var("SYNC_INTERVAL")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .unwrap_or(60)
            .max(1);

        Some(SyncConfig {
            token,
            dataset_id,
            sync_interval: Duration::from_secs(sync_interval_secs),
            workspace: PathBuf::from(WORKSPACE_DIR),
        })
    }
}

// ── HF API types ──────────────────────────────────────────────────────────────

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
    AddOrUpdate { path: String, encoding: String, content: String },
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

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("entrypoint fatal error: {err:#}");
        exit(1);
    }
}

async fn run() -> Result<()> {
    // Always create the workspace dir — OpenClaw may need it regardless of sync.
    let workspace = PathBuf::from(WORKSPACE_DIR);
    tokio::fs::create_dir_all(&workspace)
        .await
        .context("failed to create workspace directory")?;

    // Load sync config — None means sync is disabled, container still runs.
    let sync = SyncConfig::load();

    if let Some(cfg) = &sync {
        let client = build_client(cfg)?;

        eprintln!("[sync] waiting for network…");
        if wait_for_network(&client).await {
            eprintln!("[sync] network up — running startup sync");
            if let Err(e) = startup_sync(&client, cfg).await {
                eprintln!("[sync] startup sync failed (continuing): {e:#}");
            }
        } else {
            eprintln!("[sync] network unavailable after {:?} — skipping startup sync", NETWORK_STARTUP_TIMEOUT);
        }

        rebuild_sync_state(&cfg.workspace).await?;
    }

    // Spawn the child process (start.sh → LiteLLM + OpenClaw).
    let mut child = spawn_child_from_args()?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    if let Some(cfg) = &sync {
        // Sync loop — only active when sync is configured.
        let client = build_client(cfg)?;
        let mut ticker = interval(cfg.sync_interval);
        ticker.reset(); // skip the immediate first tick

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = push_workspace(&client, cfg).await {
                        eprintln!("[sync] periodic push failed: {e:#}");
                    }
                }
                _ = sigterm.recv() => {
                    eprintln!("[sync] SIGTERM — final push then shutdown");
                    forward_sigterm(&mut child);
                    if let Err(e) = push_workspace(&client, cfg).await {
                        eprintln!("[sync] final push failed: {e:#}");
                    }
                    wait_for_child_shutdown(&mut child).await;
                    break;
                }
                status = child.wait() => {
                    handle_child_exit(status, &client, cfg).await;
                }
            }
        }
    } else {
        // No sync — just supervise the child.
        loop {
            tokio::select! {
                _ = sigterm.recv() => {
                    forward_sigterm(&mut child);
                    wait_for_child_shutdown(&mut child).await;
                    break;
                }
                status = child.wait() => {
                    match status {
                        Ok(s) => exit(s.code().unwrap_or(1)),
                        Err(e) => { eprintln!("child wait error: {e:#}"); break; }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_child_exit(
    status: std::io::Result<std::process::ExitStatus>,
    client: &reqwest::Client,
    cfg: &SyncConfig,
) -> ! {
    match status {
        Ok(s) => {
            eprintln!("[sync] child exited: {s}");
            if let Err(e) = push_workspace(client, cfg).await {
                eprintln!("[sync] final push after child exit failed: {e:#}");
            }
            exit(s.code().unwrap_or(1));
        }
        Err(e) => {
            eprintln!("child wait error: {e:#}");
            exit(1);
        }
    }
}

// ── Network probe ─────────────────────────────────────────────────────────────

async fn wait_for_network(client: &reqwest::Client) -> bool {
    let url = "https://huggingface.co/api/whoami-v2";
    let deadline = tokio::time::Instant::now() + NETWORK_STARTUP_TIMEOUT;
    loop {
        let res = timeout(Duration::from_secs(8), client.get(url).send()).await;
        match res {
            Ok(Ok(_)) => return true,   // any HTTP response = DNS is working
            Ok(Err(e)) => eprintln!("[sync] probe error: {e}"),
            Err(_)     => eprintln!("[sync] probe timeout"),
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        eprintln!("[sync] retrying in {:?}…", NETWORK_RETRY_INTERVAL);
        sleep(NETWORK_RETRY_INTERVAL).await;
    }
}

// ── Startup sync ──────────────────────────────────────────────────────────────

async fn startup_sync(client: &reqwest::Client, cfg: &SyncConfig) -> Result<()> {
    ensure_dataset_exists(client, cfg).await?;
    pull_workspace(client, cfg).await
}

// ── HF API ────────────────────────────────────────────────────────────────────

fn build_client(cfg: &SyncConfig) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", cfg.token).parse().context("invalid HF_TOKEN")?,
    );
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to build HTTP client")
}

async fn ensure_dataset_exists(client: &reqwest::Client, cfg: &SyncConfig) -> Result<()> {
    let url = format!("https://huggingface.co/api/datasets/{}", cfg.dataset_id);
    let resp = client.get(&url).send().await?;
    match resp.status() {
        StatusCode::OK => {
            let repo: RepoLookup = resp.json().await.unwrap_or(RepoLookup { id: None });
            eprintln!("[sync] dataset ok: {}", repo.id.unwrap_or(cfg.dataset_id.clone()));
            Ok(())
        }
        StatusCode::NOT_FOUND => {
            let auto = env::var("AUTO_CREATE_DATASET")
                .map(|v| matches!(v.to_lowercase().as_str(), "1"|"true"|"yes"|"on"))
                .unwrap_or(false);
            if auto {
                eprintln!("[sync] dataset not found — creating…");
                create_private_dataset(client, cfg).await
            } else {
                Err(anyhow!(
                    "dataset {} not found. Create it or set AUTO_CREATE_DATASET=true",
                    cfg.dataset_id
                ))
            }
        }
        s => Err(anyhow!("dataset lookup failed ({s}): {}", resp.text().await.unwrap_or_default())),
    }
}

async fn create_private_dataset(client: &reqwest::Client, cfg: &SyncConfig) -> Result<()> {
    let (owner, name) = cfg.dataset_id.split_once('/').unwrap();
    let username = client
        .get("https://huggingface.co/api/whoami-v2")
        .send().await?.error_for_status()?
        .json::<serde_json::Value>().await?
        .get("name").and_then(|v| v.as_str()).unwrap_or("").to_owned();

    let req = CreateRepoRequest {
        name: name.to_string(),
        organization: if owner == username { None } else { Some(owner.to_string()) },
        private: true,
        repo_type: "dataset".to_string(),
    };
    let resp = client.post("https://huggingface.co/api/repos/create").json(&req).send().await?;
    if resp.status().is_success() || resp.status() == StatusCode::CONFLICT {
        Ok(())
    } else {
        Err(anyhow!("create dataset failed ({}): {}", resp.status(), resp.text().await.unwrap_or_default()))
    }
}

async fn pull_workspace(client: &reqwest::Client, cfg: &SyncConfig) -> Result<()> {
    eprintln!("[sync] pulling from {}", cfg.dataset_id);
    let files = list_remote_files(client, cfg).await?;
    for file in &files {
        let url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", cfg.dataset_id, file);
        let bytes = client.get(url).send().await?.error_for_status()?.bytes().await?;
        let target = cfg.workspace.join(file);
        if let Some(p) = target.parent() { tokio::fs::create_dir_all(p).await?; }
        let mut f = tokio::fs::File::create(&target).await?;
        f.write_all(&bytes).await?;
    }
    eprintln!("[sync] pull complete: {} files", files.len());
    Ok(())
}

async fn list_remote_files(client: &reqwest::Client, cfg: &SyncConfig) -> Result<Vec<String>> {
    let url = format!(
        "https://huggingface.co/api/datasets/{}/tree/main?recursive=true",
        cfg.dataset_id
    );
    let resp = client.get(url).send().await?;
    if resp.status() == StatusCode::NOT_FOUND { return Ok(vec![]); }
    let entries: Vec<TreeEntry> = resp.error_for_status()?.json().await?;
    Ok(entries.into_iter().filter(|e| e.kind == "file").map(|e| e.path).collect())
}

async fn push_workspace(client: &reqwest::Client, cfg: &SyncConfig) -> Result<()> {
    let state_path = cfg.workspace.join(STATE_FILE);
    let mut state = load_state(&state_path).await?;
    let mut current = HashMap::<String, FileState>::new();
    let mut operations = Vec::<CommitOperation>::new();

    for entry in WalkDir::new(&cfg.workspace).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let full = entry.path();
        if full == state_path { continue; }
        let rel = full.strip_prefix(&cfg.workspace)?.to_string_lossy().replace('\\', "/");
        let bytes = tokio::fs::read(full).await?;
        let md5 = format!("{:x}", md5::compute(&bytes));
        let size = bytes.len() as u64;
        let changed = state.files.get(&rel).map(|s| s.md5 != md5 || s.size != size).unwrap_or(true);
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

    if operations.is_empty() { eprintln!("[sync] no changes"); return Ok(()); }

    let resp = client
        .post(format!("https://huggingface.co/api/datasets/{}/commit/main", cfg.dataset_id))
        .json(&CommitRequest { commit: format!("sync {} files", operations.len()), operations })
        .send().await?;

    if !resp.status().is_success() {
        return Err(anyhow!("push failed ({}): {}", resp.status(), resp.text().await.unwrap_or_default()));
    }
    state.files = current;
    save_state(&state_path, &state).await?;
    eprintln!("[sync] push complete");
    Ok(())
}

async fn rebuild_sync_state(workspace: &Path) -> Result<()> {
    let state_path = workspace.join(STATE_FILE);
    let mut files = HashMap::new();
    for entry in WalkDir::new(workspace).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let full = entry.path();
        if full == state_path { continue; }
        let rel = full.strip_prefix(workspace)?.to_string_lossy().replace('\\', "/");
        let bytes = tokio::fs::read(full).await?;
        files.insert(rel, FileState { md5: format!("{:x}", md5::compute(&bytes)), size: bytes.len() as u64 });
    }
    save_state(&state_path, &SyncState { files }).await
}

async fn load_state(path: &Path) -> Result<SyncState> {
    if !path.exists() { return Ok(SyncState::default()); }
    Ok(serde_json::from_slice(&tokio::fs::read(path).await?).context("bad sync state")?)
}

async fn save_state(path: &Path, state: &SyncState) -> Result<()> {
    tokio::fs::write(path, serde_json::to_vec_pretty(state)?).await?;
    Ok(())
}

// ── Child process ─────────────────────────────────────────────────────────────

fn spawn_child_from_args() -> Result<Child> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() { return Err(anyhow!("no child command provided")); }
    Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn child")
}

fn forward_sigterm(child: &mut Child) {
    if let Some(id) = child.id() {
        // SAFETY: valid pid from live child
        if unsafe { libc::kill(id as libc::pid_t, libc::SIGTERM) } != 0 {
            eprintln!("SIGTERM forward failed: {}", std::io::Error::last_os_error());
        }
    }
}

async fn wait_for_child_shutdown(child: &mut Child) {
    match timeout(FINAL_WAIT_TIMEOUT, child.wait()).await {
        Ok(Ok(s)) => eprintln!("child exited after SIGTERM: {s}"),
        Ok(Err(e)) => eprintln!("wait error: {e:#}"),
        Err(_) => {
            eprintln!("child timeout — SIGKILL");
            if let Some(id) = child.id() {
                // SAFETY: valid pid from live child
                unsafe { libc::kill(id as libc::pid_t, libc::SIGKILL) };
            }
        }
    }
}