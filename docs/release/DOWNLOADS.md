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

For signing status and release approval responsibilities, see
[code signing policy](CODE_SIGNING.md).

## Installation and removal

Standalone daemon downloads are executables. Place the executable in a directory
you control and use `burst-daemon --help` for the commands supported by that
version. Copying the executable does not install a background service. Stop any
process you started before replacing or removing its executable.

For background services, see [daemon service installation](../node/INSTALLATION.md).

Desktop MSI/NSIS packages, when released, use their Windows installer and
uninstaller. Use Windows Installed apps to remove an installed application.
Application removal and removal of wallet profiles or node ledgers are separate
operations; preserve recovery material before deleting data.

See [Privacy and network behavior](PRIVACY.md) before selecting remote nodes or
running a protocol node. Keep required license and third-party notice files with
redistributed artifacts.

## Check the artifact

Keep the release manifest, checksums, license and third-party notices with the
artifact. Compare the downloaded file against the manifest. Check the signature
and notarization status stated for that exact file; do not infer it from another
platform or an earlier build.

Desktop packages and the standalone daemon have different dependencies. A static
Linux daemon does not establish that a desktop package runs on the same machine.
Follow the release's operating-system, architecture, glibc and WebView/runtime
requirements. An installer tested with a runtime already installed does not prove
that its first-install download path works.

## Read qualification claims narrowly

Check what a release was actually tested against. A successful installation on
one platform does not establish the same result on another. A test-network run
qualifies its recorded network and inputs, while a measured workload establishes
only its tested throughput, latency and memory use.

Recovery evidence also has a scope. Surviving a process crash does not establish
how physical storage behaves during a power loss. A signed or notarized package
identifies its publisher and protects the package bytes; it does not establish
that the protocol rules are correct or that every safety review has passed.

Physical body retirement remains gated in the current node. Do not enable it by
bypassing that gate. Performance targets and unresolved safety requirements must
be distinguished in the release review; neither becomes a passing result through
wording in a guide.

Maintainers record artifact-specific results and unresolved gates in the
[release readiness record](https://github.com/BURST-UBI/protocol/blob/main/spec/reviews/LIMITED_RELEASE_READINESS.md).
Those results apply only to their recorded inputs and assertions.

## Before installation or upgrade

Preserve wallet recovery material, encrypted profiles and pending-action journals.
A recovery phrase restores keys; it does not resolve an uncertain signed action.
See [wallet recovery](../wallet/WALLETS.md#what-a-phrase-does-not-restore).

Stop the application or node cleanly before replacing its files or taking a
filesystem copy of its database. Keep the original data and previous executable
until the replacement has been checked. An older binary may not understand a
newer database; downgrading the executable is not a recovery procedure by itself.

After restart, check the expected network identity, synchronization,
representative status and local errors. Resolve saved signed actions through the
wallet's recovery flow before preparing replacements.
