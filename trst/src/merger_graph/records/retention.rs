use super::GraphRecord;
use burst_types::{TrstReference, TxHash};
use std::ops::ControlFlow;

impl GraphRecord {
    /// Stream identities and edges without assuming that only current revision
    /// heads matter. Callers must authenticate the cut and resolve each namespace.
    pub fn visit_retention_references<'a, B>(
        &'a self, mut visit: impl FnMut(TrstReference<'a>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        use TrstReference::{Graph, Revision};
        match self {
            Self::Node { hash, .. } => visit(Graph(*hash))?,
            Self::Source { merge, source, .. } => {
                visit(Graph(*merge))?;
                visit(Graph(source.origin))?;
            }
            Self::Contribution { merge, origin, .. } => {
                visit(Graph(*merge))?;
                visit(Graph(*origin))?;
            }
            Self::OriginEdge { origin, child, .. } => {
                visit(Graph(*origin))?;
                visit(Graph(*child))?;
            }
            Self::DownstreamEdge { merge, child, .. } => {
                visit(Graph(*merge))?;
                visit(Graph(*child))?;
            }
            Self::RevokedOrigin(origin) => visit(Graph(*origin))?,
            Self::Revision { hash, merge, previous, origin, .. } => {
                visit(Revision(*hash))?;
                visit(Graph(*merge))?;
                if *previous != TxHash::ZERO { visit(Revision(*previous))?; }
                visit(Graph(*origin))?;
            }
            Self::CurrentRevision { merge, revision } => {
                visit(Graph(*merge))?;
                visit(Revision(*revision))?;
            }
            Self::SourceRevisions(merge) => visit(Graph(*merge))?,
            Self::SourceRevision { merge, basis, .. } => {
                visit(Graph(*merge))?;
                basis.visit_retention_references(&mut visit)?;
            }
        }
        ControlFlow::Continue(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::TrstBasis;
    use crate::MergeSource;

    #[test]
    fn retention_references_cover_every_record_and_stop_at_each_boundary() {
        use GraphRecord as R;
        use TrstReference::{Graph as G, Revision as V};
        let h = |n| TxHash::new([n; 32]);
        let cases = [
            (R::Node { hash: h(1), total: 10 }, vec![G(h(1))]),
            (R::Source { merge: h(1), position: 0, source: MergeSource { origin: h(2), amount: 10 } }, vec![G(h(1)), G(h(2))]),
            (R::Contribution { merge: h(1), origin: h(2), amount: 10 }, vec![G(h(1)), G(h(2))]),
            (R::OriginEdge { origin: h(2), position: 0, child: h(1), occurrence: 0 }, vec![G(h(2)), G(h(1))]),
            (R::DownstreamEdge { merge: h(1), position: 0, child: h(2), occurrence: 0 }, vec![G(h(1)), G(h(2))]),
            (R::RevokedOrigin(h(2)), vec![G(h(2))]),
            (R::Revision { hash: h(3), merge: h(1), previous: h(4), origin: h(2), revoked: false, remaining: 10 }, vec![V(h(3)), G(h(1)), V(h(4)), G(h(2))]),
            (R::Revision { hash: h(3), merge: h(1), previous: TxHash::ZERO, origin: h(2), revoked: true, remaining: 0 }, vec![V(h(3)), G(h(1)), G(h(2))]),
            (R::CurrentRevision { merge: h(1), revision: h(3) }, vec![G(h(1)), V(h(3))]),
            (R::SourceRevisions(h(1)), vec![G(h(1))]),
            (R::SourceRevision { merge: h(1), position: 0, index: 0, basis: TrstBasis::Remainder(h(4)) }, vec![G(h(1)), V(h(4))]),
            (R::SourceRevision { merge: h(1), position: 0, index: 0, basis: TrstBasis::Single(h(2)) }, vec![G(h(1)), G(h(2))]),
        ];
        for (record, expected) in cases {
            let (key, bytes) = record.storage_entry().unwrap();
            let record = R::from_storage_entry(&key, &bytes).unwrap();
            for limit in 0..=expected.len() {
                let mut seen = Vec::new();
                let result = record.visit_retention_references(|reference| {
                    if seen.len() == limit { return ControlFlow::Break("budget"); }
                    seen.push(reference);
                    ControlFlow::Continue(())
                });
                assert_eq!(seen, expected[..limit]);
                assert_eq!(result.is_break(), limit < expected.len());
            }
        }
    }
}
