#!/bin/bash
# AgentBox Entrypoint — ensure agy has auth token, then start wrapper
set -e

TOKEN_FILE="/root/.gemini/antigravity-cli/antigravity-oauth-token"

# If no token yet, try to use mounted host token
if [ ! -f "$TOKEN_FILE" ]; then
    if [ -f "/host-token.json" ]; then
        echo "[entrypoint] Copying host token..."
        cp /host-token.json "$TOKEN_FILE"
    else
        echo "[entrypoint] No auth token found!"
        echo "  Run on host to extract from Keychain:"
        echo "    security find-generic-password -s gemini -a antigravity -w | sed 's/^go-keyring-base64://' | base64 -d > antigravity-oauth-token"
        echo "  Then mount it as /host-token.json"
        echo "  Or run: docker exec -it <container> agy  (one-time interactive OAuth)"
        exit 1
    fi
fi

echo "[entrypoint] Auth token ready, starting agentbox-wrapper..."
exec agentbox-wrapper "$@"
