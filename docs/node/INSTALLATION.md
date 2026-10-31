# Node installation

The repository's installation scripts register a daemon as a background service.
They download release artifacts, so their presence does not establish that a
particular release is suitable for production. Check the exact artifact and its
[qualification limits](../release/DOWNLOADS.md) first. For local source development,
use the [development setup](../development/DEVELOPING.md).

Review `install.sh` or `install.ps1` from the revision you intend to use before
running it. Linux installation needs root; macOS uses the current user's
LaunchAgents; Windows installation needs an elevated PowerShell session.
These scripts install the daemon, not the desktop wallet or dashboard.

## Files and services

Default paths are shown below. Existing configuration may select another data
directory.

| Item | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Executable | `/usr/local/bin/burst-daemon` | `~/Library/Application Support/BURST/bin/burst-daemon` | `C:\ProgramData\BURST\burst-daemon.exe` |
| Configuration | `/etc/burst/config.toml` | `~/Library/Application Support/BURST/config.toml` | `C:\ProgramData\BURST\config.toml` |
| Ledger | `/var/lib/burst` | `~/Library/Application Support/BURST/data` | `C:\ProgramData\BURST\data` |
| Service | systemd `burst-node` | launchd `com.burst.node` | Windows `BurstNode` |
| Update schedule | Every 5 minutes | Every 5 minutes | Every 30 minutes |

The scripts verify downloads against the release checksum manifest, create a
configuration when one is absent and enable automatic updates. RPC and WebSocket
bind to localhost by default. Installation does not supply a bootstrap peer or
enable test funding; configure a network deliberately.

## Inspect and restart

Linux:

```bash
systemctl status burst-node
journalctl -u burst-node -f
sudo systemctl restart burst-node
```

macOS:

```bash
tail -f "$HOME/Library/Application Support/BURST/burst-node.log"
launchctl kickstart -k "gui/$(id -u)/com.burst.node"
```

Windows PowerShell:

```powershell
Get-Service BurstNode
Restart-Service BurstNode
```

## Updates and recovery

Updates download to temporary storage and keep the previous executable.
Unix names it `burst-daemon.previous.*`; Windows uses
`burst-daemon.exe.previous-*`. Updating a stopped service does not start it.
Inspect service-manager errors if an update fails.

An older executable is not a ledger rollback. Check schema compatibility before
restoring it, and preserve database backups and local signing commitments as
described in [node recovery](NODES.md).

An interrupted Unix updater may leave a `burst-daemon.update-lock` directory
beside the executable. Confirm that no updater is running before removing a
stale lock.

## Uninstall

Run the reviewed local installation script with `--uninstall`:

```bash
sudo sh ./install.sh --uninstall  # Linux
sh ./install.sh --uninstall       # macOS
```

On Windows, run `./install.ps1 --uninstall` in elevated PowerShell.
The scripts stop and unregister their services and remove the daemon.
Linux and Windows also remove the installed configuration; macOS retains it.
Ledger directories remain. Preserve recovery material before separately removing
any data. Desktop application removal follows its own package's uninstaller.

## Implementation

- [Unix installer](https://github.com/BURST-UBI/protocol/blob/main/install.sh)
- [Unix updater](https://github.com/BURST-UBI/protocol/blob/main/tools/update-node.sh)
- [Windows installer and updater](https://github.com/BURST-UBI/protocol/blob/main/install.ps1)
