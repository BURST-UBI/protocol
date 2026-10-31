//! Cut-local ordinary delegation resolution. No authenticated metadata is read.
use super::{DelegationEngine, DelegationScope, HashMap, HashSet, TxHash, WalletAddress};

#[derive(Clone, Copy)]
struct Terminal<'a> {
    address: &'a WalletAddress,
    // The legacy resolver counts the terminal visit against its depth budget.
    visits: usize,
}

// Enabled only by the diagnostic's separate instrumented pass. Timings include
// clock overhead and must not be compared with the uninstrumented total.
#[cfg(test)]
#[derive(Default, serde::Serialize)]
struct ResolverStages {
    memo_read_ns: u128,
    memo_write_ns: u128,
    eligibility_cycle_ns: u128,
    edge_selection_ns: u128,
    memo_hits: usize,
    memo_misses: usize,
    memo_writes: usize,
    terminal_writes: usize,
}

pub(crate) struct BallotResolver<'a, F> {
    engine: &'a DelegationEngine,
    proposal: Option<&'a TxHash>,
    parameter: Option<&'a crate::GovernableParam>,
    category: Option<&'a str>,
    eligible: F,
    // Keys/endpoints borrow the immutable cut; no address payload copies.
    resolved: HashMap<&'a WalletAddress, Option<Terminal<'a>>>,
    #[cfg(test)]
    lookups: usize,
    #[cfg(test)]
    stages: Option<ResolverStages>,
}

impl<'a, F: Fn(&WalletAddress) -> bool> BallotResolver<'a, F> {
    /// All inputs, including eligibility, must stay fixed for this counting cut.
    pub(crate) fn new(
        engine: &'a DelegationEngine, proposal: Option<&'a TxHash>,
        parameter: Option<&'a crate::GovernableParam>, category: Option<&'a str>, eligible: F,
    ) -> Self {
        Self { engine, proposal, parameter, category, eligible, resolved: HashMap::new(),
            #[cfg(test)] lookups: 0, #[cfg(test)] stages: None }
    }

    pub(crate) fn resolve(&mut self, from: &'a WalletAddress) -> Option<WalletAddress> {
        if self.engine.max_depth == 0 { return None }
        let mut current = from;
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut result = loop {
            #[cfg(test)]
            let started = self.stages.as_ref().map(|_| std::time::Instant::now());
            let known = self.resolved.get(current);
            #[cfg(test)]
            if let Some(stages) = &mut self.stages {
                stages.memo_read_ns += started.unwrap().elapsed().as_nanos();
                if known.is_some() { stages.memo_hits += 1; } else { stages.memo_misses += 1; }
            }
            if let Some(known) = known { break *known }
            #[cfg(test)]
            let started = self.stages.as_ref().map(|_| std::time::Instant::now());
            let invalid = !(self.eligible)(current) || seen.contains(current);
            #[cfg(test)]
            if let Some(stages) = &mut self.stages {
                stages.eligibility_cycle_ns += started.unwrap().elapsed().as_nanos();
            }
            if invalid { break None }
            #[cfg(test)] { self.lookups += 1; }
            #[cfg(test)]
            let started = self.stages.as_ref().map(|_| std::time::Instant::now());
            let next = self.proposal.and_then(|hash| self.engine.scoped_delegations
                .get(&(current.clone(), DelegationScope::Proposal(*hash))))
                .or_else(|| self.parameter.and_then(|parameter| self.engine.scoped_delegations
                    .get(&(current.clone(), DelegationScope::Parameter(parameter.clone())))))
                .or_else(|| self.category.and_then(|category| self.engine.scoped_delegations
                    .get(&(current.clone(), DelegationScope::Category(category.to_owned())))))
                .or_else(|| self.engine.delegations.get(current));
            #[cfg(test)]
            if let Some(stages) = &mut self.stages {
                stages.edge_selection_ns += started.unwrap().elapsed().as_nanos();
            }
            match next {
                Some(next) => { seen.insert(current); path.push(current); current = next; }
                None => {
                    let terminal = Some(Terminal { address: current, visits: 1 });
                    #[cfg(test)]
                    let started = self.stages.as_ref().map(|_| std::time::Instant::now());
                    // A root terminal has no suffix to share. Retain terminals
                    // only when another owner reaches them through an edge.
                    if !path.is_empty() { self.resolved.insert(current, terminal); }
                    #[cfg(test)]
                    if !path.is_empty() { if let Some(stages) = &mut self.stages {
                        stages.memo_write_ns += started.unwrap().elapsed().as_nanos();
                        stages.memo_writes += 1;
                        stages.terminal_writes += 1;
                    } }
                    break terminal;
                }
            }
        };
        // Nonterminal nodes and suffix terminals are discovered once. A terminal
        // queried before any incoming path may be checked once more when reached
        // by that path, then cached. For Q queries and V reached nodes, work stays
        // O(Q + V); isolated terminals consume no memo space. Distances preserve
        // per-owner depth even when discovery exceeds that owner's budget.
        for owner in path.into_iter().rev() {
            result = result.and_then(|terminal| terminal.visits.checked_add(1)
                .map(|visits| Terminal { address: terminal.address, visits }));
            #[cfg(test)]
            let started = self.stages.as_ref().map(|_| std::time::Instant::now());
            self.resolved.insert(owner, result);
            #[cfg(test)]
            if let Some(stages) = &mut self.stages {
                stages.memo_write_ns += started.unwrap().elapsed().as_nanos();
                stages.memo_writes += 1;
            }
        }
        result.filter(|terminal| terminal.visits <= self.engine.max_depth)
            .map(|terminal| terminal.address.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_small_graphs_match_depth_limited_walk_in_both_query_orders() {
        let people: Vec<_> = (0..4).map(|i| WalletAddress::new(format!("brst_{i}"))).collect();
        // Every outgoing edge is absent or points to one of four nodes. Includes
        // cycles, self edges, shared tails and chains beyond the caller's budget.
        for encoded in 0..625usize {
            let mut engine = DelegationEngine::default();
            let mut edges = encoded;
            for owner in &people {
                let edge = edges % 5;
                edges /= 5;
                if edge != 0 { engine.delegations.insert(owner.clone(), people[edge - 1].clone()); }
            }
            for depth in 0..=5 {
                engine.max_depth = depth;
                for mask in 0..16usize {
                    let eligible = |person: &WalletAddress| people.iter().enumerate()
                        .any(|(i, p)| p == person && mask & (1 << i) != 0);
                    for reverse in [false, true] {
                        let mut cached = BallotResolver::new(&engine, None, None, None, eligible);
                        for n in 0..4 {
                            let owner = &people[if reverse { 3 - n } else { n }];
                            assert_eq!(cached.resolve(owner), engine.resolve_for_ballot(owner,
                                None, None, None, eligible), "graph {encoded}, depth {depth}, mask {mask}");
                        }
                        let terminals = people.iter().filter(|p| eligible(p)
                            && !engine.delegations.contains_key(*p)).count();
                        // A terminal queried before its incoming path is checked
                        // twice at most; all nonterminals still expand once.
                        assert!(cached.lookups <= mask.count_ones() as usize + terminals);
                    }
                }
            }
        }
    }

    #[test]
    fn shared_tails_respect_scope_precedence_and_resolve_each_node_once() {
        let mut engine = DelegationEngine::default();
        engine.max_depth = 4;
        let hash = TxHash::new([8; 32]);
        let parameter = crate::GovernableParam::BrnRate;
        let tail = WalletAddress::new("brst_tail");
        let end = WalletAddress::new("brst_end");
        let wrong = WalletAddress::new("brst_wrong");
        engine.delegations.insert(tail.clone(), wrong.clone());
        engine.scoped_delegations.insert((tail.clone(), DelegationScope::Category("economy".into())), wrong.clone());
        engine.scoped_delegations.insert((tail.clone(), DelegationScope::Parameter(parameter.clone())), wrong.clone());
        engine.scoped_delegations.insert((tail.clone(), DelegationScope::Proposal(hash)), end.clone());
        let owners: Vec<_> = (0..1000).map(|i| WalletAddress::new(format!("brst_owner{i}"))).collect();
        for owner in &owners { engine.delegations.insert(owner.clone(), tail.clone()); }
        let mut cached = BallotResolver::new(&engine, Some(&hash), Some(&parameter), Some("economy"), |_| true);
        for owner in &owners {
            assert_eq!(cached.resolve(owner), Some(end.clone()));
            assert_eq!(cached.resolve(owner), engine.resolve_for_ballot(owner, Some(&hash), Some(&parameter), Some("economy"), |_| true));
        }
        assert_eq!(cached.lookups, owners.len() + 2);
    }
    #[test]
    fn root_terminals_skip_storage_and_later_incoming_paths_cache_them() {
        let people: Vec<_> = (0..4).map(|i| WalletAddress::new(format!("brst_{i}"))).collect();
        let mut engine = DelegationEngine::default();
        engine.delegations.insert(people[0].clone(), people[1].clone());
        engine.delegations.insert(people[1].clone(), people[2].clone());
        let mut resolver = BallotResolver::new(&engine, None, None, None, |_| true);
        // Terminal first, then incoming chain, then repeated owners and isolated
        // voter. Neither query order nor repetition may alter voting results.
        for i in [2, 0, 1, 2, 0, 3, 3] {
            assert_eq!(resolver.resolve(&people[i]),
                engine.resolve_for_ballot(&people[i], None, None, None, |_| true));
            if i == 2 && resolver.lookups == 1 { assert!(resolver.resolved.is_empty()); }
        }
        assert_eq!(resolver.lookups, 6); // Four discoveries plus two isolated queries.
        assert_eq!(resolver.resolved.len(), 3);
        assert!(!resolver.resolved.contains_key(&people[3]));
    }

    #[test]
    #[ignore = "explicit capped-host governance scaling diagnostic"]
    fn ordinary_delegation_scaling() {
        use std::time::Instant;
        let n: usize = std::env::var("BURST_GOV_PROBE_POPULATION").unwrap().parse().unwrap();
        assert!((10..=100_000).contains(&n));
        let shape = std::env::var("BURST_GOV_PROBE_SHAPE").unwrap();
        assert!(["independent", "sparse", "shared_tail", "long_chain", "cycle"].contains(&shape.as_str()));
        let setup = Instant::now();
        let people: Vec<_> = (0..n).map(|i| WalletAddress::new(format!("brst_{i:060}"))).collect();
        let eligible: HashSet<_> = people.iter().collect();
        let mut engine = DelegationEngine::default();
        engine.max_depth = 10;
        for i in 0..n {
            let next = match shape.as_str() {
                "shared_tail" if i + 1 < n => Some(if i < n - 9 { n - 9 } else { i + 1 }),
                "long_chain" if i + 1 < n => Some(i + 1),
                "cycle" => Some((i + 1) % n),
                "sparse" if i % 10 == 0 && i + 1 < n => Some(i + 1),
                _ => None,
            };
            if let Some(next) = next { engine.delegations.insert(people[i].clone(), people[next].clone()); }
        }
        let setup_ms = setup.elapsed().as_secs_f64() * 1000.0;
        for sample in 0..5 {
            let start = Instant::now();
            let baseline: Vec<_> = people.iter().map(|owner| engine.resolve_for_ballot(owner,
                None, None, None, |person| eligible.contains(person))).collect();
            let baseline_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let mut resolver = BallotResolver::new(&engine, None, None, None, |person| eligible.contains(person));
            let actual: Vec<_> = people.iter().map(|owner| resolver.resolve(owner)).collect();
            let resolved_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(actual, baseline);
            assert_eq!(resolver.lookups, n);
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let memory: Vec<_> = status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
            println!("gov_scale {}", serde_json::json!({"kind":"resolver", "shape":shape,
                "population":n,"sample":sample,"setup_ms":setup_ms,"baseline_ms":baseline_ms,
                "resolved_ms":resolved_ms,"edge_lookups":resolver.lookups,"memory":memory}));
            if std::env::var_os("BURST_GOV_PROBE_STAGES").is_some() {
                // Keep output equality and once-per-node assertions independent of
                // the timings. Release the previous memo before this extra pass.
                drop(resolver);
                let mut profiled = BallotResolver::new(&engine, None, None, None,
                    |person| eligible.contains(person));
                profiled.stages = Some(ResolverStages::default());
                let started = Instant::now();
                let output: Vec<_> = people.iter().map(|owner| profiled.resolve(owner)).collect();
                let instrumented_ms = started.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(output, baseline);
                assert_eq!(profiled.lookups, n);
                let stages = profiled.stages.as_ref().unwrap();
                // Closing the cycle probes the first node again before the
                // visited-set check rejects it; no outgoing edge is expanded.
                assert_eq!(stages.memo_misses, n + usize::from(shape == "cycle"));
                if shape == "independent" {
                    assert_eq!(stages.memo_hits, 0);
                    assert_eq!(stages.terminal_writes, 0);
                    assert_eq!(stages.memo_writes, 0);
                    assert!(profiled.resolved.is_empty());
                }
                if shape == "sparse" {
                    assert_eq!(stages.memo_hits, (n + 8) / 10);
                    assert_eq!(stages.terminal_writes, (n + 8) / 10);
                    assert_eq!(stages.memo_writes, 2 * ((n + 8) / 10));
                }
                println!("gov_stages {}", serde_json::json!({"shape":shape,
                    "population":n,"sample":sample,"instrumented_ms":instrumented_ms,
                    "memo_entries":profiled.resolved.len(),"stages":stages}));
            }
        }
    }

}
