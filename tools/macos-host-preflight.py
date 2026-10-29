"""Read-only hosted macOS inventory; no build or product qualification."""
import json
import os
import pathlib
import platform
import subprocess

root = pathlib.Path("macos-host-evidence")
root.mkdir(exist_ok=False)
commands = {
    "os": ["sw_vers"],
    "hardware": ["sysctl", "hw.ncpu", "hw.memsize"],
    "swap": ["sysctl", "vm.swapusage"],
    "memory": ["vm_stat"],
    "disk": ["df", "-k", "."],
    "xcode": ["xcode-select", "-p"],
    "sdk": ["xcrun", "--show-sdk-version"],
    "clang": ["xcrun", "clang", "--version"],
    "rust": ["rustup", "toolchain", "list"],
    "node": ["node", "--version"],
    "console": ["stat", "-f", "%Su", "/dev/console"],
    "gui": ["launchctl", "print", "gui/" + str(os.getuid())],
    "limits": ["launchctl", "limit"],
    "taskpolicy": ["sh", "-c", "command -v taskpolicy"],
}
results = {}
for name, command in commands.items():
    try:
        run = subprocess.run(command, text=True, capture_output=True, timeout=15)
        results[name] = {"command": command, "exitCode": run.returncode}
        # The GUI domain can include runner environment values; retain only availability.
        if name != "gui":
            (root / (name + ".log")).write_text(run.stdout + run.stderr)
    except (OSError, subprocess.TimeoutExpired) as error:
        results[name] = {"command": command, "error": type(error).__name__}
required = ["os", "hardware", "swap", "memory", "disk", "xcode", "sdk", "clang", "node"]
passed = platform.system() == "Darwin" and platform.machine() == os.environ["EXPECTED_ARCH"]
passed = passed and all(results[key].get("exitCode") == 0 for key in required)
report = {"outcome": "pass" if passed else "fail", "scope": "Host inventory only; no build, app runtime, resource enforcement or release qualification", "architecture": platform.machine(), "system": platform.system(), "runId": os.environ.get("GITHUB_RUN_ID"), "workflowCommit": os.environ.get("GITHUB_SHA"), "commands": results}
(root / "result.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps({key: report[key] for key in ["outcome", "scope", "architecture"]}))
raise SystemExit(0 if passed else 1)
