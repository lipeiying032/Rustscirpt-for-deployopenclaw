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

Deploy [OpenClaw](https://openclaw.ai) on HuggingFace Spaces for free — 2 vCPU, 16 GB RAM, 50 GB storage, always online.

## Required Secrets

Set these in your Space → Settings → Repository secrets:

| Secret | Description |
|--------|-------------|
| `HF_TOKEN` | Your HuggingFace token (needs read + write access) |
| `OPENCLAW_DATASET_REPO` | Dataset repo to persist data, e.g. `your-name/HuggingClaw-data` |

## Optional Secrets

| Secret | Default | Description |
|--------|---------|-------------|
| `AUTO_CREATE_DATASET` | `false` | Set `true` to auto-create the dataset repo on first start |
| `SYNC_INTERVAL` | `60` | Seconds between workspace sync pushes |
| `OPENCLAW_DEFAULT_MODEL` | — | Default AI model, e.g. `gpt-4o` |
| `ANTHROPIC_API_KEY` | — | Anthropic API key |
| `OPENAI_API_KEY` | — | OpenAI API key |
| `OPENROUTER_API_KEY` | — | OpenRouter API key |

## Setup

1. **Duplicate this Space** on the HuggingFace Space page.
2. Create a **private Dataset repo** at [huggingface.co/new-dataset](https://huggingface.co/new-dataset).
3. Set `OPENCLAW_DATASET_REPO` and `HF_TOKEN` as Repository Secrets.
4. Edit this README and update the `datasets:` field above to match your dataset repo.
5. The Space will build and OpenClaw will be available on port 7860.