use super::*;
use crate::comparison::{maps, ordered, Poll};

impl MergerGraph {
    /// Compare stored graph contents, including edge order and multiplicity.
    /// Shared map storage skips value comparison; otherwise polls allow cancellation
    /// within edge and contribution lists. Equality does not establish validity.
    pub fn snapshot_eq_with_poll(&self, other: &Self, poll: Poll<'_>) -> Result<bool, String> {
        let Self {
            origin_to_merges,
            merge_nodes,
            merge_to_downstream,
            revoked_origins,
            revisions,
            current_revisions,
            source_revisions,
        } = self;
        for (left, right) in [
            (origin_to_merges, &other.origin_to_merges),
            (merge_to_downstream, &other.merge_to_downstream),
        ] {
            if !maps(left, right, poll, "trst:comparison-graph-parent", |a, b| {
                ordered(a.iter(), b.iter(), poll, "trst:comparison-graph-edge")
            })? {
                return Ok(false);
            }
        }
        if !maps(
            merge_nodes,
            &other.merge_nodes,
            poll,
            "trst:comparison-graph-node",
            |a, b| {
                let MergeNode {
                    merge_tx,
                    source_origins,
                    total_amount,
                    revoked_contribs,
                } = a;
                if *merge_tx != b.merge_tx
                    || *total_amount != b.total_amount
                    || revoked_contribs.len() != b.revoked_contribs.len()
                {
                    return Ok(false);
                }
                if !ordered(
                    source_origins.iter(),
                    b.source_origins.iter(),
                    poll,
                    "trst:comparison-graph-source",
                )? {
                    return Ok(false);
                }
                for (origin, amount) in revoked_contribs {
                    poll("trst:comparison-graph-contribution")?;
                    if b.revoked_contribs.get(origin) != Some(amount) {
                        return Ok(false);
                    }
                }
                Ok(true)
            },
        )? {
            return Ok(false);
        }
        if !ordered(
            revoked_origins.iter(),
            other.revoked_origins.iter(),
            poll,
            "trst:comparison-graph-revoked",
        )? {
            return Ok(false);
        }
        if !maps(
            revisions,
            &other.revisions,
            poll,
            "trst:comparison-graph-revision",
            |a, b| Ok(a == b),
        )? {
            return Ok(false);
        }
        if !maps(
            current_revisions,
            &other.current_revisions,
            poll,
            "trst:comparison-graph-current",
            |a, b| Ok(a == b),
        )? {
            return Ok(false);
        }
        maps(
            source_revisions,
            &other.source_revisions,
            poll,
            "trst:comparison-graph-bases",
            |a, b| ordered(a.iter(), b.iter(), poll, "trst:comparison-graph-basis"),
        )
    }
}
