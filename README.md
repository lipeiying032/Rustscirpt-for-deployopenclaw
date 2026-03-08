---
title: RustForHuggingClaw
emoji: 🦀
colorFrom: indigo
colorTo: red
sdk: docker
app_port: 7860
datasets:
- your-username/your-space-name-data
---

# HuggingClaw 🦀

Deploy [OpenClaw](https://openclaw.ai) on HuggingFace Spaces for free —
2 vCPU · 16 GB RAM · 50 GB storage · always online.

AI provider calls are routed through **LiteLLM Proxy** so you can plug in
any provider (OpenAI, Anthropic, Gemini, Azure, OpenRouter, Ollama, local
endpoints …) with just two variables.

---

## Required Secrets

Set these in **Space → Settings → Repository secrets**:

| Secret | Example | Description |
|--------|---------|-------------|
| `HF_TOKEN` | `hf_xxx…` | HuggingFace token with **read + write** access |
| `OPENCLAW_DATASET_REPO` | `your-name/HuggingClaw-data` | Private dataset repo used to persist OpenClaw config |
| `LITELLM_API_KEY` | `sk-xxx…` | Your AI provider API key |
| `LITELLM_MODEL` | `openai/gpt-4o` | Model in LiteLLM format (see table below) |

---

## Optional Secrets

| Secret | Default | Description |
|--------|---------|-------------|
| `LITELLM_API_BASE` | *(provider default)* | Custom API endpoint — required for Azure, OpenRouter, local, etc. |
| `AUTO_CREATE_DATASET` | `false` | Set `true` to auto-create the dataset repo on first boot |
| `SYNC_INTERVAL` | `60` | Seconds between workspace sync pushes to HuggingFace |

---

## Model Examples

| Provider | `LITELLM_MODEL` | `LITELLM_API_BASE` |
|----------|----------------|-------------------|
| OpenAI | `openai/gpt-4o` | *(leave empty)* |
| Anthropic | `anthropic/claude-3-5-sonnet-20241022` | *(leave empty)* |
| Google Gemini | `gemini/gemini-1.5-pro` | *(leave empty)* |
| Azure OpenAI | `azure/<deployment-name>` | `https://<resource>.openai.azure.com` |
| OpenRouter | `openrouter/openai/gpt-4o` | `https://openrouter.ai/api/v1` |
| Ollama (local) | `ollama/llama3` | `http://localhost:11434` |
| Any OpenAI-compat | `openai/<model-name>` | your custom base URL |

---

## Setup

1. **Duplicate this Space** on the HuggingFace Space page.
2. Create a **private Dataset repo** at [huggingface.co/new-dataset](https://huggingface.co/new-dataset).
3. Set all **Required Secrets** above.
4. Edit this README: update the `datasets:` field to match your dataset repo.
5. The Space will build; OpenClaw will be available on port **7860**.

---

## Architecture

```
HF Space (Docker)
├── openclaw-hf-sync  ← Rust binary (pid 1 via tini)
│   ├── on boot  : pull ~/.openclaw from HF dataset
│   ├── every 60s: push ~/.openclaw changes to HF dataset
│   └── on exit  : final push
└── start.sh  ← child process
    ├── LiteLLM Proxy  (127.0.0.1:4000)  ← routes to your AI provider
    └── OpenClaw Gateway  (:7860)  ← points to LiteLLM Proxy
```