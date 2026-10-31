# Documentation baseline

These are the source documents indexed by the coverage record before the
canonical documentation rewrite. Their bytes match the source hashes in
`coverage.json`. The coverage record itself is retained unchanged.

Some evidence paths disappeared during module renaming. Their source files are
preserved here from the same repository commit, recorded in `archived-inputs.json`.
The coverage checker resolves historical evidence here first, then in the current
repository. These files preserve what the old claims referred to; they are not
current implementation guidance or proof that current binaries pass those tests.
Historical reproduction commands may use the old module names.

The active index is `../../coverage.json`. It covers the current Markdown pages
and implementation decisions. Changed sections receive no inherited sign-off.
Both indexes are checked, and unresolved historical requirements continue to
block the release check. Updating prose does not retire a protocol requirement.

The checker validates indexing and evidence references. It does not establish
that an evidence file proves its claim or that a past result still qualifies the
current implementation. Use the validation workflow for that assessment.

`preserved-documentation.json` records evidence documents retained before their
entry pages were shortened. These are separate from the original source baseline;
the historical coverage record and its statuses remain unchanged.
