//! Physical qubit connectivity for backends whose native two-qubit gate
//! can only be applied directly between *adjacent* qubits.
//!
//! `TrappedIon` has no [`CouplingMap`] of its own: a trapped-ion chain
//! (as already modeled by `native.rs`/`fidelity.rs`) has every qubit
//! interact with every other qubit through the shared motional mode, so
//! there's nothing to route -- `Backend::coupling_map` returns `None`
//! for it.
//!
//! # P1.1 (this module's heavy-hex generator)
//!
//! `IbmQ` now routes against a real heavy-hex lattice
//! ([`CouplingMap::heavy_hex_for`]/[`CouplingMap::heavy_hex_grid`]),
//! not the conservative linear-chain stand-in this module used to fall
//! back on for every non-all-to-all backend. Heavy-hex is the actual
//! published topology family IBM's superconducting processors use
//! (Eagle, Heron, ...): take a hexagonal lattice of "data" qubits (each
//! hexagon vertex has degree <= 3) and place one extra "flag"/"heavy"
//! qubit at the midpoint of every edge (each of those has degree 2).
//! The construction here follows the same edge-generation rule NetworkX's
//! `hexagonal_lattice_graph(m, n)` uses for an `m`-row, `n`-column grid
//! of hexagons (`m=n=1` -> a single hexagon, 6 data + 6 flag qubits = 12
//! total, matching the "12 qubits per hexagon" figure repeatedly cited
//! for heavy-hex device topologies), then subdivides every edge once.
//! `heavy_hex_grid` builds that lattice exactly; `heavy_hex_for(n)`
//! finds the smallest hexagon grid with at least `n` qubits and takes a
//! BFS-order prefix of exactly `n` -- guaranteed connected, since a
//! breadth-first prefix of a connected graph always is (every non-root
//! node in the prefix was discovered through an edge to an
//! already-included, earlier node).
//!
//! This still isn't a claim about any *specific* chip's exact physical
//! qubit numbering (real devices retire/reroute around individual bad
//! qubits, and IBM's own numbering for a given processor is its own
//! published layout, not derived from this generator) -- it's the
//! actual heavy-hex *topology family*, which is the part that matters
//! for routing correctness.
//!
//! `Rigetti` is still modeled as a fixed nearest-neighbor chain
//! (`0-1-2-...-(n-1)`, see [`CouplingMap::linear`]) -- its real grid
//! topology is a more permissive superset of a line (every interior
//! qubit has more than two neighbors), so a line remains a conservative
//! stand-in there: routing that succeeds against a line also succeeds,
//! with room to spare, against the real grid. Modeling Rigetti's actual
//! grid graph is separate future work from this heavy-hex item.

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

    /// The real heavy-hex lattice for an `m`-row, `n`-column grid of
    /// hexagons (`m, n >= 1`): a hexagonal lattice of "data" qubits
    /// with one extra "flag" qubit subdividing every edge. See this
    /// module's doc comment for the construction and its verification
    /// against the published "12 qubits per hexagon" figure at `m=n=1`.
    ///
    /// Node numbering is a deterministic BFS order from a fixed corner
    /// of the lattice (see [`heavy_hex_bfs_map`]), so it's stable
    /// across calls but not meant to line up with any real device's
    /// own published qubit numbering (see this module's doc comment).
    ///
    /// # Panics
    /// If `rows == 0` or `cols == 0` -- there is no such thing as a
    /// 0-row or 0-column grid of hexagons; use [`CouplingMap::linear`]
    /// (or an empty map) for a topology-free 0/1-qubit case instead.
    pub fn heavy_hex_grid(rows: usize, cols: usize) -> Self {
        assert!(
            rows >= 1 && cols >= 1,
            "heavy_hex_grid requires at least 1 row and 1 column of hexagons, got {}x{}",
            rows,
            cols
        );
        let hex_edges = hexagonal_lattice_edges(rows, cols);
        let heavy_edges = subdivide_edges(&hex_edges);
        heavy_hex_bfs_map(&heavy_edges, HeavyHexNode::Data(0, 0), None)
    }

    /// The smallest heavy-hex lattice with at least `num_qubits`
    /// qubits, truncated to exactly `num_qubits` by taking a
    /// breadth-first prefix from a fixed corner -- guaranteed
    /// connected (a BFS prefix of a connected graph always is: every
    /// non-root node in the prefix was discovered through an edge to
    /// an already-included, earlier node). This is what
    /// [`crate::backend::Backend::coupling_map`] uses for `IbmQ`.
    ///
    /// `num_qubits <= 1` returns a topology-free map (no edges needed
    /// to route a 0- or 1-qubit circuit), matching
    /// [`CouplingMap::linear`]'s behavior at the same sizes.
    pub fn heavy_hex_for(num_qubits: usize) -> Self {
        if num_qubits <= 1 {
            return Self {
                num_qubits,
                edges: HashSet::new(),
            };
        }
        let mut d = 1usize;
        loop {
            let hex_edges = hexagonal_lattice_edges(d, d);
            let heavy_edges = subdivide_edges(&hex_edges);
            let total = heavy_hex_node_count(&heavy_edges);
            if total >= num_qubits {
                let mut cm = heavy_hex_bfs_map(
                    &heavy_edges,
                    HeavyHexNode::Data(0, 0),
                    Some(num_qubits),
                );
                debug_assert_eq!(
                    cm.num_qubits, num_qubits,
                    "a d x d heavy-hex grid with >= num_qubits total qubits is always \
                     connected, so a BFS prefix should always reach exactly num_qubits"
                );
                cm.num_qubits = num_qubits;
                return cm;
            }
            d += 1;
        }
    }

    pub fn is_adjacent(&self, a: usize, b: usize) -> bool {
        let key = if a < b { (a, b) } else { (b, a) };
        self.edges.contains(&key)
    }

    /// Physical qubits directly coupling-adjacent to `q`, in ascending
    /// order. Used by `route.rs`'s identity-restoration pass to build a
    /// spanning tree of the coupling graph -- unlike `is_adjacent`
    /// (a yes/no check for one specific pair) or `shortest_path`
    /// (point-to-point), this is the building block for algorithms that
    /// need the graph's actual adjacency structure, e.g. any non-linear
    /// map like [`CouplingMap::heavy_hex_for`]/[`CouplingMap::heavy_hex_grid`].
    pub fn neighbors(&self, q: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for &(a, b) in &self.edges {
            if a == q {
                out.push(b);
            } else if b == q {
                out.push(a);
            }
        }
        out.sort_unstable();
        out
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

// ---------------------------------------------------------------------
// Heavy-hex construction internals.
//
// `hexagonal_lattice_edges` reproduces (with 0-based, non-negative
// coordinates instead of a general graph library) the same edge-
// generation rule NetworkX's `hexagonal_lattice_graph(m, n)` uses in
// its non-periodic case: build a full (n+1)-column x (2m+2)-row grid
// of vertical "column" edges, add horizontal "row" edges only between
// columns of matching parity, then drop the two corner nodes that
// would otherwise be left with degree 1. Verified by hand against the
// m=n=1 case (see `heavy_hex_grid_1x1_matches_a_single_hexagon` below):
// 6 vertices, 6 edges -- a hexagon (a 6-cycle), as expected for a
// single-hexagon grid.
// ---------------------------------------------------------------------

/// A node in the *pre-subdivision* hex lattice, addressed by its
/// (column, row) grid coordinate.
type HexCoord = (usize, usize);

fn hexagonal_lattice_edges(rows: usize, cols: usize) -> Vec<(HexCoord, HexCoord)> {
    let m = rows;
    let n = cols;
    let big_m = 2 * m;

    let mut edges: HashSet<(HexCoord, HexCoord)> = HashSet::new();

    // Vertical "column" edges: a full path down every column i, for
    // j = 0..=big_m (connecting the big_m + 2 nodes in that column).
    for i in 0..=n {
        for j in 0..=big_m {
            edges.insert(((i, j), (i, j + 1)));
        }
    }
    // Horizontal "row" edges: between column i and i+1, only at rows j
    // where i and j have the same parity -- this is what turns the
    // grid into hexagons instead of a full brick wall.
    for i in 0..n {
        for j in 0..=(big_m + 1) {
            if i % 2 == j % 2 {
                edges.insert(((i, j), (i + 1, j)));
            }
        }
    }

    // The two corners left with degree 1 by the pattern above aren't
    // part of any hexagon; NetworkX's generator removes them and so do
    // we, for the same reason.
    let corner1: HexCoord = (0, big_m + 1);
    let corner2: HexCoord = (n, (big_m + 1) * (n % 2));
    edges.retain(|&(a, b)| a != corner1 && b != corner1 && a != corner2 && b != corner2);

    let mut out: Vec<(HexCoord, HexCoord)> = edges.into_iter().collect();
    out.sort();
    out
}

/// A node in the *post-subdivision* heavy-hex graph: either an
/// original hex-lattice vertex (a "data" qubit) or a new qubit
/// inserted at the midpoint of the `k`-th hex-lattice edge (a "flag"
/// qubit, `k` indexing into the same sorted edge list
/// `hexagonal_lattice_edges` returns, so it's deterministic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum HeavyHexNode {
    Data(usize, usize),
    Flag(usize),
}

/// Subdivides every hex-lattice edge once: `(a, b)` becomes
/// `(a, flag) , (flag, b)` for a fresh `Flag` node per edge.
fn subdivide_edges(hex_edges: &[(HexCoord, HexCoord)]) -> Vec<(HeavyHexNode, HeavyHexNode)> {
    let mut out = Vec::with_capacity(hex_edges.len() * 2);
    for (k, &(a, b)) in hex_edges.iter().enumerate() {
        let flag = HeavyHexNode::Flag(k);
        out.push((HeavyHexNode::Data(a.0, a.1), flag));
        out.push((flag, HeavyHexNode::Data(b.0, b.1)));
    }
    out
}

/// Total qubit count (data + flag) of the heavy-hex graph described by
/// `heavy_edges` -- the number of distinct nodes touched by any edge.
fn heavy_hex_node_count(heavy_edges: &[(HeavyHexNode, HeavyHexNode)]) -> usize {
    let mut seen: HashSet<HeavyHexNode> = HashSet::new();
    for &(a, b) in heavy_edges {
        seen.insert(a);
        seen.insert(b);
    }
    seen.len()
}

/// Assigns integer qubit indices to a heavy-hex graph via BFS from
/// `start`, and builds the resulting [`CouplingMap`]. If `limit` is
/// `Some(n)`, only the first `n` BFS-visited nodes get an index (and
/// only edges between two indexed nodes survive) -- see
/// [`CouplingMap::heavy_hex_for`]'s doc comment for why that prefix is
/// always connected.
fn heavy_hex_bfs_map(
    heavy_edges: &[(HeavyHexNode, HeavyHexNode)],
    start: HeavyHexNode,
    limit: Option<usize>,
) -> CouplingMap {
    let mut adjacency: HashMap<HeavyHexNode, Vec<HeavyHexNode>> = HashMap::new();
    for &(a, b) in heavy_edges {
        adjacency.entry(a).or_default().push(b);
        adjacency.entry(b).or_default().push(a);
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
    }

    let mut order: Vec<HeavyHexNode> = Vec::new();
    let mut visited: HashSet<HeavyHexNode> = HashSet::new();
    let mut queue: VecDeque<HeavyHexNode> = VecDeque::new();
    queue.push_back(start);
    visited.insert(start);
    while let Some(node) = queue.pop_front() {
        if let Some(lim) = limit {
            if order.len() >= lim {
                break;
            }
        }
        order.push(node);
        if let Some(neighbors) = adjacency.get(&node) {
            for &next in neighbors {
                if visited.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }

    let index: HashMap<HeavyHexNode, usize> =
        order.iter().enumerate().map(|(i, &n)| (n, i)).collect();

    let mut edges: HashSet<(usize, usize)> = HashSet::new();
    for &(a, b) in heavy_edges {
        if let (Some(&ia), Some(&ib)) = (index.get(&a), index.get(&b)) {
            edges.insert(if ia < ib { (ia, ib) } else { (ib, ia) });
        }
    }

    CouplingMap {
        num_qubits: order.len(),
        edges,
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

    /// A 1x1 grid of hexagons is a single hexagon: 6 data qubits + 6
    /// flag qubits (one per edge) = 12 total, matching the "each
    /// hexagon has 12 qubits" figure cited for heavy-hex device
    /// topologies. The 6 data qubits should form a 6-cycle before
    /// subdivision, so after subdivision every qubit has exactly 2
    /// neighbors (it's just a longer, 12-node cycle).
    #[test]
    fn heavy_hex_grid_1x1_matches_a_single_hexagon() {
        let map = CouplingMap::heavy_hex_grid(1, 1);
        assert_eq!(map.num_qubits(), 12, "a single hexagon should be 6 data + 6 flag qubits");
        for q in 0..12 {
            let degree = (0..12).filter(|&other| other != q && map.is_adjacent(q, other)).count();
            assert_eq!(degree, 2, "qubit {} should have exactly 2 neighbors on a bare hexagon", q);
        }
        // It should be one connected cycle, not e.g. two disjoint triangles.
        for target in 1..12 {
            assert!(
                map.shortest_path(0, target).is_some(),
                "qubit {} should be reachable from qubit 0",
                target
            );
        }
    }

    /// No qubit in a heavy-hex lattice should have more than 3
    /// neighbors: data qubits sit at hexagon vertices (degree <= 3 in
    /// the pre-subdivision lattice), flag qubits sit at edge midpoints
    /// (always degree 2). A larger grid than the bare-hexagon case
    /// above is needed to actually exercise a degree-3 data qubit.
    #[test]
    fn heavy_hex_grid_has_max_degree_three() {
        let map = CouplingMap::heavy_hex_grid(3, 3);
        let mut saw_degree_three = false;
        for q in 0..map.num_qubits() {
            let degree = (0..map.num_qubits())
                .filter(|&other| other != q && map.is_adjacent(q, other))
                .count();
            assert!(degree <= 3, "qubit {} has degree {} > 3", q, degree);
            if degree == 3 {
                saw_degree_three = true;
            }
        }
        assert!(saw_degree_three, "a 3x3 grid should have at least one interior degree-3 data qubit");
    }

    #[test]
    fn heavy_hex_grid_is_fully_connected() {
        let map = CouplingMap::heavy_hex_grid(2, 3);
        for target in 1..map.num_qubits() {
            assert!(
                map.shortest_path(0, target).is_some(),
                "qubit {} should be reachable from qubit 0 in a 2x3 heavy-hex grid",
                target
            );
        }
    }

    /// `heavy_hex_for` must return *exactly* the requested qubit count
    /// (not just "at least"), for a range of sizes including ones that
    /// don't land exactly on a d x d grid's natural qubit count.
    #[test]
    fn heavy_hex_for_returns_exact_requested_size() {
        for n in [0, 1, 2, 5, 12, 13, 25, 50, 77] {
            let map = CouplingMap::heavy_hex_for(n);
            assert_eq!(map.num_qubits(), n, "heavy_hex_for({}) returned the wrong qubit count", n);
        }
    }

    /// The whole point of building the truncated map via a BFS prefix:
    /// it must stay connected even when it cuts a real heavy-hex grid
    /// off partway through, for sizes both smaller and larger than one
    /// bare hexagon (12 qubits).
    #[test]
    fn heavy_hex_for_is_always_connected() {
        for n in [2, 5, 12, 13, 25, 50, 77] {
            let map = CouplingMap::heavy_hex_for(n);
            for target in 1..n {
                assert!(
                    map.shortest_path(0, target).is_some(),
                    "heavy_hex_for({}): qubit {} unreachable from qubit 0",
                    n,
                    target
                );
            }
        }
    }

    #[test]
    fn heavy_hex_for_never_exceeds_degree_three() {
        for n in [12, 25, 50, 77] {
            let map = CouplingMap::heavy_hex_for(n);
            for q in 0..n {
                let degree = (0..n).filter(|&other| other != q && map.is_adjacent(q, other)).count();
                assert!(degree <= 3, "heavy_hex_for({}): qubit {} has degree {} > 3", n, q, degree);
            }
        }
    }

    #[test]
    #[should_panic]
    fn heavy_hex_grid_rejects_zero_rows_or_cols() {
        CouplingMap::heavy_hex_grid(0, 3);
    }

    #[test]
    fn neighbors_matches_is_adjacent_on_a_linear_map() {
        let map = CouplingMap::linear(4);
        assert_eq!(map.neighbors(0), vec![1]);
        assert_eq!(map.neighbors(1), vec![0, 2]);
        assert_eq!(map.neighbors(2), vec![1, 3]);
        assert_eq!(map.neighbors(3), vec![2]);
    }

    #[test]
    fn neighbors_matches_is_adjacent_on_a_heavy_hex_map() {
        let map = CouplingMap::heavy_hex_grid(2, 3);
        for q in 0..map.num_qubits() {
            let nbrs = map.neighbors(q);
            for other in 0..map.num_qubits() {
                assert_eq!(
                    nbrs.contains(&other),
                    map.is_adjacent(q, other),
                    "neighbors({}) disagrees with is_adjacent({}, {})",
                    q,
                    q,
                    other
                );
            }
        }
    }
}