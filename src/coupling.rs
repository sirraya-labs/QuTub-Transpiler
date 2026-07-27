//! Physical qubit connectivity for backends whose native two-qubit gate
//! can only be applied directly between *adjacent* qubits.
//!
//! `TrappedIon` has no [`CouplingMap`] of its own: a trapped-ion chain
//! (as already modeled by `native.rs`/`fidelity.rs`) has every qubit
//! interact with every other qubit through the shared motional mode, so
//! there's nothing to route -- `Backend::coupling_map` returns `None`
//! for it. `IbmQ` and `Rigetti` are both modeled here as a fixed
//! nearest-neighbor chain (`0-1-2-...-(n-1)`, see [`CouplingMap::linear`]).
//!
//! This is a deliberate simplification, not a claim about either
//! device's real topology: IBM's heavy-hex lattice and Rigetti's grid
//! are both *more* permissive than a line (every interior qubit has
//! more than two neighbors), so a line is a conservative subset every
//! real layout contains as a Hamiltonian path -- routing that succeeds
//! against a line also succeeds, with room to spare, against the real
//! lattice. Modeling the actual heavy-hex/grid graphs (and, more
//! importantly, a specific real device's exact physical qubit
//! numbering) is real future work; this closes the "any qubit pair can
//! interact directly" gap that [`crate::backend::lower`] had before,
//! without pretending to know a specific chip's exact wiring.

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct CouplingMap {
    num_qubits: usize,
    // Undirected edges, always stored with the smaller index first.
    edges: HashSet<(usize, usize)>,
}

impl CouplingMap {
    /// A nearest-neighbor chain: qubit `q` is adjacent to `q + 1` only.
    pub fn linear(num_qubits: usize) -> Self {
        let mut edges = HashSet::new();
        for q in 0..num_qubits.saturating_sub(1) {
            edges.insert((q, q + 1));
        }
        Self { num_qubits, edges }
    }

    pub fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    pub fn is_adjacent(&self, a: usize, b: usize) -> bool {
        let key = if a < b { (a, b) } else { (b, a) };
        self.edges.contains(&key)
    }

    /// Shortest path between two physical qubits (inclusive of both
    /// endpoints), via BFS. `None` if no path exists (the coupling
    /// graph is disconnected between `start` and `goal` -- never
    /// happens for [`CouplingMap::linear`], but a routing pass built on
    /// top of a future non-linear map should still handle it rather
    /// than panic on a caller's behalf).
    pub fn shortest_path(&self, start: usize, goal: usize) -> Option<Vec<usize>> {
        if start == goal {
            return Some(vec![start]);
        }

        let mut adjacency: HashMap<usize, Vec<usize>> = HashMap::new();
        for &(a, b) in &self.edges {
            adjacency.entry(a).or_default().push(b);
            adjacency.entry(b).or_default().push(a);
        }

        let mut visited = vec![false; self.num_qubits];
        let mut predecessor = vec![usize::MAX; self.num_qubits];
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited[start] = true;

        while let Some(current) = queue.pop_front() {
            if current == goal {
                break;
            }
            if let Some(neighbors) = adjacency.get(&current) {
                for &next in neighbors {
                    if !visited[next] {
                        visited[next] = true;
                        predecessor[next] = current;
                        queue.push_back(next);
                    }
                }
            }
        }

        if !visited[goal] {
            return None;
        }

        let mut path = vec![goal];
        let mut current = goal;
        while current != start {
            current = predecessor[current];
            path.push(current);
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_map_has_only_neighbor_edges() {
        let map = CouplingMap::linear(4);
        assert!(map.is_adjacent(0, 1));
        assert!(map.is_adjacent(1, 2));
        assert!(map.is_adjacent(2, 3));
        assert!(!map.is_adjacent(0, 2));
        assert!(!map.is_adjacent(0, 3));
        assert!(!map.is_adjacent(1, 3));
        // Adjacency is symmetric.
        assert!(map.is_adjacent(1, 0));
    }

    #[test]
    fn shortest_path_walks_the_chain() {
        let map = CouplingMap::linear(5);
        assert_eq!(map.shortest_path(0, 3), Some(vec![0, 1, 2, 3]));
        assert_eq!(map.shortest_path(4, 4), Some(vec![4]));
        assert_eq!(map.shortest_path(3, 0), Some(vec![3, 2, 1, 0]));
    }

    #[test]
    fn single_qubit_map_has_no_edges() {
        let map = CouplingMap::linear(1);
        assert!(!map.is_adjacent(0, 0));
        assert_eq!(map.shortest_path(0, 0), Some(vec![0]));
    }
}
