#!/bin/sh
# tool-logger.sh — log tool calls to a local activity file
#
# Reads the hook envelope from stdin and appends a one-line JSON entry
# to ~/.echo-agent/tool-activity.log with event name, tool name, and timestamp.
# `toolName` is the resolved tool (e.g. `linear__save_issue` for MCP calls).

INPUT=$(cat)

EVENT=$(echo "$INPUT" | grep -o '"hookEventName":"[^"]*"' | sed 's/"hookEventName":"//;s/"$//')
TOOL=$(echo "$INPUT" | grep -o '"toolName":"[^"]*"' | head -1 | sed 's/"toolName":"//;s/"$//')
BACKGROUNDED=$(echo "$INPUT" | grep -o '"isBackgrounded":[a-z]*' | sed 's/"isBackgrounded"://')
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

LOG_FILE="${HOME}/.echo-agent/tool-activity.log"
mkdir -p "$(dirname "$LOG_FILE")"

echo "{\"timestamp\":\"${TIMESTAMP}\",\"event\":\"${EVENT}\",\"tool\":\"${TOOL}\",\"backgrounded\":${BACKGROUNDED:-false}}" >> "$LOG_FILE"
