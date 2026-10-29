# Code signing policy

BURST distributes a protocol node, desktop wallet and node dashboard. A code
signature identifies the publisher and protects the signed bytes against
modification. It does not establish protocol safety or production readiness.

## Windows signing status

SignPath Foundation is the proposed provider for Windows code signing.
Foundation approval and the Windows signing integration are not active. Do not
treat existing Windows downloads as SignPath-signed or assume Foundation
sponsorship. If accepted, the certificate will identify SignPath Foundation.
The required provider attribution will be added when that service is active.

## Responsibilities and release approval

The project owner, [Nitesh Gautam (eliesh0)](https://github.com/eliesh0), is the
maintainer, reviewer and designated signing approver. Contributions from other
people require maintainer review before they enter a release. This includes
build scripts, workflows, dependency changes and signing configuration.

Each release signing request requires explicit approval by the designated
approver after reviewing its source revision, build provenance, artifact
identity and qualification results. An automated build succeeding does not
approve a public release. GitHub and signing-service accounts used for this
work must use multi-factor authentication.

Windows signing must use an integration accepted by the signing provider that
binds the artifacts to the reviewed source and build. Product names and versions
must match the approved release metadata. Arbitrary uploaded executables and
unreviewed replacements do not qualify. Third-party binaries retain their own
publisher identities; they are not signed as BURST code.

A release must preserve required third-party copyright and license notices.
Signing credentials must stay outside source archives, build caches and release
evidence. Access is restricted to the signing operation and its designated
approver.

## Downloads and privacy

See [Downloads](DOWNLOADS.md) for available artifacts and their scope, and
[Privacy and network behavior](PRIVACY.md) for information sent to nodes, peers
and infrastructure services. Report suspected release tampering through a
[repository issue](https://github.com/BURST-UBI/protocol/issues), without posting
wallet secrets or private signing credentials.
