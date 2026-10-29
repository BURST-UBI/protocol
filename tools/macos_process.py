"""Stop only a spawned process group, retaining a ten-second cleanup bound."""
import os
import signal
import subprocess
import time


def members(pgid, deadline=None):
    rows = subprocess.check_output(['ps', '-axo', 'uid=,pid=,pgid=,stat='], text=True, timeout=2 if deadline is None else max(0.001, min(2, deadline-time.monotonic())))
    return [tuple(row.split()) for row in rows.splitlines()
            if len(row.split()) == 4 and row.split()[2] == str(pgid) and not row.split()[3].startswith('Z')]


def signal_group(pgid, sig, deadline=None):
    group = members(pgid, deadline)
    if not group:
        return
    if any(int(row[0]) != os.getuid() for row in group):
        raise PermissionError('Refuse to signal a group containing another user')
    try:
        os.killpg(pgid, sig)
    except (ProcessLookupError, PermissionError):
        # Darwin can reject a signal after the final member has exited.
        if members(pgid, deadline):
            raise


def stop_group(child):
    deadline = time.monotonic() + 10
    signal_group(child.pid, signal.SIGTERM, deadline)
    try:
        child.wait(timeout=max(0, deadline - time.monotonic() - 1))
    except subprocess.TimeoutExpired:
        pass
    signal_group(child.pid, signal.SIGKILL, deadline)
    child.wait(timeout=max(0, deadline - time.monotonic()))
    while members(child.pid, deadline):
        if time.monotonic() >= deadline:
            raise TimeoutError('Owned process group did not stop within ten seconds')
        time.sleep(0.02)
