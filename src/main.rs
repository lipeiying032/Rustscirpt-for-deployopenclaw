 use anyhow::{anyhow, Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::interval;
use walkdir::WalkDir;

const DEFAULT_WORKSPACE: &str = "/home/user/.openclaw/workspace";
const MANIFEST_PATH: &str = ".sync_manifest.json";

#[derive(Clone, Debug)]
struct Config {
    hf_token: String,
    hf_dataset_id: String,
    sync_interval: u64,
    start_command: String,
    workspace_dir: PathBuf,
}

#[derive(Clone)]
struct SyncManager {
    client: Client,
    config: Config,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Manifest {
    files: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct DatasetInfo {
    id: String,
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

impl Config {
    fn from_env() -> Result<Self> {
        let hf_token = env::var("HF_TOKEN").context("missing HF_TOKEN")?;
        let hf_dataset_id = env::var("HF_DATASET_ID").context("missing HF_DATASET_ID")?;
        let sync_interval = env::var("SYNC_INTERVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let start_command = env::var("START_COMMAND").context("missing START_COMMAND")?;

        Ok(Self {
            hf_token,
            hf_dataset_id,
            sync_interval,
            start_command,
            workspace_dir: PathBuf::from(DEFAULT_WORKSPACE),
        })
    }
}

impl SyncManager {
    fn new(config: Config) -> Result<Self> {
        let client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { client, config })
    }

    async fn init_dataset(&self) -> Result<()> {
        if self.dataset_exists().await? {
            println!("[hf] dataset exists: {}", self.config.hf_dataset_id);
            return Ok(());
        }

        println!("[hf] dataset not found, creating private dataset");
        self.create_dataset().await
    }

    async fn startup_pull(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.config.workspace_dir)
            .await
            .context("failed to create workspace directory")?;

        let entries = self.list_remote_files().await?;
        if entries.is_empty() {
            println!("[pull] dataset is empty, skip pull");
            return Ok(());
        }

        for path in &entries {
            let content = self.download_file(&path).await?;
            let local_path = self.config.workspace_dir.join(&path);
            if let Some(parent) = local_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&local_path, content)
                .await
                .with_context(|| format!("failed writing {}", local_path.display()))?;
        }

        println!("[pull] restored {} file(s)", entries.len());
        Ok(())
    }

    async fn push_cycle(&self) -> Result<()> {
        let mut manifest = self.load_local_manifest().await?;
        let mut new_manifest = Manifest::default();

        let files = collect_workspace_files(&self.config.workspace_dir)?;
        for file in files {
            let rel = file
                .strip_prefix(&self.config.workspace_dir)
                .context("failed to compute relative path")?
                .to_string_lossy()
                .replace('\\', "/");

            let bytes = tokio::fs::read(&file)
                .await
                .with_context(|| format!("failed to read {}", file.display()))?;
            let hash = format!("{:x}", md5::compute(&bytes));
            new_manifest.files.insert(rel.clone(), hash.clone());

            let changed = manifest.files.get(&rel).map(|h| h != &hash).unwrap_or(true);
            if changed {
                self.upload_file(&rel, bytes).await?;
                println!("[push] uploaded {rel}");
            }
        }

        manifest = new_manifest;
        self.save_local_manifest(&manifest).await?;
        self.upload_manifest(&manifest).await?;
        Ok(())
    }

    async fn dataset_exists(&self) -> Result<bool> {
        let url = format!(
            "https://huggingface.co/api/datasets/{}",
            self.config.hf_dataset_id
        );
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.config.hf_token)
            .send()
            .await
            .context("dataset existence request failed")?;

        match resp.status() {
            StatusCode::OK => {
                let info: DatasetInfo = resp.json().await.context("invalid dataset payload")?;
                println!("[hf] connected dataset: {}", info.id);
                Ok(true)
            }
            StatusCode::NOT_FOUND => Ok(false),
            s => Err(anyhow!("failed checking dataset, status={s}")),
        }
    }

    async fn create_dataset(&self) -> Result<()> {
        let (organization, name) = split_dataset_id(&self.config.hf_dataset_id)?;
        let payload = CreateRepoRequest {
            name,
            organization,
            private: true,
            repo_type: "dataset".to_string(),
        };

        let resp = self
            .client
            .post("https://huggingface.co/api/repos/create")
            .bearer_auth(&self.config.hf_token)
            .json(&payload)
            .send()
            .await
            .context("create dataset request failed")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "failed creating dataset {}, status={} body={}",
                self.config.hf_dataset_id,
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        Ok(())
    }

    async fn list_remote_files(&self) -> Result<Vec<String>> {
        let url = format!(
            "https://huggingface.co/api/datasets/{}/tree/main?recursive=true&expand=true",
            self.config.hf_dataset_id
        );

        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.config.hf_token)
            .send()
            .await
            .context("list dataset files request failed")?;

        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }

        if !resp.status().is_success() {
            return Err(anyhow!(
                "failed listing dataset files, status={}",
                resp.status()
            ));
        }

        let entries: Vec<TreeEntry> = resp.json().await.context("invalid tree payload")?;
        let files = entries
            .into_iter()
            .filter(|e| e.entry_type == "file")
            .map(|e| e.path)
            .filter(|p| p != MANIFEST_PATH)
            .collect();
        Ok(files)
    }

    async fn download_file(&self, remote_path: &str) -> Result<Vec<u8>> {
        let encoded = url_encode_path(remote_path);
        let url = format!(
            "https://huggingface.co/datasets/{}/resolve/main/{}?download=1",
            self.config.hf_dataset_id, encoded
        );

        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.config.hf_token)
            .send()
            .await
            .with_context(|| format!("download failed for {remote_path}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "failed downloading {remote_path}, status={}",
                resp.status()
            ));
        }

        Ok(resp.bytes().await?.to_vec())
    }

    async fn upload_file(&self, remote_path: &str, bytes: Vec<u8>) -> Result<()> {
        let encoded = url_encode_path(remote_path);
        let url = format!(
            "https://huggingface.co/api/datasets/{}/upload/main/{}",
            self.config.hf_dataset_id, encoded
        );

        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.config.hf_token)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("upload failed for {remote_path}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "failed uploading {remote_path}, status={} body={}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    async fn upload_manifest(&self, manifest: &Manifest) -> Result<()> {
        let content = serde_json::to_vec_pretty(manifest)?;
        self.upload_file(MANIFEST_PATH, content).await
    }

    async fn load_local_manifest(&self) -> Result<Manifest> {
        let path = self.config.workspace_dir.join(MANIFEST_PATH);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let bytes = tokio::fs::read(path).await?;
            return Ok(serde_json::from_slice(&bytes).unwrap_or_default());
        }

        let remote = self.download_file(MANIFEST_PATH).await;
        match remote {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(_) => Ok(Manifest::default()),
        }
    }

    async fn save_local_manifest(&self, manifest: &Manifest) -> Result<()> {
        let path = self.config.workspace_dir.join(MANIFEST_PATH);
        let bytes = serde_json::to_vec_pretty(manifest)?;
        tokio::fs::write(path, bytes).await?;
        Ok(())
    }
}

fn split_dataset_id(dataset_id: &str) -> Result<(Option<String>, String)> {
    let mut parts = dataset_id.split('/');
    let first = parts
        .next()
        .ok_or_else(|| anyhow!("invalid HF_DATASET_ID"))?;
    let second = parts
        .next()
        .ok_or_else(|| anyhow!("invalid HF_DATASET_ID"))?;
    if parts.next().is_some() {
        return Err(anyhow!("invalid HF_DATASET_ID"));
    }
    Ok((Some(first.to_string()), second.to_string()))
}

fn collect_workspace_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_dir() {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(root)
            .context("strip prefix failed")?
            .to_string_lossy()
            .replace('\\', "/");

        if rel == MANIFEST_PATH {
            continue;
        }

        files.push(entry.into_path());
    }
    Ok(files)
}

fn url_encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let mut out = String::new();
            for b in segment.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        out.push(char::from(b))
                    }
                    _ => out.push_str(&format!("%{:02X}", b)),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn spawn_openclaw(command: &str) -> Result<Child> {
    let parts = shell_words::split(command).context("failed to parse START_COMMAND")?;
    if parts.is_empty() {
        return Err(anyhow!("START_COMMAND is empty"));
    }

    let mut cmd = Command::new(&parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }

    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    cmd.spawn().context("failed to spawn OpenClaw process")
}

async fn forward_signal(child: &mut Child, sig: &str) {
    let Some(pid) = child.id() else {
        return;
    };

    let signal_num = match sig {
        "TERM" => "-TERM",
        "INT" => "-INT",
        _ => return,
    };

    let status = Command::new("kill")
        .arg(signal_num)
        .arg(pid.to_string())
        .status()
        .await;

    match status {
        Ok(s) if s.success() => println!("[signal] forwarded {sig} to child {pid}"),
        Ok(s) => eprintln!("[signal] failed forwarding {sig}, status={s}"),
        Err(e) => eprintln!("[signal] failed forwarding {sig}: {e}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let sync = SyncManager::new(config.clone())?;

    sync.init_dataset().await?;
    sync.startup_pull().await?;

    println!("[boot] starting command: {}", config.start_command);
    let mut child = spawn_openclaw(&config.start_command).await?;

    let mut ticker = interval(Duration::from_secs(config.sync_interval));
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = sync.push_cycle().await {
                    eprintln!("[push] periodic sync failed: {e:#}");
                }
            }
            _ = sigterm.recv() => {
                println!("[signal] SIGTERM received");
                forward_signal(&mut child, "TERM").await;
                if let Err(e) = sync.push_cycle().await {
                    eprintln!("[push] final sync failed on SIGTERM: {e:#}");
                }
                break;
            }
            _ = sigint.recv() => {
                println!("[signal] SIGINT received");
                forward_signal(&mut child, "INT").await;
                if let Err(e) = sync.push_cycle().await {
                    eprintln!("[push] final sync failed on SIGINT: {e:#}");
                }
                break;
            }
            status = child.wait() => {
                let status = status.context("failed waiting for child process")?;
                println!("[proc] child exited with status: {status}");
                if let Err(e) = sync.push_cycle().await {
                    eprintln!("[push] final sync failed after child exit: {e:#}");
                }
                break;
            }
        }
    }

    Ok(())
}