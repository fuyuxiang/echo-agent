#!/bin/sh
# session-log.sh — append session events to an audit log
#
# Reads the hook envelope from stdin and appends a one-line JSON entry
# to ~/.echo-agent/session-audit.log with event name, session ID, cwd, and
# timestamp.

INPUT=$(cat)

EVENT=$(echo "$INPUT" | grep -o '"hookEventName":"[^"]*"' | sed 's/"hookEventName":"//;s/"$//')
SESSION=$(echo "$INPUT" | grep -o '"sessionId":"[^"]*"' | sed 's/"sessionId":"//;s/"$//')
CWD=$(echo "$INPUT" | grep -o '"cwd":"[^"]*"' | sed 's/"cwd":"//;s/"$//')
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

LOG_FILE="${HOME}/.echo-agent/session-audit.log"
mkdir -p "$(dirname "$LOG_FILE")"

echo "{\"timestamp\":\"${TIMESTAMP}\",\"event\":\"${EVENT}\",\"session\":\"${SESSION}\",\"cwd\":\"${CWD}\"}" >> "$LOG_FILE"