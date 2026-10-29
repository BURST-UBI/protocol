# Downloads

BURST includes a protocol daemon, a Tauri desktop wallet and a Tauri node
dashboard. The daemon stores and validates the ledger and provides node services.
The wallet manages local keys and submits signed actions to a configured node.
The dashboard reads node status, network activity and governance information.

Published daemon artifacts are listed on the
[GitHub releases page](https://github.com/BURST-UBI/protocol/releases).
Archived builds are development artifacts, not a declaration that the current
production qualification gates have passed. Read the notes for the exact version
and platform before using a download. Wallet and dashboard candidates are not
advertised here as public releases until their release is approved.

## Code signing policy

[Code signing policy](CODE_SIGNING.md) identifies the maintainer, approver and
artifact review requirements. SignPath Foundation is the proposed Windows
signing provider; its service is not active for BURST. Existing Windows downloads
must not be represented as SignPath-signed. Signing does not replace the
production qualification requirements.

## Installation and removal

Standalone daemon downloads are executables. Place the executable in a directory
you control and use `burst-daemon --help` for the commands supported by that
version. Copying the executable does not install a background service. Stop any
process you started before replacing or removing its executable.

Desktop MSI/NSIS packages, when released, use their Windows installer and
uninstaller. Use Windows Installed apps to remove an installed application.
Application removal and removal of wallet profiles or node ledgers are separate
operations; preserve recovery material before deleting data.

See [Privacy and network behavior](PRIVACY.md) before selecting remote nodes or
running a protocol node. Keep required license and third-party notice files with
redistributed artifacts.
