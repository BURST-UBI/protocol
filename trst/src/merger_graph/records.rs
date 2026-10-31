//! Lossless graph rows. Edge positions preserve order and multiplicity; revision
//! history stays separate from the current revocation state.
use super::*;
use std::collections::BTreeMap;
mod delta;
mod removal;
mod retention;
mod revocation_undo;

#[cfg(test)]
mod tests;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphRecord {
    Node {
        hash: TxHash,
        total: u128,
    },
    Source {
        merge: TxHash,
        position: u64,
        source: MergeSource,
    },
    Contribution {
        merge: TxHash,
        origin: TxHash,
        amount: u128,
    },
    OriginEdge {
        origin: TxHash,
        position: u64,
        child: TxHash,
        occurrence: u64,
    },
    DownstreamEdge {
        merge: TxHash,
        position: u64,
        child: TxHash,
        occurrence: u64,
    },
    RevokedOrigin(TxHash),
    Revision {
        hash: TxHash,
        merge: TxHash,
        previous: TxHash,
        origin: TxHash,
        revoked: bool,
        remaining: u128,
    },
    CurrentRevision {
        merge: TxHash,
        revision: TxHash,
    },
    SourceRevisions(TxHash),
    SourceRevision {
        merge: TxHash,
        position: u64,
        index: u64,
        basis: TrstBasis,
    },
}

impl GraphRecord {
    pub const STORAGE_PREFIX: &'static str = "trst_graph/v1/";
    const MAX_BYTES: u64 = 1024;

    /// Storage prefix and durable ordering slot for a reverse edge.
    pub fn edge_group_position(&self) -> Option<(String, u64)> {
        match self {
            GraphRecord::OriginEdge {
                origin, position, ..
            } => Some((
                format!("{}3/{origin}/", GraphRecord::STORAGE_PREFIX),
                *position,
            )),
            GraphRecord::DownstreamEdge {
                merge, position, ..
            } => Some((
                format!("{}4/{merge}/", GraphRecord::STORAGE_PREFIX),
                *position,
            )),
            _ => None,
        }
    }

    /// Set a reverse edge slot without changing its storage key.
    ///
    /// Panics for records other than OriginEdge and DownstreamEdge.
    pub fn set_edge_position(&mut self, value: u64) {
        match self {
            GraphRecord::OriginEdge { position, .. } | GraphRecord::DownstreamEdge { position, .. } => {
                *position = value
            }
            _ => unreachable!("position belongs to a reverse edge"),
        }
    }

    fn key(&self) -> String {
        let suffix = match self {
            Self::Node { hash, .. } => format!("0/{hash}"),
            Self::Source {
                merge, position, ..
            } => format!("1/{merge}/{position:016x}"),
            Self::Contribution { merge, origin, .. } => format!("2/{merge}/{origin}"),
            Self::OriginEdge {
                origin,
                child,
                occurrence,
                ..
            } => format!("3/{origin}/{child}/{occurrence:016x}"),
            Self::DownstreamEdge {
                merge,
                child,
                occurrence,
                ..
            } => format!("4/{merge}/{child}/{occurrence:016x}"),
            Self::RevokedOrigin(origin) => format!("5/{origin}"),
            Self::Revision { hash, .. } => format!("6/{hash}"),
            Self::CurrentRevision { merge, .. } => format!("7/{merge}"),
            Self::SourceRevisions(merge) => format!("8/{merge}"),
            Self::SourceRevision {
                merge, position, ..
            } => format!("9/{merge}/{position:016x}"),
        };
        format!("{}{suffix}", Self::STORAGE_PREFIX)
    }

    pub fn storage_entry(&self) -> Result<(String, Vec<u8>), String> {
        let bytes = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(Self::MAX_BYTES)
            .serialize(&(1u8, self))
            .map_err(|error| error.to_string())?;
        Ok((self.key(), bytes))
    }

    /// Decode a bounded version-1 row and check its primary key. Edge positions
    /// live in the payload, not the key; ordering and occurrence checks happen
    /// during reconstruction. This does not validate graph relationships.
    pub fn from_storage_entry(key: &str, bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > Self::MAX_BYTES
            || !key.starts_with(Self::STORAGE_PREFIX)
            || key.len() > 511
        {
            return Err("invalid merger graph record bounds or namespace".into());
        }
        let (version, record): (u8, Self) = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(Self::MAX_BYTES)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|error| error.to_string())?;
        if version != 1 || record.key() != key {
            return Err("merger graph record version or key mismatch".into());
        }
        Ok(record)
    }
}

/// Reconstruct a graph from caller-supplied records. Nodes precede sources and
/// contributions; source-revision headers precede their entries. Both source
/// sequences require contiguous positions from zero. Reverse edges can arrive
/// out of order and have position gaps, but positions must be unique per parent
/// and each child's occurrences must count from zero in position order.
/// Any push error permanently invalidates the reader.
#[derive(Default)]
pub struct GraphRecordReader {
    graph: MergerGraph,
    origins: EdgeReader,
    downstream: EdgeReader,
    failed: bool,
}

#[derive(Default)]
struct EdgeReader(HashMap<TxHash, Box<PendingEdges>>);

// A pointer-sized table value avoids enlarging every hash slot for the common
// one-child case. Only parents with multiple positions allocate an ordered tree.
enum PendingEdges {
    Single { position: u64, child: TxHash, occurrence: u64 },
    Multiple(BTreeMap<u64, (TxHash, u64)>),
}

impl EdgeReader {
    fn push(
        &mut self,
        parent: TxHash,
        position: u64,
        child: TxHash,
        occurrence: u64,
    ) -> Result<(), String> {
        use std::collections::hash_map::Entry;
        match self.0.entry(parent) {
            Entry::Vacant(entry) => {
                entry.insert(Box::new(PendingEdges::Single { position, child, occurrence }));
            }
            Entry::Occupied(mut entry) => {
                let pending = entry.get_mut().as_mut();
                match pending {
                    PendingEdges::Single { position: old_position, child: old_child, occurrence: old_occurrence } => {
                        if *old_position == position {
                            return Err("duplicate merger graph edge position".into());
                        }
                        let mut edges = BTreeMap::new();
                        edges.insert(*old_position, (*old_child, *old_occurrence));
                        edges.insert(position, (child, occurrence));
                        *pending = PendingEdges::Multiple(edges);
                    }
                    PendingEdges::Multiple(edges) => {
                        if edges.insert(position, (child, occurrence)).is_some() {
                            return Err("duplicate merger graph edge position".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn finish(self) -> Result<SharedMap<TxHash, EdgeList>, String> {
        self.finish_with_poll(&|_| Ok(()))
    }

    fn finish_with_poll(self, poll: &dyn Fn(&'static str) -> Result<(), String>) -> Result<SharedMap<TxHash, EdgeList>, String> {
        self.0
            .into_iter()
            .map(|(parent, edges)| {
                poll("trst:graph-build-parent")?;
                let edges = match *edges {
                    PendingEdges::Single { child, occurrence, .. } => {
                        if occurrence != 0 {
                            return Err("nonsequential merger graph edge occurrence".into());
                        }
                        return Ok((parent, vec![child].into()));
                    }
                    PendingEdges::Multiple(edges) => edges,
                };
                let mut counts = HashMap::<TxHash, u64>::new();
                let mut children = EdgeList::default();
                for (child, occurrence) in edges.into_values() {
                    poll("trst:graph-build-edge")?;
                    let expected = counts.entry(child).or_default();
                    if *expected != occurrence {
                        return Err("nonsequential merger graph edge occurrence".into());
                    }
                    *expected += 1;
                    children.push(child);
                }
                Ok((parent, children))
            })
            .collect()
    }
}

impl GraphRecordReader {
    pub fn push(&mut self, record: GraphRecord) -> Result<(), String> {
        if self.failed {
            return Err("merger graph reader already failed".into());
        }
        let result = self.push_inner(record);
        self.failed |= result.is_err();
        result
    }

    fn push_inner(&mut self, record: GraphRecord) -> Result<(), String> {
        fn append<T>(values: &mut Vec<T>, position: u64, value: T) -> Result<(), String> {
            if u64::try_from(values.len()).ok() != Some(position) {
                return Err("nonsequential merger graph record".into());
            }
            values.push(value);
            Ok(())
        }
        let graph = &mut self.graph;
        match record {
            GraphRecord::Node { hash, total } => {
                if graph
                    .merge_nodes
                    .insert(
                        hash,
                        MergeNode {
                            merge_tx: hash,
                            source_origins: Vec::new(),
                            total_amount: total,
                            revoked_contribs: HashMap::new(),
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate merger graph node".into());
                }
            }
            GraphRecord::Source {
                merge,
                position,
                source,
            } => {
                append(
                    &mut graph
                        .merge_nodes
                        .get_mut(&merge)
                        .ok_or("missing merger graph node")?
                        .source_origins,
                    position,
                    source,
                )?;
            }
            GraphRecord::Contribution {
                merge,
                origin,
                amount,
            } => {
                if graph
                    .merge_nodes
                    .get_mut(&merge)
                    .ok_or("missing merger graph node")?
                    .revoked_contribs
                    .insert(origin, amount)
                    .is_some()
                {
                    return Err("duplicate merger graph contribution".into());
                }
            }
            GraphRecord::OriginEdge {
                origin,
                position,
                child,
                occurrence,
            } => {
                self.origins.push(origin, position, child, occurrence)?;
            }
            GraphRecord::DownstreamEdge {
                merge,
                position,
                child,
                occurrence,
            } => {
                self.downstream.push(merge, position, child, occurrence)?;
            }
            GraphRecord::RevokedOrigin(origin) => {
                if !graph.revoked_origins.insert(origin) {
                    return Err("duplicate revoked origin".into());
                }
            }
            GraphRecord::Revision {
                hash,
                merge,
                previous,
                origin,
                revoked,
                remaining,
            } => {
                if graph
                    .revisions
                    .insert(
                        hash,
                        RevocationRevision {
                            merge_tx: merge,
                            previous,
                            origin,
                            revoked,
                            remaining,
                        },
                    )
                    .is_some()
                {
                    return Err("duplicate merger graph revision".into());
                }
            }
            GraphRecord::CurrentRevision { merge, revision } => {
                if graph.current_revisions.insert(merge, revision).is_some() {
                    return Err("duplicate current merger graph revision".into());
                }
            }
            GraphRecord::SourceRevisions(merge) => {
                if graph.source_revisions.insert(merge, Vec::new()).is_some() {
                    return Err("duplicate merge source revision header".into());
                }
            }
            GraphRecord::SourceRevision {
                merge,
                position,
                index,
                basis,
            } => {
                let index = usize::try_from(index).map_err(|error| error.to_string())?;
                append(
                    graph
                        .source_revisions
                        .get_mut(&merge)
                        .ok_or("missing merge source revision header")?,
                    position,
                    (index, basis),
                )?;
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<MergerGraph, String> {
        self.finish_with_poll(&|_| Ok(()))
    }

    /// Assemble edges and validate graph structure and revision records. Errors,
    /// including cancellation, return no partial graph. Burn ownership, holding
    /// references and ledger authentication still require combined validation.
    pub fn finish_with_poll(mut self, poll: &dyn Fn(&'static str) -> Result<(), String>) -> Result<MergerGraph, String> {
        if self.failed {
            return Err("cannot publish failed merger graph reconstruction".into());
        }
        self.graph.origin_to_merges = self.origins.finish_with_poll(poll)?;
        self.graph.merge_to_downstream = self.downstream.finish_with_poll(poll)?;
        self.graph.validate_persisted_with_poll(poll)?;
        Ok(self.graph)
    }
}

impl MergerGraph {
    /// Sum record counts across nodes and indexes without encoding rows. Returns
    /// None on usize overflow; this scans containers rather than using one cache.
    pub fn record_count(&self) -> Option<usize> {
        [
            self.merge_nodes.len(),
            self.revoked_origins.len(),
            self.revisions.len(),
            self.current_revisions.len(),
            self.source_revisions.len(),
        ]
        .into_iter()
        .chain(
            self.merge_nodes
                .values()
                .map(|node| node.source_origins.len()),
        )
        .chain(
            self.merge_nodes
                .values()
                .map(|node| node.revoked_contribs.len()),
        )
        .chain(
            self.origin_to_merges
                .values()
                .map(|children| children.len()),
        )
        .chain(
            self.merge_to_downstream
                .values()
                .map(|children| children.len()),
        )
        .chain(self.source_revisions.values().map(|sources| sources.len()))
        .try_fold(0usize, usize::checked_add)
    }

    /// Emit records in deterministic groups, with headers before dependent rows.
    /// Sorting uses temporary reference vectors, and each edge group tracks child
    /// occurrence counts. No whole-graph encoded buffer is built. Edge positions
    /// are dense output offsets, not original durable slots. Callback errors stop
    /// emission without undoing prior effects; callers stage persistent writes.
    pub fn visit_records<E>(
        &self,
        mut visit: impl FnMut(GraphRecord) -> Result<(), E>,
    ) -> Result<(), E> {
        fn ordered<'a, T: 'a>(
            entries: impl Iterator<Item = (&'a TxHash, &'a T)>,
        ) -> Vec<(&'a TxHash, &'a T)> {
            let mut values: Vec<_> = entries.collect();
            values.sort_unstable_by_key(|(key, _)| *key.as_bytes());
            values
        }
        for (hash, node) in ordered(self.merge_nodes.iter()) {
            visit(GraphRecord::Node {
                hash: *hash,
                total: node.total_amount,
            })?;
            for (position, source) in node.source_origins.iter().enumerate() {
                visit(GraphRecord::Source {
                    merge: *hash,
                    position: position as u64,
                    source: source.clone(),
                })?;
            }
            for (origin, amount) in ordered(node.revoked_contribs.iter()) {
                visit(GraphRecord::Contribution {
                    merge: *hash,
                    origin: *origin,
                    amount: *amount,
                })?;
            }
        }
        for (origin, children) in ordered(self.origin_to_merges.iter()) {
            let mut occurrences = HashMap::<TxHash, u64>::new();
            for (position, child) in children.iter().enumerate() {
                let occurrence = occurrences.entry(*child).or_default();
                visit(GraphRecord::OriginEdge {
                    origin: *origin,
                    position: position as u64,
                    child: *child,
                    occurrence: *occurrence,
                })?;
                *occurrence += 1;
            }
        }
        for (merge, children) in ordered(self.merge_to_downstream.iter()) {
            let mut occurrences = HashMap::<TxHash, u64>::new();
            for (position, child) in children.iter().enumerate() {
                let occurrence = occurrences.entry(*child).or_default();
                visit(GraphRecord::DownstreamEdge {
                    merge: *merge,
                    position: position as u64,
                    child: *child,
                    occurrence: *occurrence,
                })?;
                *occurrence += 1;
            }
        }
        let mut revoked: Vec<_> = self.revoked_origins.iter().copied().collect();
        revoked.sort_unstable_by_key(|hash| *hash.as_bytes());
        for origin in revoked {
            visit(GraphRecord::RevokedOrigin(origin))?;
        }
        for (hash, revision) in ordered(self.revisions.iter()) {
            visit(GraphRecord::Revision {
                hash: *hash,
                merge: revision.merge_tx,
                previous: revision.previous,
                origin: revision.origin,
                revoked: revision.revoked,
                remaining: revision.remaining,
            })?;
        }
        for (merge, revision) in ordered(self.current_revisions.iter()) {
            visit(GraphRecord::CurrentRevision {
                merge: *merge,
                revision: *revision,
            })?;
        }
        for (merge, sources) in ordered(self.source_revisions.iter()) {
            visit(GraphRecord::SourceRevisions(*merge))?;
            for (position, (index, basis)) in sources.iter().enumerate() {
                visit(GraphRecord::SourceRevision {
                    merge: *merge,
                    position: position as u64,
                    index: *index as u64,
                    basis: *basis,
                })?;
            }
        }
        Ok(())
    }

    /// Reconstruct and structurally validate records from one caller-owned
    /// consistent view. This helper does not open or pin a storage transaction.
    pub fn from_records(
        records: impl IntoIterator<Item = Result<GraphRecord, String>>,
    ) -> Result<Self, String> {
        let mut reader = GraphRecordReader::default();
        for record in records {
            reader.push(record?)?;
        }
        reader.finish()
    }
}
