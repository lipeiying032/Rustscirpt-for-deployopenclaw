#!/usr/bin/env bash
set -euo pipefail

# ── 1. Validate required variables ────────────────────────────────────────────
: "${LITELLM_API_KEY:?Secret LITELLM_API_KEY is required}"
: "${LITELLM_MODEL:?Secret LITELLM_MODEL is required (e.g. openai/gpt-4o or anthropic/claude-3-5-sonnet-20241022)}"

# ── 2. Write LiteLLM proxy config ─────────────────────────────────────────────
LITELLM_CONFIG=/tmp/litellm_config.yaml

{
  echo "model_list:"
  echo "  - model_name: default"
  echo "    litellm_params:"
  echo "      model: \"${LITELLM_MODEL}\""
  echo "      api_key: \"${LITELLM_API_KEY}\""
  if [ -n "${LITELLM_API_BASE:-}" ]; then
    echo "      api_base: \"${LITELLM_API_BASE}\""
  fi
  echo ""
  echo "litellm_settings:"
  echo "  drop_params: true"
  echo "  num_retries: 3"
  echo "  request_timeout: 120"
} > "$LITELLM_CONFIG"

echo "[start.sh] LiteLLM config:"
cat "$LITELLM_CONFIG"

# ── 3. Start LiteLLM proxy in the background ──────────────────────────────────
litellm --config "$LITELLM_CONFIG" --port 4000 --host 127.0.0.1 &
LITELLM_PID=$!
echo "[start.sh] LiteLLM proxy started (pid=$LITELLM_PID)"

# Kill LiteLLM on any exit
trap 'echo "[start.sh] shutting down LiteLLM"; kill "$LITELLM_PID" 2>/dev/null || true' EXIT TERM INT

# ── 4. Wait for LiteLLM to be healthy (max 60 s) ─────────────────────────────
MAX_WAIT=60
WAITED=0
until curl -sf http://127.0.0.1:4000/health/liveliness > /dev/null 2>&1; do
    if [ "$WAITED" -ge "$MAX_WAIT" ]; then
        echo "[start.sh] ERROR: LiteLLM not healthy after ${MAX_WAIT}s"
        exit 1
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
echo "[start.sh] LiteLLM healthy after ${WAITED}s"

# ── 5. Start OpenClaw gateway pointing at LiteLLM ─────────────────────────────
# OPENAI_API_KEY / OPENAI_BASE_URL: tell OpenClaw to use LiteLLM as its backend.
# OPENCLAW_DEFAULT_MODEL=default:   matches the model_name in litellm_config.yaml.
# OPENCLAW_API_PORT is already set to 7860 in the Dockerfile ENV.
exec env \
    OPENCLAW_DEFAULT_MODEL=default \
    OPENAI_API_KEY=litellm-proxy \
    OPENAI_BASE_URL=http://127.0.0.1:4000 \
    openclaw gateway --allow-unconfigured