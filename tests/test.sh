#!/bin/bash
# Verifier entry point. Rebuilds the agent's crate from /app as an unprivileged
# user, runs the hidden case suite against the binary, and writes the fraction
# of passing cases to /logs/verifier/reward.txt.
set -u
mkdir -p /logs/verifier
echo 0 > /logs/verifier/reward.txt
python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run_cases.py"
