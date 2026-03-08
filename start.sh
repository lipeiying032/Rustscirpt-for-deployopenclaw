#!/usr/bin/env bash
set -euo pipefail

# ── 1. Validate required LiteLLM variables ────────────────────────────────────
: "${LITELLM_API_KEY:?LITELLM_API_KEY secret is required}"
: "${LITELLM_MODEL:?LITELLM_MODEL secret is required (e.g. openai/gpt-4o)}"

# ── 2. Generate LiteLLM proxy config dynamically ──────────────────────────────
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

echo "[start.sh] LiteLLM config written:"
cat "$LITELLM_CONFIG"

# ── 3. Start LiteLLM proxy in the background ──────────────────────────────────
litellm --config "$LITELLM_CONFIG" --port 4000 --host 127.0.0.1 &
LITELLM_PID=$!
echo "[start.sh] LiteLLM proxy started (pid $LITELLM_PID)"

# Forward SIGTERM to LiteLLM when we exit
trap 'kill $LITELLM_PID 2>/dev/null || true' EXIT TERM INT

# ── 4. Wait for LiteLLM to become healthy (max 30 s) ─────────────────────────
MAX_WAIT=30
WAITED=0
until curl -sf http://127.0.0.1:4000/health/liveliness > /dev/null 2>&1; do
    if [ "$WAITED" -ge "$MAX_WAIT" ]; then
        echo "[start.sh] ERROR: LiteLLM did not become healthy within ${MAX_WAIT}s — aborting"
        exit 1
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
echo "[start.sh] LiteLLM healthy after ${WAITED}s"

# ── 5. Launch OpenClaw gateway pointing at the LiteLLM proxy ─────────────────
# We expose LiteLLM as an OpenAI-compatible endpoint on 127.0.0.1:4000.
# OPENCLAW_DEFAULT_MODEL must match the model_name defined above ("default").
exec env \
    OPENCLAW_DEFAULT_MODEL=default \
    OPENAI_API_KEY=litellm-proxy \
    OPENAI_BASE_URL=http://127.0.0.1:4000 \
    openclaw gateway --allow-unconfigured