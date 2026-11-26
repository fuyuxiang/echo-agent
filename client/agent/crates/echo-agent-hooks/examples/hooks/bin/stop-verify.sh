#!/bin/sh
# stop-verify.sh — keep the agent working until the build passes.
#
# A Stop hook runs when the agent is about to finish its turn. Emitting a
# block decision feeds the reason back to the model and runs another round;
# the built-in cap ends the turn after 8 continuations. Set a generous
# timeout on the hook (see stop-verify.json), since a timed-out hook fails
# open and lets the agent stop.

INPUT=$(cat)

# Gate only genuine turn ends, not the observe-only session-end fire.
REASON=$(echo "$INPUT" | grep -o '"reason":"[^"]*"' | sed 's/"reason":"//;s/"$//')
if [ "$REASON" != "end_turn" ]; then
  exit 0
fi

if cargo build --quiet >/dev/null 2>&1; then
  # Build is green: allow the stop.
  exit 0
fi

# Build is red: keep the agent working, with the failure as feedback.
echo '{"decision":"block","reason":"cargo build failed; fix the errors before finishing."}'
