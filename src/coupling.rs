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
//! DFS-order prefix of exactly `n` -- guaranteed connected, since a
//! depth-first prefix of a connected graph always is (every non-root
//! node in the prefix was discovered through an edge to an
//! already-included, earlier node).
//!
//! # Why DFS, not BFS, for the node numbering
//! This used to be a BFS-order prefix, which is connected for the same
//! reason a DFS one is -- but BFS numbering is much worse for the thing
//! that numbering actually gets used for downstream: `route.rs`'s
//! `choose_initial_layout`/`find_hamiltonian_path` specifically search
//! for a physical qubit path that lines up with the *identity* mapping
//! (physical index == logical index), because that's the one layout a
//! circuit can reach with zero `Swap`s. BFS visits *every* neighbor of
//! a node before moving on, so on any node of degree >= 2 (i.e.
//! essentially everywhere in this topology) only the first of those
//! neighbors can land on the very next index -- consecutive BFS indices
//! are graph-adjacent almost nowhere beyond the root, which made the
//! identity-mapping search's "free" case nearly unreachable in
//! practice (confirmed empirically: for `heavy_hex_for(16)`, only 1 of
//! 15 consecutive-index pairs `(i, i+1)` was an actual coupling edge
//! under the old BFS numbering). DFS numbering, by contrast, keeps
//! extending into a fresh, unvisited neighbor before backtracking, so
//! consecutive indices stay graph-adjacent except right at a
//! backtrack -- for the `d=1` case (`n <= 12`, a bare 12-cycle) this
//! numbering traces the entire cycle as a genuine Hamiltonian path, so
//! `find_hamiltonian_path` can realize the identity layout with
//! *zero* `Swap`s instead of paying real routing distance to reach a
//! numbering-artifact-driven detour.
//!
//! This still isn't a claim about any *specific* chip's exact physical
//! qubit numbering (real devices retire/reroute around individual bad
//! qubits, and IBM's own numbering for a given processor is its own
//! published layout, not derived from this generator) -- it's the
//! actual heavy-hex *topology family*, which is the part that matters
//! for routing correctness.
//!
//! # P1.3 (this module's square-grid generator)
//!
//! `Rigetti` now routes against a real square lattice
//! ([`CouplingMap::square_grid_for`]/[`CouplingMap::square_grid`]),
//! not the `linear` stand-in this module used to fall back on for it.
//! Rigetti's current Ankaa-class processors (Ankaa-2, Ankaa-3) are
//! published as a plain rectangular grid of qubits with tunable
//! couplers -- interior qubits have four-fold connectivity, edge
//! qubits three, corners two -- *not* the square-octagonal unit cell
//! Rigetti's earlier Aspen generation used. [`CouplingMap::square_grid`]
//! builds that grid exactly; [`CouplingMap::square_grid_for`] finds the
//! smallest square grid with at least `n` qubits and takes a DFS-order
//! prefix of exactly `n`, for the same reason [`CouplingMap::heavy_hex_for`]
//! does: a depth-first prefix of a connected graph is always connected
//! -- and, per this module's doc comment above on why heavy-hex made
//! the same switch, DFS keeps far more consecutive-index pairs
//! graph-adjacent than BFS did, which is what actually matters for
//! `route.rs`'s identity-biased layout search.
//!
//! As with heavy-hex, this is the real *topology family*, not a claim
//! about any specific chip's own published qubit numbering.

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct CouplingMap {
    num_qubits: usize,
    // Undirected edges, always stored with the smaller index first.
    edges: HashSet<(usize, usize)>,
}

impl CouplingMap {
    /// A nearest-neighbor chain: qubit `q` is adjacent to `q + 1` only.
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// let map = CouplingMap::linear(4);
    /// assert_eq!(map.num_qubits(), 4);
    /// // Each qubit is only ever adjacent to its immediate neighbor.
    /// assert!(map.is_adjacent(0, 1));
    /// assert!(!map.is_adjacent(0, 2));
    /// ```
    pub fn linear(num_qubits: usize) -> Self {
        let mut edges = HashSet::new();
        for q in 0..num_qubits.saturating_sub(1) {
            edges.insert((q, q + 1));
        }
        Self { num_qubits, edges }
    }

    /// The number of physical qubits in this coupling map.
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// let map = CouplingMap::linear(5);
    /// assert_eq!(map.num_qubits(), 5);
    /// ```
    pub fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    /// Builds a coupling map directly from a list of undirected edges
    /// over `num_qubits` physical qubits -- e.g. a real backend's own
    /// published or live-queried connectivity (see `submit_ibm.py`'s
    /// `--dump-coupling-map` and `crate::backend::lower_with_coupling`),
    /// instead of one of this module's synthetic topology generators
    /// above. `ibm_export.rs`'s own module doc flags exactly why this
    /// constructor exists: a circuit routed against a synthetic map's
    /// edges has no guarantee of matching a *specific* real chip's
    /// actual wiring (disabled qubits, chip-specific layout), so a
    /// two-qubit gate placed against the synthetic map can land on a
    /// pair that isn't coupled on the real device at all.
    ///
    /// Edges may be given in either direction and may repeat; both are
    /// normalized to this struct's own smaller-index-first storage
    /// convention. Returns `Err` for a self-loop (`a == b`, never a
    /// real coupling edge) or an endpoint `>= num_qubits` -- a real
    /// device's own edge list should never contain either, and
    /// silently dropping a malformed edge here would hide a bug in
    /// whatever produced the list (e.g. a stale qubit-count) instead of
    /// surfacing it at load time, before it can cause a confusing
    /// routing failure downstream.
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// // A small "plus"-shaped topology on 5 qubits: 0-1, 0-2, 0-3, 0-4.
    /// let map = CouplingMap::from_edges(5, [(0, 1), (0, 2), (0, 3), (0, 4)]).unwrap();
    /// assert_eq!(map.num_qubits(), 5);
    /// assert!(map.is_adjacent(0, 3));
    /// assert!(!map.is_adjacent(1, 2));
    ///
    /// // Edge direction and repetition are normalized away.
    /// let same = CouplingMap::from_edges(5, [(1, 0), (0, 2), (2, 0), (0, 3), (3, 0), (0, 4)]).unwrap();
    /// assert_eq!(map.neighbors(0), same.neighbors(0));
    ///
    /// // Malformed edges are rejected rather than silently dropped.
    /// assert!(CouplingMap::from_edges(3, [(0, 0)]).is_err());
    /// assert!(CouplingMap::from_edges(3, [(0, 3)]).is_err());
    /// ```
    pub fn from_edges(
        num_qubits: usize,
        edges: impl IntoIterator<Item = (usize, usize)>,
    ) -> Result<Self, String> {
        let mut set = HashSet::new();
        for (a, b) in edges {
            if a == b {
                return Err(format!(
                    "CouplingMap::from_edges: self-loop at qubit {} is not a valid coupling edge",
                    a
                ));
            }
            if a >= num_qubits || b >= num_qubits {
                return Err(format!(
                    "CouplingMap::from_edges: edge ({}, {}) references a qubit index \
                     >= num_qubits ({})",
                    a, b, num_qubits
                ));
            }
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            set.insert((lo, hi));
        }
        Ok(Self {
            num_qubits,
            edges: set,
        })
    }

    /// The real heavy-hex lattice for an `m`-row, `n`-column grid of
    /// hexagons (`m, n >= 1`): a hexagonal lattice of "data" qubits
    /// with one extra "flag" qubit subdividing every edge. See this
    /// module's doc comment for the construction and its verification
    /// against the published "12 qubits per hexagon" figure at `m=n=1`.
    ///
    /// Node numbering is a deterministic DFS order from a fixed corner
    /// of the lattice (see [`heavy_hex_dfs_map`]), so it's stable
    /// across calls but not meant to line up with any real device's
    /// own published qubit numbering (see this module's doc comment,
    /// including why DFS rather than BFS).
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
        heavy_hex_dfs_map(&heavy_edges, HeavyHexNode::Data(0, 0), None)
    }

    /// The smallest heavy-hex lattice with at least `num_qubits`
    /// qubits, truncated to exactly `num_qubits` by taking a
    /// depth-first prefix from a fixed corner -- guaranteed connected
    /// (a DFS prefix of a connected graph always is: every non-root
    /// node in the prefix was discovered through an edge to an
    /// already-included, earlier node), and -- unlike the BFS-order
    /// prefix this used to take -- keeps consecutive physical indices
    /// graph-adjacent almost everywhere, not just at the root (see
    /// this module's doc comment). This is what
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
                let mut cm = heavy_hex_dfs_map(
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

    /// The real square-lattice topology for a `rows`-by-`cols`
    /// rectangular grid of qubits (`rows, cols >= 1`): qubit `(r, c)`
    /// is adjacent to `(r+1, c)` and `(r, c+1)` wherever those
    /// coordinates exist, giving every interior qubit 4 neighbors, edge
    /// qubits 3, and corner qubits 2. This is the actual published
    /// topology family Rigetti's current-generation Ankaa-class
    /// superconducting processors (Ankaa-2, Ankaa-3) use -- see this
    /// module's doc comment.
    ///
    /// Node numbering is a deterministic DFS order from a fixed corner
    /// of the grid (see [`square_grid_dfs_map`]), so it's stable across
    /// calls but not meant to line up with any real device's own
    /// published qubit numbering (see this module's doc comment,
    /// including why DFS rather than BFS).
    ///
    /// # Panics
    /// If `rows == 0` or `cols == 0` -- there is no such thing as a
    /// 0-row or 0-column grid; use [`CouplingMap::linear`] (or an empty
    /// map) for a topology-free 0/1-qubit case instead.
    pub fn square_grid(rows: usize, cols: usize) -> Self {
        assert!(
            rows >= 1 && cols >= 1,
            "square_grid requires at least 1 row and 1 column, got {}x{}",
            rows,
            cols
        );
        let edges = square_grid_edges(rows, cols);
        square_grid_dfs_map(&edges, (0, 0), None)
    }

    /// The smallest square grid with at least `num_qubits` qubits,
    /// truncated to exactly `num_qubits` by taking a depth-first
    /// prefix from a fixed corner -- guaranteed connected, for the same
    /// reason [`CouplingMap::heavy_hex_for`] is (a DFS prefix of a
    /// connected graph always is), and with the same consecutive-index
    /// adjacency benefit described in this module's doc comment. This
    /// is what [`crate::backend::Backend::coupling_map`] uses for
    /// `Rigetti`.
    ///
    /// `num_qubits <= 1` returns a topology-free map, matching
    /// [`CouplingMap::linear`]/[`CouplingMap::heavy_hex_for`]'s
    /// behavior at the same sizes.
    pub fn square_grid_for(num_qubits: usize) -> Self {
        if num_qubits <= 1 {
            return Self {
                num_qubits,
                edges: HashSet::new(),
            };
        }
        let mut side = 1usize;
        loop {
            let total = side * side;
            if total >= num_qubits {
                let edges = square_grid_edges(side, side);
                let mut cm = square_grid_dfs_map(&edges, (0, 0), Some(num_qubits));
                debug_assert_eq!(
                    cm.num_qubits, num_qubits,
                    "a side x side square grid with >= num_qubits total qubits is always \
                     connected, so a BFS prefix should always reach exactly num_qubits"
                );
                cm.num_qubits = num_qubits;
                return cm;
            }
            side += 1;
        }
    }

    /// Whether two physical qubits are directly coupling-adjacent.
    /// Adjacency is symmetric: `is_adjacent(a, b)` equals `is_adjacent(b, a)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// let map = CouplingMap::linear(4);
    /// assert!(map.is_adjacent(1, 2));
    /// assert!(map.is_adjacent(2, 1));
    /// assert!(!map.is_adjacent(1, 3));
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// let map = CouplingMap::linear(4);
    /// // Qubit 1 sits between qubits 0 and 2 on a chain.
    /// assert_eq!(map.neighbors(1), vec![0, 2]);
    /// // Endpoints of the chain have a single neighbor.
    /// assert_eq!(map.neighbors(0), vec![1]);
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use sirraya_qutub_transpiler::CouplingMap;
    ///
    /// let map = CouplingMap::linear(5);
    /// assert_eq!(map.shortest_path(0, 3), Some(vec![0, 1, 2, 3]));
    /// assert_eq!(map.shortest_path(2, 2), Some(vec![2]));
    ///
    /// // Disconnected maps return None between separate components.
    /// let disconnected = CouplingMap::from_edges(4, [(0, 1), (2, 3)]).unwrap();
    /// assert_eq!(disconnected.shortest_path(0, 3), None);
    /// ```
    pub fn shortest_path(&self, start: usize, goal: usize) -> Option<Vec<usize>> {
        if start == goal {
            return Some(vec![start]);
        }

        // NOTE: deliberately calls `self.neighbors(current)` (which
        // sorts before returning) rather than building its own
        // adjacency list by iterating `self.edges` directly. `edges`
        // is a `HashSet<(usize, usize)>`, and `HashSet`'s default
        // hasher is randomly seeded per process -- iterating it (the
        // old code here did, via `for &(a, b) in &self.edges`) put
        // each node's neighbors in a different order on every run,
        // even for the identical `CouplingMap`. That doesn't change
        // *whether* a shortest path is found, but it silently changes
        // *which* shortest path wins whenever more than one exists at
        // the same length (any branching point, e.g. a heavy-hex
        // degree-3 node) -- BFS's first-discovered predecessor is the
        // one that gets kept, so a reordered neighbor list picks a
        // different, still-shortest, but different path. `route.rs`'s
        // naive `route` calls this directly for every non-adjacent
        // two-qubit gate, so that nondeterminism propagated straight
        // into its SWAP count on any topology with real branching.
        let mut visited = vec![false; self.num_qubits];
        let mut predecessor = vec![usize::MAX; self.num_qubits];
        let mut queue = VecDeque::new();
        queue.push_back(start);
        visited[start] = true;

        while let Some(current) = queue.pop_front() {
            if current == goal {
                break;
            }
            for next in self.neighbors(current) {
                if !visited[next] {
                    visited[next] = true;
                    predecessor[next] = current;
                    queue.push_back(next);
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

/// Assigns integer qubit indices to a heavy-hex graph via DFS from
/// `start`, and builds the resulting [`CouplingMap`]. If `limit` is
/// `Some(n)`, only the first `n` DFS-visited nodes get an index (and
/// only edges between two indexed nodes survive) -- see
/// [`CouplingMap::heavy_hex_for`]'s doc comment for why that prefix is
/// always connected, and this module's doc comment for why DFS is used
/// here instead of BFS (consecutive indices stay graph-adjacent almost
/// everywhere, not just at the root).
///
/// Iterative (explicit stack), not recursive: this graph is small in
/// practice, but there's no principled bound on `limit` that would
/// make a fixed recursion depth safe in general. Neighbors of each
/// node are pushed in descending sorted order so the *smallest*
/// unvisited neighbor is popped (and thus visited) next -- same
/// left-to-right, deterministic visitation order the old BFS version
/// had via its sorted adjacency lists.
fn heavy_hex_dfs_map(
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
    let mut stack: Vec<HeavyHexNode> = vec![start];
    visited.insert(start);
    while let Some(node) = stack.pop() {
        if let Some(lim) = limit {
            if order.len() >= lim {
                break;
            }
        }
        order.push(node);
        if let Some(neighbors) = adjacency.get(&node) {
            for &next in neighbors.iter().rev() {
                if visited.insert(next) {
                    stack.push(next);
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

// ---------------------------------------------------------------------
// Square-grid construction internals.
//
// Much simpler than the heavy-hex case above: a plain rectangular grid
// graph needs no edge-subdivision step, and its nodes are already a
// single flat coordinate type (no data-vs-flag distinction), so
// `square_grid_edges`/`square_grid_bfs_map` mirror
// `hexagonal_lattice_edges`/`heavy_hex_bfs_map`'s structure without
// needing an enum for the node type.
// ---------------------------------------------------------------------

/// A node in the square-grid graph, addressed by its `(row, col)`
/// coordinate.
type GridCoord = (usize, usize);

fn square_grid_edges(rows: usize, cols: usize) -> Vec<(GridCoord, GridCoord)> {
    let mut edges: HashSet<(GridCoord, GridCoord)> = HashSet::new();
    for r in 0..rows {
        for c in 0..cols {
            if r + 1 < rows {
                edges.insert(((r, c), (r + 1, c)));
            }
            if c + 1 < cols {
                edges.insert(((r, c), (r, c + 1)));
            }
        }
    }
    let mut out: Vec<(GridCoord, GridCoord)> = edges.into_iter().collect();
    out.sort();
    out
}

/// Assigns integer qubit indices to a square-grid graph via DFS from
/// `start`, and builds the resulting [`CouplingMap`]. If `limit` is
/// `Some(n)`, only the first `n` DFS-visited nodes get an index (and
/// only edges between two indexed nodes survive) -- see
/// [`CouplingMap::square_grid_for`]'s doc comment for why that prefix
/// is always connected. Mirrors [`heavy_hex_dfs_map`] exactly, just
/// over `GridCoord` instead of `HeavyHexNode` -- see that function's
/// doc comment for why DFS is used instead of the BFS this used to be.
fn square_grid_dfs_map(
    edges: &[(GridCoord, GridCoord)],
    start: GridCoord,
    limit: Option<usize>,
) -> CouplingMap {
    let mut adjacency: HashMap<GridCoord, Vec<GridCoord>> = HashMap::new();
    for &(a, b) in edges {
        adjacency.entry(a).or_default().push(b);
        adjacency.entry(b).or_default().push(a);
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort();
    }

    let mut order: Vec<GridCoord> = Vec::new();
    let mut visited: HashSet<GridCoord> = HashSet::new();
    let mut stack: Vec<GridCoord> = vec![start];
    visited.insert(start);
    while let Some(node) = stack.pop() {
        if let Some(lim) = limit {
            if order.len() >= lim {
                break;
            }
        }
        order.push(node);
        if let Some(neighbors) = adjacency.get(&node) {
            for &next in neighbors.iter().rev() {
                if visited.insert(next) {
                    stack.push(next);
                }
            }
        }
    }

    let index: HashMap<GridCoord, usize> =
        order.iter().enumerate().map(|(i, &n)| (n, i)).collect();

    let mut cm_edges: HashSet<(usize, usize)> = HashSet::new();
    for &(a, b) in edges {
        if let (Some(&ia), Some(&ib)) = (index.get(&a), index.get(&b)) {
            cm_edges.insert(if ia < ib { (ia, ib) } else { (ib, ia) });
        }
    }

    CouplingMap {
        num_qubits: order.len(),
        edges: cm_edges,
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

    /// A 1x1 square grid is a single, edgeless qubit -- there's no
    /// second qubit to be adjacent to.
    #[test]
    fn square_grid_1x1_has_a_single_isolated_qubit() {
        let map = CouplingMap::square_grid(1, 1);
        assert_eq!(map.num_qubits(), 1);
        assert!(map.neighbors(0).is_empty());
    }

    /// A 2x3 square grid should have exactly 6 qubits, each with the
    /// degree its position dictates: corners 2, edges 3, interior 4.
    /// A 2-row grid has no interior cells, so this only pins down
    /// corner/edge degrees; `square_grid_has_an_interior_degree_four_qubit`
    /// below covers the interior case on a bigger grid.
    #[test]
    fn square_grid_2x3_matches_expected_degrees() {
        let map = CouplingMap::square_grid(2, 3);
        assert_eq!(map.num_qubits(), 6);
        let degree = |q: usize| {
            (0..6).filter(|&other| other != q && map.is_adjacent(q, other)).count()
        };
        let degrees: Vec<usize> = (0..6).map(degree).collect();
        let mut sorted = degrees.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![2, 2, 2, 2, 3, 3],
            "a 2x3 grid should have 4 corners (degree 2) and 2 edge-midpoints (degree 3): {:?}",
            degrees
        );
    }

    #[test]
    fn from_edges_builds_a_map_matching_a_hand_checked_topology() {
        // A tiny "T" shape: 0-1, 1-2, 1-3. Qubit 1 should be the only
        // degree-3 node.
        let map = CouplingMap::from_edges(4, [(0, 1), (1, 2), (1, 3)]).unwrap();
        assert_eq!(map.num_qubits(), 4);
        assert!(map.is_adjacent(0, 1));
        assert!(map.is_adjacent(1, 2));
        assert!(map.is_adjacent(1, 3));
        assert!(!map.is_adjacent(0, 2));
        assert!(!map.is_adjacent(2, 3));
        let degree = |q: usize| (0..4).filter(|&o| o != q && map.is_adjacent(q, o)).count();
        assert_eq!(degree(1), 3);
        assert_eq!(degree(0), 1);
    }

    #[test]
    fn from_edges_is_direction_and_duplicate_insensitive() {
        // (2,0) and a repeated (0,2) should collapse to the same single
        // edge as (0,2), matching this struct's own storage convention.
        let a = CouplingMap::from_edges(3, [(0, 2)]).unwrap();
        let b = CouplingMap::from_edges(3, [(2, 0), (0, 2)]).unwrap();
        assert!(a.is_adjacent(0, 2) && b.is_adjacent(0, 2));
        assert_eq!(a.neighbors(0), b.neighbors(0));
    }

    #[test]
    fn from_edges_rejects_self_loop() {
        assert!(CouplingMap::from_edges(3, [(1, 1)]).is_err());
    }

    #[test]
    fn from_edges_rejects_out_of_range_endpoint() {
        assert!(CouplingMap::from_edges(3, [(0, 5)]).is_err());
    }

    /// The actual point of this constructor: a real device's own
    /// (possibly irregular -- some qubits disabled, non-uniform
    /// degree) edge list should route exactly as told, not get
    /// "corrected" toward a regular topology the way the synthetic
    /// generators above are.
    #[test]
    fn from_edges_supports_irregular_real_device_style_topologies() {
        // A star with one disabled spoke (no edge to qubit 4 at all).
        let map = CouplingMap::from_edges(5, [(0, 1), (0, 2), (0, 3)]).unwrap();
        assert_eq!(map.neighbors(4).len(), 0, "qubit 4 has no edges, same as a disabled qubit");
        assert_eq!(map.neighbors(0).len(), 3);
    }

    /// No qubit in a square grid should have more than 4 neighbors, and
    /// a big-enough grid should actually have an interior qubit that
    /// reaches that maximum.
    #[test]
    fn square_grid_has_an_interior_degree_four_qubit() {
        let map = CouplingMap::square_grid(3, 3);
        let mut saw_degree_four = false;
        for q in 0..map.num_qubits() {
            let degree = (0..map.num_qubits())
                .filter(|&other| other != q && map.is_adjacent(q, other))
                .count();
            assert!(degree <= 4, "qubit {} has degree {} > 4", q, degree);
            if degree == 4 {
                saw_degree_four = true;
            }
        }
        assert!(saw_degree_four, "a 3x3 grid should have at least one interior degree-4 qubit");
    }

    #[test]
    fn square_grid_is_fully_connected() {
        let map = CouplingMap::square_grid(3, 4);
        for target in 1..map.num_qubits() {
            assert!(
                map.shortest_path(0, target).is_some(),
                "qubit {} should be reachable from qubit 0 in a 3x4 square grid",
                target
            );
        }
    }

    #[test]
    #[should_panic]
    fn square_grid_rejects_zero_rows_or_cols() {
        CouplingMap::square_grid(0, 3);
    }

    /// `square_grid_for` must return *exactly* the requested qubit
    /// count (not just "at least"), for a range of sizes including
    /// ones that don't land exactly on a side x side grid's natural
    /// qubit count.
    #[test]
    fn square_grid_for_returns_exact_requested_size() {
        for n in [0, 1, 2, 5, 9, 10, 16, 50, 77] {
            let map = CouplingMap::square_grid_for(n);
            assert_eq!(map.num_qubits(), n, "square_grid_for({}) returned the wrong qubit count", n);
        }
    }

    /// The whole point of building the truncated map via a BFS prefix:
    /// it must stay connected even when it cuts a real square grid off
    /// partway through, for sizes both smaller and larger than one bare
    /// side x side grid.
    #[test]
    fn square_grid_for_is_always_connected() {
        for n in [2, 5, 9, 10, 16, 50, 77] {
            let map = CouplingMap::square_grid_for(n);
            for target in 1..n {
                assert!(
                    map.shortest_path(0, target).is_some(),
                    "square_grid_for({}): qubit {} unreachable from qubit 0",
                    n,
                    target
                );
            }
        }
    }

    #[test]
    fn square_grid_for_never_exceeds_degree_four() {
        for n in [9, 16, 50, 77] {
            let map = CouplingMap::square_grid_for(n);
            for q in 0..n {
                let degree = (0..n).filter(|&other| other != q && map.is_adjacent(q, other)).count();
                assert!(degree <= 4, "square_grid_for({}): qubit {} has degree {} > 4", n, q, degree);
            }
        }
    }

    /// The actual reason for the BFS -> DFS switch (see this module's
    /// doc comment): `route.rs`'s `find_hamiltonian_path` needs a
    /// physical path close to the identity mapping to route a chain
    /// circuit (e.g. GHZ-state prep) with few or zero `Swap`s. Under
    /// the old BFS numbering this was nearly unreachable in practice
    /// (empirically ~1 real edge out of every `n-1` consecutive-index
    /// pairs); this pins down that DFS numbering restores it for a
    /// spread of sizes spanning both a single hexagon (`n <= 12`, a
    /// bare cycle -- should be a *perfect* Hamiltonian path, 100%) and
    /// a multi-hexagon grid with real degree-3 branch points (`n > 12`
    /// -- allow the rare backtrack, but require the identity path to
    /// still be almost entirely intact).
    #[test]
    fn heavy_hex_for_dfs_numbering_keeps_the_identity_mapping_nearly_a_hamiltonian_path() {
        for &n in &[5usize, 10, 12, 13, 16, 25, 50, 77] {
            let map = CouplingMap::heavy_hex_for(n);
            let present = (0..n - 1).filter(|&i| map.is_adjacent(i, i + 1)).count();
            let total = n - 1;
            if n <= 12 {
                assert_eq!(
                    present, total,
                    "heavy_hex_for({}) is a bare cycle (see this module's doc comment) -- \
                     DFS numbering should trace it as a perfect Hamiltonian path, but only \
                     {}/{} consecutive-index pairs are adjacent",
                    n, present, total
                );
            } else {
                assert!(
                    present * 10 >= total * 9,
                    "heavy_hex_for({}): DFS numbering should keep at least 90% of \
                     consecutive-index pairs graph-adjacent (only backtracking at real \
                     degree-3 branch points), got {}/{}",
                    n, present, total
                );
            }
        }
    }

    /// Same check as the heavy-hex one above, for the square-grid
    /// generator's DFS numbering.
    #[test]
    fn square_grid_for_dfs_numbering_keeps_the_identity_mapping_nearly_a_hamiltonian_path() {
        for &n in &[5usize, 9, 10, 16, 25, 50, 77] {
            let map = CouplingMap::square_grid_for(n);
            let present = (0..n - 1).filter(|&i| map.is_adjacent(i, i + 1)).count();
            let total = n - 1;
            assert!(
                present * 10 >= total * 8,
                "square_grid_for({}): DFS numbering should keep at least 80% of \
                 consecutive-index pairs graph-adjacent, got {}/{}",
                n, present, total
            );
        }
    }

    #[test]
    fn neighbors_matches_is_adjacent_on_a_square_grid_map() {
        let map = CouplingMap::square_grid(3, 4);
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