#!/usr/bin/env bash
set -euo pipefail

LITELLM_PID=""

# ── 0. Write openclaw.json before gateway starts ──────────────────────────────
OPENCLAW_CONFIG_DIR="${HOME}/.openclaw"
OPENCLAW_CONFIG="${OPENCLAW_CONFIG_DIR}/openclaw.json"
mkdir -p "$OPENCLAW_CONFIG_DIR"

if [ ! -f "$OPENCLAW_CONFIG" ]; then
  cat > "$OPENCLAW_CONFIG" << 'OPENCLAW_JSON'
{
  "gateway": {
    "bind": "lan",
    "controlUi": {
      "dangerouslyAllowHostHeaderOriginFallback": true
    }
  }
}
OPENCLAW_JSON
  echo "[start.sh] openclaw.json written"
else
  echo "[start.sh] openclaw.json already exists, skipping write"
fi

# ── 1. Check if LiteLLM should be enabled ─────────────────────────────────────
if [ -z "${LITELLM_API_KEY:-}" ] || [ -z "${LITELLM_MODEL:-}" ]; then
    echo "[start.sh] LITELLM_API_KEY or LITELLM_MODEL not set — starting OpenClaw without LiteLLM proxy"
    exec openclaw gateway --port 7860 --allow-unconfigured
fi

# ── 2. Write LiteLLM proxy config ─────────────────────────────────────────────
LITELLM_CONFIG=/tmp/litellm_config.yaml

{
  echo "model_list:"
  echo "  - model_name: \"*\""
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

echo "[start.sh] LiteLLM config written for model: ${LITELLM_MODEL}"

# ── 3. Start LiteLLM proxy in the background ──────────────────────────────────
litellm --config "$LITELLM_CONFIG" --port 4000 --host 127.0.0.1 &
LITELLM_PID=$!
echo "[start.sh] LiteLLM started (pid=$LITELLM_PID)"

cleanup() {
    if [ -n "$LITELLM_PID" ]; then
        echo "[start.sh] stopping LiteLLM (pid=$LITELLM_PID)"
        kill "$LITELLM_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── 4. Wait for LiteLLM to be healthy (max 60 s) ─────────────────────────────
MAX_WAIT=60
WAITED=0
until curl -sf http://127.0.0.1:4000/health/liveliness > /dev/null 2>&1; do
    if [ "$WAITED" -ge "$MAX_WAIT" ]; then
        echo "[start.sh] WARNING: LiteLLM not healthy after ${MAX_WAIT}s — starting OpenClaw without proxy"
        exec openclaw gateway --port 7860 --allow-unconfigured
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
echo "[start.sh] LiteLLM healthy after ${WAITED}s"

# ── 5. Start OpenClaw pointing at LiteLLM proxy ───────────────────────────────
exec env \
    OPENAI_API_KEY=litellm-proxy \
    OPENAI_BASE_URL=http://127.0.0.1:4000 \
    openclaw gateway --port 7860 --allow-unconfigured