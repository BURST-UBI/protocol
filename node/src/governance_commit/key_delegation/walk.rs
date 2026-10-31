use std::{collections::HashMap, hash::Hash};

/// A fixed-cut hop either advances or terminates at its first effective ballot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Step<K, V> {
    Next(K),
    Stop(Option<V>),
}

// Most cached stops and short paths need only their base hop. Keep it inline;
// longer prefixes retain the same indexed slots and budget semantics.
enum PrefixSlots<K, V> {
    One(Option<Step<K, V>>),
    Many(Vec<Option<Step<K, V>>>),
}

impl<K, V> Default for PrefixSlots<K, V> {
    fn default() -> Self {
        Self::One(None)
    }
}

impl<K, V> PrefixSlots<K, V> {
    fn as_slice(&self) -> &[Option<Step<K, V>>] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }

    fn set(&mut self, level: usize, value: Step<K, V>) {
        if let Self::One(first) = self {
            if level == 0 {
                *first = Some(value);
                return;
            }
            let mut values = Vec::with_capacity(level + 1);
            values.push(first.take());
            values.resize_with(level + 1, || None);
            *self = Self::Many(values);
        }
        if let Self::Many(values) = self {
            values.resize_with(values.len().max(level + 1), || None);
            values[level] = Some(value);
        }
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        match self {
            Self::One(_) => 0,
            Self::Many(values) => values.capacity(),
        }
    }
}

impl<K, V> std::ops::Deref for PrefixSlots<K, V> {
    type Target = [Option<Step<K, V>>];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[cfg(test)]
impl<'a, K, V> IntoIterator for &'a PrefixSlots<K, V> {
    type Item = &'a Option<Step<K, V>>;
    type IntoIter = std::slice::Iter<'a, Option<Step<K, V>>>;
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Lazily composed power-of-two prefixes. Loading a prefix never reads beyond
/// its requested budget. Each reached node has at most floor(log2(D))+1 slots,
/// where D is the largest positive budget used with this cache.
/// Cycles need no eager scan: their already loaded hops compose into finite
/// prefixes and a budget-exhausted query returns no vote.
pub(super) struct Walk<K, V> {
    prefixes: HashMap<K, PrefixSlots<K, V>>,
    #[cfg(test)]
    stages: Option<Stages>,
}

#[cfg(test)]
#[derive(Default, serde::Serialize)]
struct Stages {
    memo_read_ns: u128,
    memo_write_ns: u128,
    load_ns: u128,
    reads: usize,
    hits: usize,
    writes: usize,
    loads: usize,
}

impl<K: Clone + Eq + Hash, V: Clone> Walk<K, V> {
    pub(super) fn new() -> Self {
        Self {
            prefixes: HashMap::new(),
            #[cfg(test)]
            stages: None,
        }
    }

    fn cached_prefix(&self, owner: &K, level: usize) -> Option<(usize, Step<K, V>)> {
        let slots = self.prefixes.get(owner)?;
        if let Some(Some(known)) = slots.get(level) {
            return Some((level, known.clone()));
        }
        let last = slots.len().checked_sub(1)?;
        slots.last()?.as_ref().map(|known| (last, known.clone()))
    }

    fn retain(&mut self, owner: &K, level: usize, result: &Step<K, V>) {
        #[cfg(test)]
        let start = self.stages.as_ref().map(|_| std::time::Instant::now());
        self.prefixes
            .entry(owner.clone())
            .or_default()
            .set(level, result.clone());
        #[cfg(test)]
        if let Some(stages) = &mut self.stages {
            stages.memo_write_ns += start.unwrap().elapsed().as_nanos();
            stages.writes += 1;
        }
    }

    fn prefix<E>(
        &mut self,
        owner: &K,
        level: usize,
        retain_stop: bool,
        load: &mut impl FnMut(&K) -> Result<Step<K, V>, E>,
    ) -> Result<Step<K, V>, E> {
        #[cfg(test)]
        let start = self.stages.as_ref().map(|_| std::time::Instant::now());
        let known = self.cached_prefix(owner, level);
        #[cfg(test)]
        if let Some(stages) = &mut self.stages {
            stages.memo_read_ns += start.unwrap().elapsed().as_nanos();
            stages.reads += 1;
            stages.hits += usize::from(known.is_some());
        }
        let (mut completed, mut result) = if let Some(known) = known {
            known
        } else {
            #[cfg(test)]
            let start = self.stages.as_ref().map(|_| std::time::Instant::now());
            let loaded = load(owner);
            #[cfg(test)]
            if let Some(stages) = &mut self.stages {
                stages.load_ns += start.unwrap().elapsed().as_nanos();
                stages.loads += 1;
            }
            let result = loaded?;
            if !retain_stop && matches!(&result, Step::Stop(_)) {
                return Ok(result);
            }
            self.retain(owner, 0, &result);
            (0, result)
        };
        // Extend known prefixes upward. A short stopped path need not descend
        // through every requested level before discovering its first ballot.
        while completed < level {
            let Step::Next(next) = result else {
                return Ok(result);
            };
            result = self.prefix(&next, completed, true, load)?;
            completed += 1;
            self.retain(owner, completed, &result);
        }
        Ok(result)
    }

    /// The callback and all eligibility/ballot inputs must stay fixed for this
    /// cache's lifetime. Loader errors propagate without being cached; prefixes
    /// built before an error remain. A zero budget returns no vote without loading.
    pub(super) fn resolve<E>(
        &mut self,
        owner: &K,
        mut budget: usize,
        mut load: impl FnMut(&K) -> Result<Step<K, V>, E>,
    ) -> Result<Option<V>, E> {
        let mut current = owner.clone();
        if budget != 0 {
            // Discover immediate termination without building a tower of equal
            // stopped prefixes. Standalone stops are not retained; when reached
            // through another owner's prefix they are cached normally. Across Q
            // queries and V reached nodes, base loads are bounded by Q + V.
            if let Step::Stop(vote) = self.prefix(&current, 0, false, &mut load)? {
                return Ok(vote);
            }
        }
        while budget != 0 {
            let level = (usize::BITS - 1 - budget.leading_zeros()) as usize;
            match self.prefix(&current, level, true, &mut load)? {
                Step::Next(next) => current = next,
                Step::Stop(vote) => return Ok(vote),
            }
            budget -= 1usize << level;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn reference(edges: &[Step<usize, u8>], owner: usize, depth: usize) -> Option<u8> {
        let mut seen = HashSet::new();
        let mut current = owner;
        for _ in 0..depth {
            if !seen.insert(current) {
                return None;
            }
            match edges[current] {
                Step::Next(next) => current = next,
                Step::Stop(vote) => return vote,
            }
        }
        None
    }

    #[test]
    fn inline_prefix_promotes_without_changing_short_budget_answers() {
        let mut walk = Walk::<usize, u8>::new();
        let mut load = |node: &usize| {
            Ok::<_, ()>(if *node == 1 {
                Step::Stop(Some(7))
            } else {
                Step::Next(node + 1)
            })
        };
        assert_eq!(walk.resolve(&0, 1, &mut load), Ok(None));
        assert_eq!(walk.prefixes[&0].len(), 1);
        assert_eq!(walk.prefixes[&0].capacity(), 0);
        assert_eq!(walk.resolve(&0, 128, &mut load), Ok(Some(7)));
        assert_eq!(walk.prefixes[&0].len(), 2);
        assert_eq!(walk.prefixes[&0].capacity(), 2);
        assert_eq!(walk.prefixes[&1].len(), 1);
        assert_eq!(walk.prefixes[&1].capacity(), 0);
        assert_eq!(walk.resolve(&0, 1, &mut load), Ok(None));
        assert_eq!(walk.resolve(&0, 2, &mut load), Ok(Some(7)));
        assert_eq!(walk.resolve(&1, 0, &mut load), Ok(None));
    }

    #[test]
    fn sparse_prefix_reads_do_not_grow_with_unused_depth() {
        for depth in [2, 10, 128, usize::MAX] {
            let mut walk = Walk::<usize, u8>::new();
            walk.stages = Some(Stages::default());
            for owner in 0..100 {
                assert_eq!(
                    walk.resolve(&owner, depth, |node| Ok::<_, ()>(if node % 10 == 0 {
                        Step::Next(node + 1)
                    } else {
                        Step::Stop(None)
                    })),
                    Ok(None)
                );
            }
            let stages = walk.stages.unwrap();
            assert_eq!(stages.loads, 100);
            assert_eq!(stages.reads, 120);
            assert_eq!(stages.writes, 30);
        }
    }

    #[test]
    fn all_small_graphs_match_first_ballot_with_mixed_budgets_and_orders() {
        // Four possible edges, stop without vote, or either of two votes.
        for encoded in 0..7usize.pow(4) {
            let mut code = encoded;
            let edges: Vec<_> = (0..4)
                .map(|_| {
                    let choice = code % 7;
                    code /= 7;
                    match choice {
                        0..=3 => Step::Next(choice),
                        4 => Step::Stop(None),
                        n => Step::Stop(Some(n as u8)),
                    }
                })
                .collect();
            for reverse in [false, true] {
                let mut walk = Walk::new();
                let mut loads = [0; 4];
                for depth in [0, 1, 2, 5, 3, 8, 1] {
                    for n in 0..4 {
                        let owner = if reverse { 3 - n } else { n };
                        let actual = walk
                            .resolve(&owner, depth, |node| {
                                loads[*node] += 1;
                                Ok::<_, ()>(edges[*node].clone())
                            })
                            .unwrap();
                        assert_eq!(
                            actual,
                            reference(&edges, owner, depth),
                            "graph {encoded}, owner {owner}, depth {depth}"
                        );
                    }
                }
                for (node, count) in loads.into_iter().enumerate() {
                    match edges[node] {
                        Step::Next(_) => assert!(count <= 1),
                        // Six positive-budget owner queries plus at most one
                        // discovery through an incoming path, then cached.
                        Step::Stop(_) => assert!(count <= 7),
                    }
                }
                assert!(walk.prefixes.values().all(|slots| slots.len() <= 4));
            }
        }
    }

    #[test]
    fn prefix_does_not_read_errors_beyond_budget_or_after_a_vote() {
        let mut walk = Walk::new();
        let mut visited = Vec::new();
        let mut load = |node: &usize| {
            visited.push(*node);
            if *node == 2 {
                Err("malformed")
            } else {
                Ok(Step::<_, u8>::Next(node + 1))
            }
        };
        assert_eq!(walk.resolve(&0, 2, &mut load), Ok(None));
        assert_eq!(walk.resolve(&0, 3, &mut load), Err("malformed"));
        assert_eq!(visited, vec![0, 1, 2]);
        let mut walk = Walk::new();
        assert_eq!(
            walk.resolve(&0, usize::MAX, |node| {
                if *node == 0 {
                    Ok(Step::Next(1))
                } else if *node == 1 {
                    Ok(Step::Stop(Some(7u8)))
                } else {
                    Err("unreachable")
                }
            }),
            Ok(Some(7))
        );
    }

    #[test]
    fn no_vote_cycles_have_logarithmic_storage_even_at_maximum_budget() {
        let mut walk = Walk::<usize, u8>::new();
        let mut loads = 0;
        for owner in 0..32 {
            assert_eq!(
                walk.resolve(&owner, usize::MAX, |node| {
                    loads += 1;
                    Ok::<_, ()>(Step::Next((node + 1) % 32))
                })
                .unwrap(),
                None
            );
        }
        assert_eq!(loads, 32);
        assert!(
            walk.prefixes
                .values()
                .map(|slots| slots.len())
                .sum::<usize>()
                <= 32 * usize::BITS as usize
        );
    }

    #[test]
    fn completed_prefixes_are_not_promoted_and_short_budgets_still_apply() {
        let mut walk = Walk::new();
        let mut loads = 0;
        for depth in [usize::MAX, 1, 128, 2] {
            assert_eq!(
                walk.resolve(&0usize, depth, |_| {
                    loads += 1;
                    Ok::<_, ()>(Step::Stop(Some(9u8)))
                })
                .unwrap(),
                Some(9)
            );
        }
        assert_eq!(loads, 4); // One cheap stop evaluation per explicit query.
        assert!(walk.prefixes.is_empty());

        let mut walk = Walk::new();
        let mut load = |node: &usize| {
            Ok::<_, ()>(if *node == 2 {
                Step::Stop(Some(7u8))
            } else {
                Step::Next(node + 1)
            })
        };
        assert_eq!(walk.resolve(&0, 128, &mut load).unwrap(), Some(7));
        assert_eq!(walk.prefixes[&0].len(), 3); // Four-hop prefix; no promotion to the requested depth.
        assert_eq!(walk.resolve(&0, 2, &mut load).unwrap(), None);
        assert_eq!(walk.resolve(&0, 3, &mut load).unwrap(), Some(7));
    }

    #[test]
    fn terminal_first_then_incoming_path_retains_shared_result() {
        let mut walk = Walk::new();
        let mut loads = [0usize; 4];
        let mut load = |node: &usize| {
            loads[*node] += 1;
            Ok::<_, ()>(match node {
                0 => Step::Next(1),
                1 => Step::Next(2),
                2 => Step::Stop(Some(7u8)),
                _ => Step::Stop(None),
            })
        };
        assert_eq!(walk.resolve(&2, 10, &mut load).unwrap(), Some(7));
        assert!(walk.prefixes.is_empty());
        assert_eq!(walk.resolve(&0, 10, &mut load).unwrap(), Some(7));
        for owner in [2, 0, 1, 2] {
            assert_eq!(walk.resolve(&owner, 10, &mut load).unwrap(), Some(7));
        }
        assert_eq!(walk.resolve(&3, 10, &mut load).unwrap(), None);
        assert_eq!(walk.resolve(&3, 10, &mut load).unwrap(), None);
        assert_eq!(loads, [1, 1, 2, 2]);
        assert!(!walk.prefixes.contains_key(&3));
    }

    #[test]
    #[ignore = "explicit capped-host prefix traversal diagnostic"]
    fn prefix_traversal_scaling() {
        use burst_types::WalletAddress;
        use std::time::Instant;
        let population: usize = std::env::var("BURST_GOV_PROBE_POPULATION")
            .unwrap()
            .parse()
            .unwrap();
        let depth: usize = std::env::var("BURST_GOV_PROBE_DEPTH")
            .unwrap()
            .parse()
            .unwrap();
        let shape = std::env::var("BURST_GOV_PROBE_SHAPE").unwrap();
        assert!((100..=100_000).contains(&population));
        assert!([10, 128].contains(&depth));
        assert!(
            ["stopped", "sparse", "shared_vote", "long_chain", "cycle"].contains(&shape.as_str())
        );
        let setup = Instant::now();
        let people: Vec<_> = (0..population)
            .map(|n| WalletAddress::new(format!("brst_{n:060}")))
            .collect();
        // Synthetic, already-authenticated hops. This isolates traversal; it
        // excludes scope restoration, signatures, grant IDs and persistent writes.
        let edges: HashMap<_, _> = people
            .iter()
            .enumerate()
            .map(|(n, owner)| {
                let next = match shape.as_str() {
                    "sparse" if n % 10 == 0 && n + 1 < population => Some(n + 1),
                    "shared_vote" if n + 1 < population => Some(if n < population - 9 {
                        population - 9
                    } else {
                        n + 1
                    }),
                    "long_chain" if n + 1 < population => Some(n + 1),
                    "cycle" => Some((n + 1) % population),
                    _ => None,
                };
                let step = next.map_or_else(
                    || Step::Stop((shape == "shared_vote").then_some(1u8)),
                    |index| Step::Next(people[index].clone()),
                );
                (owner.clone(), step)
            })
            .collect();
        let setup_ms = setup.elapsed().as_secs_f64() * 1000.0;
        for sample in 0..5 {
            let mut baseline_hops = 0usize;
            let start = Instant::now();
            let baseline: Vec<_> = people
                .iter()
                .map(|owner| {
                    let mut current = owner.clone();
                    let mut seen = HashSet::new();
                    for _ in 0..depth {
                        if !seen.insert(current.clone()) {
                            return None;
                        }
                        baseline_hops += 1;
                        match edges.get(&current).unwrap() {
                            Step::Next(next) => current = next.clone(),
                            Step::Stop(vote) => return *vote,
                        }
                    }
                    None
                })
                .collect();
            let baseline_ms = start.elapsed().as_secs_f64() * 1000.0;
            let mut walk = Walk::new();
            let mut loads = 0usize;
            let start = Instant::now();
            let actual: Vec<_> = people
                .iter()
                .map(|owner| {
                    walk.resolve(owner, depth, |node| {
                        loads += 1;
                        Ok::<_, ()>(edges.get(node).unwrap().clone())
                    })
                    .unwrap()
                })
                .collect();
            let resolved_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(actual, baseline);
            assert_eq!(loads, population);
            let slots: usize = walk.prefixes.values().map(|slots| slots.len()).sum();
            let entries = walk
                .prefixes
                .values()
                .flatten()
                .filter(|entry| entry.is_some())
                .count();
            let capacity: usize = walk.prefixes.values().map(PrefixSlots::capacity).sum();
            let levels = (usize::BITS - depth.leading_zeros()) as usize;
            assert!(slots <= population * levels);
            if shape == "stopped" {
                assert_eq!(slots, 0);
                assert_eq!(capacity, 0);
                assert!(walk.prefixes.is_empty());
            }
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let memory: Vec<_> = status
                .lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
                .collect();
            println!(
                "gov_scale {}",
                serde_json::json!({
                    "kind":"key_prefix", "population":population, "depth":depth, "shape":shape,
                    "sample":sample, "setup_ms":setup_ms, "baseline_ms":baseline_ms,
                    "resolved_ms":resolved_ms, "baseline_hops":baseline_hops, "base_loads":loads,
                    "prefix_slots":slots, "prefix_entries":entries, "prefix_capacity":capacity,
                    "prefix_inline_slots":walk.prefixes.values().filter(|slots| matches!(slots, PrefixSlots::One(Some(_)))).count(),
                    "memo_value_bytes":std::mem::size_of::<PrefixSlots<WalletAddress, u8>>(),
                    "map_capacity":walk.prefixes.capacity(), "memory":memory,
                })
            );
            if std::env::var_os("BURST_GOV_PROBE_STAGES").is_some() {
                // Separate pass: clock overhead and allocator state preclude
                // attributing these stage sums exactly to the earlier total.
                drop(walk);
                let mut profiled = Walk::new();
                profiled.stages = Some(Stages::default());
                let start = Instant::now();
                let output: Vec<_> = people
                    .iter()
                    .map(|owner| {
                        profiled
                            .resolve(owner, depth, |node| {
                                Ok::<_, ()>(edges.get(node).unwrap().clone())
                            })
                            .unwrap()
                    })
                    .collect();
                let instrumented_ms = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(output, baseline);
                assert_eq!(profiled.stages.as_ref().unwrap().loads, population);
                println!(
                    "gov_stages {}",
                    serde_json::json!({
                        "population":population, "depth":depth, "shape":shape, "sample":sample,
                        "instrumented_ms":instrumented_ms, "stages":profiled.stages,
                        "prefix_capacity":profiled.prefixes.values().map(PrefixSlots::capacity).sum::<usize>(),
                        "map_capacity":profiled.prefixes.capacity(),
                    })
                );
            }
        }
    }
}
