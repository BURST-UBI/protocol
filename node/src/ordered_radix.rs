//! Storage-independent construction of an immutable tree from sorted 256-bit keys.
//! The sink owns byte encoding and persistence; a tree root confers no authority.
use burst_store::StoreError;
use std::marker::PhantomData;

type Key = [u8; 32];

pub(crate) enum Node<V, L> {
    Leaf { key: Key, value: V },
    Branch { bit: u16, left: L, right: L },
}

/// Unfinished right edge, with at most 256 branches. Storage owners must keep
/// every referenced node available until construction finishes.
#[derive(Clone)]
pub(crate) struct Checkpoint<L> {
    pub previous: Option<Key>,
    pub pending: Option<L>,
    pub branches: Vec<(u16, L)>,
    pub count: usize,
}

pub(crate) struct Builder<V, L> {
    previous: Option<Key>,
    pending: Option<L>,
    branches: Vec<(u16, L)>,
    count: usize,
    failed: bool,
    value: PhantomData<fn(V)>,
}

fn invalid(message: &str) -> StoreError { StoreError::Corruption(message.into()) }

impl<V, L> Builder<V, L> {
    pub(crate) fn new() -> Self {
        Self { previous: None, pending: None, branches: Vec::new(), count: 0,
            failed: false, value: PhantomData }
    }

    pub(crate) fn checkpoint(&self) -> Result<Checkpoint<L>, StoreError> where L: Clone {
        if self.failed { return Err(invalid("failed radix builder cannot checkpoint")); }
        Ok(Checkpoint { previous: self.previous, pending: self.pending.clone(),
            branches: self.branches.clone(), count: self.count })
    }

    /// Check continuation structure, not the contents of referenced sink nodes.
    pub(crate) fn resume(state: Checkpoint<L>) -> Result<Self, StoreError> {
        let empty = state.count == 0;
        if state.previous.is_none() != empty || state.pending.is_none() != empty
            || state.branches.len() > 256 || (empty && !state.branches.is_empty())
            || (!empty && state.branches.len() >= state.count)
            || state.branches.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(invalid("invalid radix continuation"));
        }
        if let Some(previous) = state.previous {
            for (bit, _) in &state.branches {
                if *bit >= 256 || previous[*bit as usize / 8] & (0x80 >> (*bit % 8)) == 0 {
                    return Err(invalid("invalid radix continuation right edge"));
                }
            }
        }
        Ok(Self { previous: state.previous, pending: state.pending, branches: state.branches,
            count: state.count, failed: false, value: PhantomData })
    }

    /// Emit each leaf and branch once, keeping only the unfinished right edge.
    /// An error prevents further pushes, checkpoints and successful completion.
    /// Nodes already emitted remain the sink owner's responsibility.
    pub(crate) fn push(&mut self, key: Key, value: V,
        mut append: impl FnMut(Node<V, L>) -> Result<L, StoreError>,
    ) -> Result<(), StoreError> {
        if self.failed { return Err(invalid("ordered radix builder already failed")); }
        self.failed = true;
        if let Some(old) = self.previous {
            if key <= old { return Err(invalid("bulk radix keys are not strictly ordered")); }
            let bit = key.iter().zip(old).enumerate().find_map(|(index, (a, b))| {
                let difference = a ^ b;
                (difference != 0).then(|| (index * 8 + difference.leading_zeros() as usize) as u16)
            }).ok_or_else(|| invalid("missing bulk radix split"))?;
            let mut left = self.pending.take().ok_or_else(|| invalid("missing previous radix leaf"))?;
            while self.branches.last().is_some_and(|(parent, _)| *parent > bit) {
                let (parent, sibling) = self.branches.pop().expect("checked bulk branch");
                left = append(Node::Branch { bit: parent, left: sibling, right: left })?;
            }
            if self.branches.last().is_some_and(|(parent, _)| *parent >= bit) {
                return Err(invalid("inconsistent bulk radix split"));
            }
            self.branches.push((bit, left));
        }
        self.pending = Some(append(Node::Leaf { key, value })?);
        self.previous = Some(key);
        self.count = self.count.checked_add(1).ok_or_else(|| invalid("bulk radix count overflow"))?;
        self.failed = false;
        Ok(())
    }

    pub(crate) fn finish(mut self,
        mut append: impl FnMut(Node<V, L>) -> Result<L, StoreError>,
    ) -> Result<(Option<L>, usize), StoreError> {
        if self.failed { return Err(invalid("ordered radix builder already failed")); }
        if let Some(mut root) = self.pending {
            while let Some((bit, left)) = self.branches.pop() {
                root = append(Node::Branch { bit, left, right: root })?;
            }
            Ok((Some(root), self.count))
        } else { Ok((None, self.count)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_preserve_emissions_at_every_right_edge_depth() {
        // Successive keys fill the right edge from bit zero through bit 255.
        let mut keys = vec![[0; 32]];
        let mut key = [0; 32];
        for bit in 0..256 {
            key[bit / 8] |= 0x80 >> (bit % 8);
            keys.push(key);
        }
        let build = |resume: bool| {
            let mut emitted = Vec::new();
            let mut builder = Builder::<(), usize>::new();
            let mut append = |node| {
                emitted.push(match node {
                    Node::Leaf { key, .. } => (key, 256, 0, 0),
                    Node::Branch { bit, left, right } => ([0; 32], bit, left, right),
                });
                Ok(emitted.len() - 1)
            };
            for (index, key) in keys.iter().enumerate() {
                builder.push(*key, (), &mut append).unwrap();
                let state = builder.checkpoint().unwrap();
                assert_eq!(state.branches.len(), index);
                if resume { builder = Builder::resume(state).unwrap(); }
            }
            let result = builder.finish(&mut append).unwrap();
            (result, emitted)
        };
        let original = build(false);
        assert_eq!(original.1.len(), 513);
        assert_eq!(build(true), original);
    }

    #[test]
    fn resume_rejects_inconsistent_frontiers() {
        let valid = Checkpoint { previous: Some([255; 32]), pending: Some(2usize),
            branches: vec![(0, 0), (255, 1)], count: 3 };
        assert!(Builder::<(), _>::resume(valid.clone()).is_ok());
        for mode in 0..7 {
            let mut state = valid.clone();
            match mode {
                0 => state.previous = None,
                1 => state.pending = None,
                2 => state.count = 0,
                3 => state.count = 2,
                4 => state.branches[1].0 = 0,
                5 => state.branches[1].0 = 256,
                _ => state.previous = Some([0; 32]),
            }
            assert!(Builder::<(), _>::resume(state).is_err());
        }
        let empty = Builder::<(), usize>::new().checkpoint().unwrap();
        assert_eq!(Builder::resume(empty).unwrap().finish(|_: Node<(), usize>| unreachable!()).unwrap(), (None, 0));
    }

    #[test]
    fn sink_failure_cannot_publish_a_partial_root() {
        let mut builder = Builder::<(), usize>::new();
        builder.push([0; 32], (), |_| Ok(0)).unwrap();
        assert!(builder.push([1; 32], (), |_| Err(invalid("sink failed"))).is_err());
        assert!(builder.checkpoint().is_err());
        assert!(builder.push([2; 32], (), |_| panic!("poisoned builder must not write")).is_err());
        assert!(builder.finish(|_| panic!("poisoned builder must not publish")).is_err());
    }

    #[test]
    fn branch_failure_and_duplicate_key_cannot_publish() {
        let mut builder = Builder::<(), usize>::new();
        builder.push([0; 32], (), |_| Ok(0)).unwrap();
        builder.push([1; 32], (), |_| Ok(1)).unwrap();
        assert!(builder.finish(|_| Err(invalid("branch sink failed"))).is_err());
        let mut builder = Builder::<(), usize>::new();
        builder.push([0; 32], (), |_| Ok(0)).unwrap();
        assert!(builder.push([0; 32], (), |_| panic!("duplicate must not write")).is_err());
        assert!(builder.finish(|_| panic!("poisoned builder must not publish")).is_err());
    }
}
