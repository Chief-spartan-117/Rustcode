//! Lane layout for a commit graph: which column each commit sits in and which lines to draw.

/// A commit as the layout needs it. Commits come newest first, children before parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub parents: Vec<String>,
}

/// What to draw for one commit. Columns count from the left.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphRow {
    /// Column of the commit's dot.
    pub column: usize,
    /// Lines that run the full height of the row.
    pub through: Vec<usize>,
    /// Lines that come from the top edge and end at the dot.
    pub from_above: Vec<usize>,
    /// Lines that start at the dot and go to the bottom edge.
    pub to_below: Vec<usize>,
    /// Number of columns the row needs.
    pub width: usize,
}

/// Lay out `nodes`, one row each.
pub fn layout_graph(nodes: &[GraphNode]) -> Vec<GraphRow> {
    // For each column, the commit its line is heading for.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        let waiting: Vec<usize> =
            lanes.iter().enumerate().filter(|(_, l)| l.as_deref() == Some(node.id.as_str())).map(|(i, _)| i).collect();
        let column = match waiting.first() {
            Some(&first) => first,
            None => free_lane(&mut lanes),
        };
        let from_above = waiting.clone();
        for &i in &waiting {
            lanes[i] = None;
        }
        let through: Vec<usize> =
            lanes.iter().enumerate().filter(|(_, l)| l.is_some()).map(|(i, _)| i).collect();

        let mut to_below = Vec::new();
        for (n, parent) in node.parents.iter().enumerate() {
            let existing = lanes.iter().position(|l| l.as_deref() == Some(parent.as_str()));
            let target = match existing {
                Some(i) => i,
                None if n == 0 => column,
                None => free_lane(&mut lanes),
            };
            lanes[target] = Some(parent.clone());
            if !to_below.contains(&target) {
                to_below.push(target);
            }
        }
        let width = lanes.len().max(column + 1).max(through.iter().max().map_or(0, |m| m + 1));
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(GraphRow { column, through, from_above, to_below, width });
    }
    rows
}

/// First empty column, or a new one on the right.
fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parents: &[&str]) -> GraphNode {
        GraphNode { id: id.to_owned(), parents: parents.iter().map(|p| (*p).to_owned()).collect() }
    }

    #[test]
    fn a_straight_history_stays_in_one_column() {
        let rows = layout_graph(&[node("c", &["b"]), node("b", &["a"]), node("a", &[])]);
        assert!(rows.iter().all(|r| r.column == 0 && r.through.is_empty()));
        assert_eq!(rows[0].from_above, Vec::<usize>::new());
        assert_eq!(rows[0].to_below, vec![0]);
        assert_eq!(rows[1].from_above, vec![0]);
        assert_eq!(rows[2].to_below, Vec::<usize>::new());
    }

    #[test]
    fn a_merge_opens_a_second_lane_and_the_branch_point_closes_it() {
        // m merges b into a-line; b and a both come from base.
        let rows = layout_graph(&[node("m", &["a", "b"]), node("b", &["base"]), node("a", &["base"]), node("base", &[])]);
        assert_eq!(rows[0].to_below, vec![0, 1]);
        // b sits in lane 1 while lane 0 passes by.
        assert_eq!(rows[1].column, 1);
        assert_eq!(rows[1].through, vec![0]);
        assert_eq!(rows[2].column, 0);
        assert_eq!(rows[2].through, vec![1]);
        // a joins the lane that waits for base, so base sits in that lane.
        assert_eq!(rows[2].to_below, vec![1]);
        assert_eq!(rows[3].column, 1);
        assert_eq!(rows[3].from_above, vec![1]);
    }

    #[test]
    fn a_parent_already_in_a_lane_is_reused() {
        let rows = layout_graph(&[node("x", &["p"]), node("y", &["p"]), node("p", &[])]);
        assert_eq!(rows[0].to_below, vec![0]);
        // y is not waited for, so it takes a new lane, and both lines meet at p.
        assert_eq!(rows[1].column, 1);
        assert_eq!(rows[1].to_below, vec![0]);
        assert_eq!(rows[2].from_above, vec![0]);
    }
}
