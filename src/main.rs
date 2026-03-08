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
use tokio::time::{interval, timeout};
use walkdir::WalkDir;

/// The OpenClaw config directory synced to/from HuggingFace.
const WORKSPACE_DIR: &str = "/home/user/.openclaw";

const STATE_FILE: &str = ".hf-sync-state.json";
const FINAL_WAIT_TIMEOUT: Duration = Duration::from_secs(20);

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

    ensure_dataset_exists(&client, &cfg).await?;
    pull_workspace(&client, &cfg).await?;
    rebuild_sync_state(&cfg.workspace).await?;

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
                        eprintln!("child exited with status: {s}");
                        if let Err(err) = push_workspace(&client, &cfg).await {
                            eprintln!("final push after child exit failed: {err:#}");
                        }
                        exit(s.code().unwrap_or(1));
                    }
                    Err(err) => {
                        eprintln!("failed waiting for child: {err:#}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

fn load_config() -> Result<Config> {
    let token = env::var("HF_TOKEN").context("HF_TOKEN is required")?;

    // Accept OPENCLAW_DATASET_REPO (canonical) with HF_DATASET_ID as fallback.
    let dataset_id = env::var("OPENCLAW_DATASET_REPO")
        .or_else(|_| env::var("HF_DATASET_ID"))
        .context("OPENCLAW_DATASET_REPO (or HF_DATASET_ID) is required")?;

    validate_dataset_id(&dataset_id)?;

    let sync_interval_secs: u64 = env::var("SYNC_INTERVAL")
        .unwrap_or_else(|_| "60".to_string())
        .parse()
        .context("SYNC_INTERVAL must be an integer in seconds")?;

    if sync_interval_secs == 0 {
        return Err(anyhow!("SYNC_INTERVAL must be greater than 0"));
    }

    Ok(Config {
        token,
        dataset_id,
        sync_interval: Duration::from_secs(sync_interval_secs),
        workspace: PathBuf::from(WORKSPACE_DIR),
    })
}

fn validate_dataset_id(dataset_id: &str) -> Result<()> {
    let mut parts = dataset_id.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(anyhow!(
            "OPENCLAW_DATASET_REPO must be in the form owner/name"
        ));
    }
    Ok(())
}

fn build_client(cfg: &Config) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        format!("Bearer {}", cfg.token)
            .parse()
            .context("invalid HF_TOKEN for auth header")?,
    );
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());

    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .context("failed to create HTTP client")
}

async fn ensure_dataset_exists(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let url = format!("https://huggingface.co/api/datasets/{}", cfg.dataset_id);
    let response = client.get(&url).send().await?;

    match response.status() {
        StatusCode::OK => {
            let repo: RepoLookup = response.json().await.unwrap_or(RepoLookup { id: None });
            eprintln!("dataset exists: {}", repo.id.unwrap_or(cfg.dataset_id.clone()));
            Ok(())
        }
        StatusCode::NOT_FOUND => {
            let auto_create = env::var("AUTO_CREATE_DATASET")
                .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false);

            if auto_create {
                eprintln!("dataset {} not found — AUTO_CREATE_DATASET=true, creating…", cfg.dataset_id);
                create_private_dataset(client, cfg).await
            } else {
                Err(anyhow!(
                    "dataset {} not found. Create it at huggingface.co/new-dataset \
                     or set AUTO_CREATE_DATASET=true.",
                    cfg.dataset_id
                ))
            }
        }
        status => {
            let body = response.text().await.unwrap_or_default();
            Err(anyhow!("dataset lookup failed ({status}): {body}"))
        }
    }
}

async fn create_private_dataset(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    let (owner, name) = cfg
        .dataset_id
        .split_once('/')
        .ok_or_else(|| anyhow!("OPENCLAW_DATASET_REPO must be in the form owner/name"))?;

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

    let response = client
        .post("https://huggingface.co/api/repos/create")
        .json(&req)
        .send()
        .await?;

    if response.status().is_success() || response.status() == StatusCode::CONFLICT {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(anyhow!("failed to create dataset ({status}): {body}"))
    }
}

async fn pull_workspace(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    eprintln!("pulling workspace from HuggingFace: {}", cfg.dataset_id);
    let remote_files = list_remote_files(client, cfg).await?;

    for file in &remote_files {
        let url = format!(
            "https://huggingface.co/datasets/{}/resolve/main/{}",
            cfg.dataset_id, file
        );
        let bytes = client.get(url).send().await?.error_for_status()?.bytes().await?;
        let target = cfg.workspace.join(file);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut f = tokio::fs::File::create(&target).await?;
        f.write_all(&bytes).await?;
    }

    eprintln!("pull complete: {} files restored", remote_files.len());
    Ok(())
}

async fn list_remote_files(client: &reqwest::Client, cfg: &Config) -> Result<Vec<String>> {
    let url = format!(
        "https://huggingface.co/api/datasets/{}/tree/main?recursive=true",
        cfg.dataset_id
    );
    let response = client.get(url).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let entries: Vec<TreeEntry> = response.error_for_status()?.json().await?;
    Ok(entries.into_iter().filter(|e| e.kind == "file").map(|e| e.path).collect())
}

async fn push_workspace(client: &reqwest::Client, cfg: &Config) -> Result<()> {
    eprintln!("pushing workspace to HuggingFace…");
    let state_path = cfg.workspace.join(STATE_FILE);
    let mut state = load_state(&state_path).await?;

    let mut current = HashMap::<String, FileState>::new();
    let mut operations = Vec::<CommitOperation>::new();

    for entry in WalkDir::new(&cfg.workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let full_path = entry.path();
        if full_path == state_path {
            continue;
        }

        let relative = full_path
            .strip_prefix(&cfg.workspace)
            .context("failed to strip workspace prefix")?
            .to_string_lossy()
            .replace('\\', "/");

        let bytes = tokio::fs::read(full_path).await?;
        let md5 = format!("{:x}", md5::compute(&bytes));
        let size = bytes.len() as u64;

        let changed = state
            .files
            .get(&relative)
            .map(|s| s.md5 != md5 || s.size != size)
            .unwrap_or(true);

        if changed {
            operations.push(CommitOperation::AddOrUpdate {
                path: relative.clone(),
                encoding: "base64".to_string(),
                content: base64::engine::general_purpose::STANDARD.encode(&bytes),
            });
        }

        current.insert(relative, FileState { md5, size });
    }

    let old_paths: HashSet<_> = state.files.keys().cloned().collect();
    let new_paths: HashSet<_> = current.keys().cloned().collect();
    for removed in old_paths.difference(&new_paths) {
        operations.push(CommitOperation::Delete { path: removed.clone() });
    }

    if operations.is_empty() {
        eprintln!("no workspace changes detected");
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
    let response = client.post(url).json(&req).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("push failed ({status}): {body}"));
    }

    state.files = current;
    save_state(&state_path, &state).await?;
    eprintln!("push complete");
    Ok(())
}

async fn rebuild_sync_state(workspace: &Path) -> Result<()> {
    let mut files = HashMap::<String, FileState>::new();
    let state_path = workspace.join(STATE_FILE);

    for entry in WalkDir::new(workspace)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let full_path = entry.path();
        if full_path == state_path {
            continue;
        }
        let relative = full_path
            .strip_prefix(workspace)
            .context("failed to strip workspace prefix")?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = tokio::fs::read(full_path).await?;
        files.insert(relative, FileState {
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
    Ok(serde_json::from_slice(&raw).context("failed to parse sync state file")?)
}

async fn save_state(path: &Path, state: &SyncState) -> Result<()> {
    tokio::fs::write(path, serde_json::to_vec_pretty(state)?).await?;
    Ok(())
}

fn spawn_child_from_args() -> Result<Child> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        return Err(anyhow!(
            "no command provided — pass the child command as entrypoint arguments"
        ));
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
        // SAFETY: id is a valid pid from a live child process.
        let ret = unsafe { libc::kill(id as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            eprintln!("failed to forward SIGTERM: {}", std::io::Error::last_os_error());
        }
    }
}

async fn wait_for_child_shutdown(child: &mut Child) {
    match timeout(FINAL_WAIT_TIMEOUT, child.wait()).await {
        Ok(Ok(s)) => eprintln!("child exited after SIGTERM: {s}"),
        Ok(Err(e)) => eprintln!("error waiting for child after SIGTERM: {e:#}"),
        Err(_) => {
            eprintln!("child did not exit within {:?} — sending SIGKILL", FINAL_WAIT_TIMEOUT);
            if let Some(id) = child.id() {
                // SAFETY: id is a valid pid from a live child process.
                let ret = unsafe { libc::kill(id as libc::pid_t, libc::SIGKILL) };
                if ret != 0 {
                    eprintln!("failed to SIGKILL: {}", std::io::Error::last_os_error());
                }
            }
        }
    }
}