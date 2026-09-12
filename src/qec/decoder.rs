//! A real minimum-weight perfect matching (MWPM) decoder -- the
//! decoding *strategy* every state-of-the-art graph-based QEC decoder
//! uses (Google's, IBM's, and Quantinuum's real-time surface-code
//! decoders are all built on graph matching; PyMatching, the
//! most widely used open-source implementation, is a highly optimized
//! blossom-algorithm solver for exactly this problem). This module is
//! what lets [`crate::qec`] scale past a fixed, hand-enumerated
//! syndrome table: a code's syndrome graph, once defined, is decoded
//! by real computation, not a static [`crate::ir::Gate::If`] tree that
//! would need one branch per possible syndrome (`2^(num_syndrome_bits)`
//! of them -- entirely impractical past a handful of syndrome bits,
//! let alone at the scale a real surface code needs).
//!
//! # What's real here, and what's honestly a simplification
//! The matching problem this solves -- pair up "defects" (syndrome
//! checks that fired) so the total pairwise graph-distance is
//! minimized, with each defect allowed to match another defect *or* an
//! unlimited-capacity boundary node -- is exactly the real MWPM
//! decoding problem, not an approximation of it. What *is* a real,
//! honestly-flagged limitation: [`minimum_weight_perfect_matching`]
//! solves it by exact exhaustive search over matchings rather than the
//! polynomial-time blossom algorithm (Edmonds, 1965) real production
//! decoders use. For the defect counts realistic QEC experiments and
//! this crate's own tests produce (a handful of simultaneous errors,
//! not thousands), exhaustive search is fast and, being exact, returns
//! the *identical* matching a full blossom implementation would --
//! the difference is scalability, not correctness. Implementing the
//! real polynomial-time blossom algorithm (with its own real
//! complexity -- blossom contraction, alternating trees) is genuine,
//! separate follow-on work; shipping a subtly-wrong fast
//! implementation would be worse than an honestly-slow correct one for
//! something whose entire value is getting the matching right.

use std::collections::HashSet;

/// One node in a decoding graph: either a real syndrome-check
/// location, or the boundary -- a single, unlimited-capacity node
/// representing "this defect's error chain terminates at the edge of
/// the code" (standard in MWPM decoding: an odd number of defects is
/// completely normal, since some can independently route to the
/// boundary instead of pairing with another defect).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodingNode {
    Check(usize),
    Boundary,
}

/// Computes the exact minimum-weight perfect matching over `defects`,
/// where `distance(a, b)` gives the graph-distance weight of matching
/// `a` to `b` (called with [`DecodingNode::Boundary`] for a
/// defect-to-boundary match). Returns one `(defect, partner)` pair per
/// defect matched to another defect (each such pair appears once, not
/// twice) plus one `(defect, DecodingNode::Boundary)` entry per defect
/// routed to the boundary. See this module's doc comment for what
/// "exact" means here and its real scalability limit.
///
/// Solved by recursion on "what does the first remaining defect match
/// to" (another defect, or the boundary), trying every option and
/// keeping the globally cheapest -- exponential in the number of
/// defects, exact regardless of how many there are.
pub fn minimum_weight_perfect_matching(
    defects: &[DecodingNode],
    distance: impl Fn(DecodingNode, DecodingNode) -> f64 + Copy,
) -> Vec<(DecodingNode, DecodingNode)> {
    let (_, matching) = best_matching(defects, distance);
    matching
}

fn best_matching(
    remaining: &[DecodingNode],
    distance: impl Fn(DecodingNode, DecodingNode) -> f64 + Copy,
) -> (f64, Vec<(DecodingNode, DecodingNode)>) {
    if remaining.is_empty() {
        return (0.0, Vec::new());
    }
    let first = remaining[0];
    let rest = &remaining[1..];

    // Option 1: `first` routes to the boundary alone.
    let (boundary_rest_cost, boundary_rest_matching) = best_matching(rest, distance);
    let mut best_cost = distance(first, DecodingNode::Boundary) + boundary_rest_cost;
    let mut best_pairs = boundary_rest_matching;
    best_pairs.push((first, DecodingNode::Boundary));

    // Option 2: `first` pairs with each other remaining defect in turn.
    for i in 0..rest.len() {
        let partner = rest[i];
        let mut without_partner: Vec<DecodingNode> = rest.to_vec();
        without_partner.remove(i);
        let (sub_cost, sub_matching) = best_matching(&without_partner, distance);
        let cost = distance(first, partner) + sub_cost;
        if cost < best_cost {
            best_cost = cost;
            let mut pairs = sub_matching;
            pairs.push((first, partner));
            best_pairs = pairs;
        }
    }

    (best_cost, best_pairs)
}

/// Expands a [`minimum_weight_perfect_matching`] result into the exact
/// set of qubits an error chain passes through, given `path_qubits`,
/// which must return the ordered list of physical qubits lying on the
/// graph edges between any two adjacent nodes on the shortest path
/// connecting `a` and `b` (including a path to
/// [`DecodingNode::Boundary`]). Every qubit on every matched pair's
/// path gets corrected; a qubit appearing on more than one path's
/// intersection is corrected an even number of times and so, by
/// construction (each correction is its own inverse), nets out to no
/// correction there -- exactly the right behavior when two matched
/// error chains happen to overlap.
pub fn corrections_from_matching(
    matching: &[(DecodingNode, DecodingNode)],
    path_qubits: impl Fn(DecodingNode, DecodingNode) -> Vec<usize>,
) -> Vec<usize> {
    let mut flipped: HashSet<usize> = HashSet::new();
    for &(a, b) in matching {
        for q in path_qubits(a, b) {
            if !flipped.remove(&q) {
                flipped.insert(q);
            }
        }
    }
    let mut qubits: Vec<usize> = flipped.into_iter().collect();
    qubits.sort_unstable();
    qubits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_defect_set_has_empty_matching() {
        let m = minimum_weight_perfect_matching(&[], |_, _| 0.0);
        assert!(m.is_empty());
    }

    #[test]
    fn single_defect_routes_to_boundary() {
        let defects = [DecodingNode::Check(0)];
        let m = minimum_weight_perfect_matching(&defects, |_, b| match b {
            DecodingNode::Boundary => 1.0,
            _ => 5.0,
        });
        assert_eq!(m, vec![(DecodingNode::Check(0), DecodingNode::Boundary)]);
    }

    #[test]
    fn two_close_defects_prefer_pairing_over_two_boundary_routes() {
        // distance(0,1) = 1 (cheap direct pairing); distance to
        // boundary = 10 each (expensive) -- pairing should win.
        let defects = [DecodingNode::Check(0), DecodingNode::Check(1)];
        let dist = |a: DecodingNode, b: DecodingNode| match (a, b) {
            (DecodingNode::Check(x), DecodingNode::Check(y)) => (x as f64 - y as f64).abs(),
            _ => 10.0,
        };
        let m = minimum_weight_perfect_matching(&defects, dist);
        assert_eq!(m, vec![(DecodingNode::Check(0), DecodingNode::Check(1))]);
    }

    #[test]
    fn two_far_defects_prefer_two_boundary_routes_over_pairing() {
        // distance(0,1) = 10 (expensive direct pairing); distance to
        // boundary = 1 each (cheap) -- routing both to the boundary
        // independently should win (total 2 vs. 10).
        let defects = [DecodingNode::Check(0), DecodingNode::Check(1)];
        let dist = |a: DecodingNode, b: DecodingNode| match (a, b) {
            (DecodingNode::Check(_), DecodingNode::Check(_)) => 10.0,
            _ => 1.0,
        };
        let mut m = minimum_weight_perfect_matching(&defects, dist);
        m.sort_by_key(|(node, _)| match node {
            DecodingNode::Check(c) => *c,
            DecodingNode::Boundary => usize::MAX,
        });
        assert_eq!(
            m,
            vec![
                (DecodingNode::Check(0), DecodingNode::Boundary),
                (DecodingNode::Check(1), DecodingNode::Boundary),
            ]
        );
    }

    #[test]
    fn corrections_from_matching_cancels_overlapping_paths() {
        // Two matched pairs whose paths share qubit 2 -- it should be
        // corrected zero times net (flipped twice cancels), while
        // qubits 0,1 (only on the first path) and 3,4 (only on the
        // second) are each corrected once.
        let matching = vec![
            (DecodingNode::Check(0), DecodingNode::Check(1)),
            (DecodingNode::Check(2), DecodingNode::Check(3)),
        ];
        let path_qubits = |a: DecodingNode, b: DecodingNode| -> Vec<usize> {
            match (a, b) {
                (DecodingNode::Check(0), DecodingNode::Check(1)) => vec![0, 1, 2],
                (DecodingNode::Check(2), DecodingNode::Check(3)) => vec![2, 3, 4],
                _ => vec![],
            }
        };
        let mut corrected = corrections_from_matching(&matching, path_qubits);
        corrected.sort_unstable();
        assert_eq!(corrected, vec![0, 1, 3, 4]);
    }
}
