#!/usr/bin/env python3
"""Builds /app in release mode and runs the public examples in examples/cases.jsonl.

usage: python3 run_examples.py [-v]

Each case gives the command-line arguments, the text fed to stdin, the exact
expected stdout and the expected exit status. stderr is never compared.
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
verbose = "-v" in sys.argv[1:]

build = subprocess.run(["cargo", "build", "--release", "--offline"], cwd=HERE)
if build.returncode != 0:
    sys.exit("build failed")
binary = os.path.join(HERE, "target", "release", "jqlite")

passed = 0
with open(os.path.join(HERE, "examples", "cases.jsonl"), encoding="utf-8") as f:
    cases = [json.loads(line) for line in f if line.strip()]
for case in cases:
    try:
        p = subprocess.run([binary] + case["args"], input=case["stdin"].encode(), capture_output=True, timeout=10)
        got_out, got_code = p.stdout.decode(errors="replace"), p.returncode
    except subprocess.TimeoutExpired:
        got_out, got_code = "<timeout>", None
    ok = got_out == case["stdout"] and got_code == case["exit_code"]
    passed += ok
    if not ok or verbose:
        print(f"[{'PASS' if ok else 'FAIL'}] #{case['id']} {case['category']}: jqlite {' '.join(map(repr, case['args']))}")
        if not ok:
            print(f"    stdin:    {case['stdin']!r}")
            print(f"    expected: {case['stdout']!r} (exit {case['exit_code']})")
            print(f"    got:      {got_out!r} (exit {got_code})")
print(f"{passed}/{len(cases)} public examples passed")
