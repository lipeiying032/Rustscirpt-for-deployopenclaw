#!/usr/bin/env bash
set -euo pipefail

LITELLM_PID=""

# ── 0. Write openclaw.json ─────────────────────────────────────────────────────
OPENCLAW_CONFIG_DIR="${HOME}/.openclaw"
mkdir -p "$OPENCLAW_CONFIG_DIR"

GATEWAY_TOKEN="${OPENCLAW_GATEWAY_TOKEN:-openclaw-hf-default-token}"

# Register LiteLLM proxy as a custom provider directly in openclaw.json.
# apiKey goes in models.providers (not auth-profiles.json) for custom providers.
# agents.defaults.model points to litellm/default so OpenClaw stops trying Anthropic.
cat > "${OPENCLAW_CONFIG_DIR}/openclaw.json" << OPENCLAW_JSON
{
  "gateway": {
    "bind": "lan",
    "auth": {
      "mode": "token",
      "token": "${GATEWAY_TOKEN}"
    },
    "controlUi": {
      "allowInsecureAuth": true,
      "dangerouslyDisableDeviceAuth": true,
      "dangerouslyAllowHostHeaderOriginFallback": true
    }
  },
  "models": {
    "providers": {
      "litellm": {
        "baseUrl": "http://127.0.0.1:4000",
        "apiKey": "litellm-proxy",
        "api": "openai-responses",
        "models": [
          {
            "id": "default",
            "name": "LiteLLM Proxy (${LITELLM_MODEL:-custom})",
            "contextWindow": 200000,
            "maxTokens": 8192,
            "input": ["text", "image"],
            "reasoning": false
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": "litellm/default"
    }
  }
}
OPENCLAW_JSON

echo "[start.sh] openclaw.json written (provider=litellm, token=${GATEWAY_TOKEN:0:8}...)"
echo "[start.sh] Access UI at: https://<your-space>.hf.space/#token=${GATEWAY_TOKEN}"

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

# ── 5. Start OpenClaw ─────────────────────────────────────────────────────────
exec openclaw gateway --port 7860 --allow-unconfigured