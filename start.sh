#!/usr/bin/env bash
set -euo pipefail

# ── 1. Validate required LiteLLM vars ─────────────────────────────────────────
: "${LITELLM_API_KEY:?LITELLM_API_KEY is required}"
: "${LITELLM_MODEL:?LITELLM_MODEL is required (e.g. openai/gpt-4o or anthropic/claude-3-5-sonnet-20241022)}"

# ── 2. Generate LiteLLM proxy config ──────────────────────────────────────────
LITELLM_CONFIG=/tmp/litellm_config.yaml

cat > "$LITELLM_CONFIG" << YAML
model_list:
  - model_name: default
    litellm_params:
      model: "${LITELLM_MODEL}"
      api_key: "${LITELLM_API_KEY}"
$(if [ -n "${LITELLM_API_BASE:-}" ]; then echo "      api_base: \"${LITELLM_API_BASE}\""; fi)

litellm_settings:
  drop_params: true
  num_retries: 3
  request_timeout: 120
YAML

echo "[start.sh] LiteLLM config written to $LITELLM_CONFIG"

# ── 3. Start LiteLLM proxy in the background ──────────────────────────────────
litellm --config "$LITELLM_CONFIG" --port 4000 --host 127.0.0.1 &
LITELLM_PID=$!
echo "[start.sh] LiteLLM proxy started (pid $LITELLM_PID)"

# ── 4. Wait for LiteLLM to be healthy ─────────────────────────────────────────
MAX_WAIT=30
WAITED=0
until curl -sf http://127.0.0.1:4000/health/liveliness > /dev/null 2>&1; do
    if [ "$WAITED" -ge "$MAX_WAIT" ]; then
        echo "[start.sh] ERROR: LiteLLM did not become healthy within ${MAX_WAIT}s"
        exit 1
    fi
    sleep 1
    WAITED=$((WAITED + 1))
done
echo "[start.sh] LiteLLM is healthy after ${WAITED}s"

# ── 5. Start OpenClaw pointing to the LiteLLM proxy ──────────────────────────
# We expose LiteLLM as an OpenAI-compatible endpoint.
# OPENCLAW_DEFAULT_MODEL must match the model_name we set above ("default").
exec env \
    OPENCLAW_DEFAULT_MODEL=default \
    OPENAI_API_KEY=litellm-proxy \
    OPENAI_BASE_URL=http://127.0.0.1:4000 \
    node openclaw.mjs gateway --allow-unconfigured