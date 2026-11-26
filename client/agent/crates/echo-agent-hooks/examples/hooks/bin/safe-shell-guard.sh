#!/bin/sh
# safe-shell-guard.sh — block obviously destructive shell commands
#
# This hook reads the PreToolUse envelope from stdin, extracts the
# command field from toolInput, and checks it against a blocklist.
#
# Returns {"decision":"deny","reason":"..."} + exit 2 for matches,
# {"decision":"allow"} + exit 0 otherwise.

INPUT=$(cat)

# Extract the command from the toolInput JSON.
# Uses basic grep/sed since jq may not be available everywhere.
COMMAND=$(echo "$INPUT" | grep -o '"command":"[^"]*"' | head -1 | sed 's/"command":"//;s/"$//')

if [ -z "$COMMAND" ]; then
  echo '{"decision":"allow"}'
  exit 0
fi

# Blocklist patterns (case-insensitive check).
LOWER_CMD=$(echo "$COMMAND" | tr '[:upper:]' '[:lower:]')

case "$LOWER_CMD" in
  *"rm -rf /"*|*"rm -rf --no-preserve-root"*)
    echo '{"decision":"deny","reason":"Blocked: rm -rf / is not allowed"}'
    exit 2
    ;;
  *"sudo rm -rf"*)
    echo '{"decision":"deny","reason":"Blocked: sudo rm -rf is not allowed"}'
    exit 2
    ;;
  *"mkfs"*)
    echo '{"decision":"deny","reason":"Blocked: mkfs commands are not allowed"}'
    exit 2
    ;;
  *"dd if=/dev/zero of=/dev"*|*"dd if=/dev/urandom of=/dev"*)
    echo '{"decision":"deny","reason":"Blocked: dd to device is not allowed"}'
    exit 2
    ;;
  *":(){"|*"fork bomb"*)
    echo '{"decision":"deny","reason":"Blocked: fork bomb detected"}'
    exit 2
    ;;
  *"> /dev/sda"*|*"> /dev/hda"*|*"> /dev/nvme"*)
    echo '{"decision":"deny","reason":"Blocked: direct write to block device"}'
    exit 2
    ;;
esac

echo '{"decision":"allow"}'
exit 0