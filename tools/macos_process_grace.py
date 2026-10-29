"""Give the entire owned group its existing termination grace, not only Cargo."""
import json
import signal
import sys
import time
from macos_process import members, signal_group


def stop_group(child):
    deadline = time.monotonic() + 10
    signal_group(child.pid, signal.SIGTERM, deadline)
    while time.monotonic() < deadline - 1:
        child.poll()
        if not members(child.pid, deadline):
            child.wait(timeout=max(0, deadline - time.monotonic()))
            return
        time.sleep(min(0.05, max(0, deadline - 1 - time.monotonic())))
    remaining = members(child.pid, deadline)
    if remaining:
        print('Owned group at kill deadline: ' + json.dumps(remaining), file=sys.stderr)
        signal_group(child.pid, signal.SIGKILL, deadline)
    child.wait(timeout=max(0, deadline - time.monotonic()))
    while members(child.pid, deadline):
        if time.monotonic() >= deadline:
            raise TimeoutError('Owned process group did not stop within ten seconds')
        time.sleep(min(0.02, max(0, deadline - time.monotonic())))
