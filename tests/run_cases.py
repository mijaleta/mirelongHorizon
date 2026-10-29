#!/usr/bin/env python3
"""Hidden verifier for the jqlite task.

1. Stops any process still running as the agent user and makes this directory
   root-only, so neither the build nor the candidate binary can read the cases.
2. Copies /app (without target/ and .git/, symlinks kept as links) to a fresh
   directory and runs `cargo build --release --offline` there as the agent user.
3. Runs every case in cases.jsonl against the built binary, again as the agent
   user. A case passes when stdout matches byte for byte and the exit status
   matches. stderr is ignored.
4. Writes passed / total to /logs/verifier/reward.txt.
"""
import json
import os
import pwd
import shutil
import subprocess
import sys
from collections import defaultdict

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
APP_DIR = "/app"
BUILD_DIR = "/tmp/jqlite-verify/src"
BUILD_HOME = "/tmp/jqlite-verify/home"
BIN_DIR = "/tmp/jqlite-verify/bin"
REWARD = "/logs/verifier/reward.txt"
DETAILS = "/logs/verifier/details.json"
BUILD_TIMEOUT = 900
CASE_TIMEOUT = 10


def write_reward(score, details):
    with open(REWARD, "w") as f:
        f.write(f"{score:.4f}\n")
    with open(DETAILS, "w") as f:
        json.dump(details, f, indent=2)
    print(f"reward: {score:.4f}")


def chown_tree(path, uid, gid):
    os.lchown(path, uid, gid)
    for root, dirs, files in os.walk(path):
        for name in dirs + files:
            os.lchown(os.path.join(root, name), uid, gid)


def as_agent(agent, **kwargs):
    return dict(user=agent.pw_uid, group=agent.pw_gid, extra_groups=[], **kwargs)


def main():
    agent = pwd.getpwnam("agent")
    subprocess.run(["pkill", "-9", "-u", "agent"], check=False)
    os.chmod(TESTS_DIR, 0o700)

    with open(os.path.join(TESTS_DIR, "cases.jsonl"), encoding="utf-8") as f:
        cases = [json.loads(line) for line in f if line.strip()]

    shutil.rmtree("/tmp/jqlite-verify", ignore_errors=True)
    if not os.path.isfile(os.path.join(APP_DIR, "Cargo.toml")):
        print("no Cargo.toml in /app")
        write_reward(0.0, {"build": "missing Cargo.toml"})
        return
    shutil.copytree(APP_DIR, BUILD_DIR, symlinks=True, ignore=shutil.ignore_patterns("target", ".git"))
    os.makedirs(BUILD_HOME)
    chown_tree("/tmp/jqlite-verify", agent.pw_uid, agent.pw_gid)

    env = {
        "PATH": "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME": BUILD_HOME,
        "CARGO_HOME": os.path.join(BUILD_HOME, "cargo"),
        "RUSTUP_HOME": "/usr/local/rustup",
        "CARGO_NET_OFFLINE": "true",
        "LANG": "C.UTF-8",
    }
    try:
        build = subprocess.run(
            ["cargo", "build", "--release", "--offline"],
            **as_agent(agent, cwd=BUILD_DIR, env=env, capture_output=True, timeout=BUILD_TIMEOUT),
        )
        build_ok = build.returncode == 0
        build_log = build.stderr.decode(errors="replace")[-4000:]
    except subprocess.TimeoutExpired:
        build_ok, build_log = False, "build timed out"
    built = os.path.join(BUILD_DIR, "target", "release", "jqlite")
    if not build_ok or not os.path.isfile(built):
        print("build failed:\n" + build_log)
        write_reward(0.0, {"build": "failed", "log": build_log})
        return

    # Run a root-owned copy so a candidate cannot rewrite its own binary mid-suite.
    os.makedirs(BIN_DIR)
    binary = os.path.join(BIN_DIR, "jqlite")
    shutil.copy(built, binary)
    os.chmod(binary, 0o755)

    run_env = {"PATH": "/usr/bin:/bin", "HOME": BUILD_HOME, "LANG": "C.UTF-8"}
    passed = 0
    by_category = defaultdict(lambda: [0, 0])
    failures = []
    for case in cases:
        try:
            p = subprocess.run(
                [binary] + case["args"],
                **as_agent(agent, input=case["stdin"].encode(), cwd=BUILD_HOME, env=run_env,
                           capture_output=True, timeout=CASE_TIMEOUT),
            )
            ok = p.stdout == case["stdout"].encode() and p.returncode == case["exit_code"]
        except subprocess.TimeoutExpired:
            ok = False
        passed += ok
        by_category[case["category"]][0] += ok
        by_category[case["category"]][1] += 1
        if not ok:
            failures.append(case["id"])

    subprocess.run(["pkill", "-9", "-u", "agent"], check=False)
    for cat, (good, total) in sorted(by_category.items()):
        print(f"{cat:12s} {good:3d}/{total}")
    print(f"total        {passed:3d}/{len(cases)}")
    write_reward(
        passed / len(cases),
        {"passed": passed, "total": len(cases), "by_category": dict(by_category), "failed_ids": failures},
    )


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # never leave a stale reward behind
        print(f"verifier error: {exc!r}", file=sys.stderr)
        write_reward(0.0, {"verifier_error": repr(exc)})
