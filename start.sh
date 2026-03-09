#!/usr/bin/env bash
set -euo pipefail

LITELLM_PID=""

# ── 1. Check if LiteLLM should be enabled ─────────────────────────────────────
# Both LITELLM_API_KEY and LITELLM_MODEL must be set to enable the proxy.
# If either is missing, OpenClaw starts without a pre-configured AI backend
# and the user can configure providers through OpenClaw's own UI.
if [ -z "${LITELLM_API_KEY:-}" ] || [ -z "${LITELLM_MODEL:-}" ]; then
    echo "[start.sh] LITELLM_API_KEY or LITELLM_MODEL not set — starting OpenClaw without LiteLLM proxy"
    echo "[start.sh] You can configure AI providers through OpenClaw's UI after startup"
    exec openclaw gateway --port 7860 --bind lan --allow-unconfigured
fi

# ── 2. Write LiteLLM proxy config ─────────────────────────────────────────────
LITELLM_CONFIG=/tmp/litellm_config.yaml

# OpenClaw's internally stored default model name — we register it in LiteLLM
# so any model name OpenClaw sends will be routed to the user's actual provider.
# The wildcard entry ("*") catches any other model name OpenClaw might send.
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
        exec openclaw gateway --port 7860 --bind lan --allow-unconfigured
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
echo "[start.sh] LiteLLM healthy after ${WAITED}s"

# ── 5. Start OpenClaw pointing at LiteLLM proxy ───────────────────────────────
exec env \
    OPENAI_API_KEY=litellm-proxy \
    OPENAI_BASE_URL=http://127.0.0.1:4000 \
    openclaw gateway --port 7860 --bind lan --allow-unconfigured