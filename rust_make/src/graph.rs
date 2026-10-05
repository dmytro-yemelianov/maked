use crate::ast::Makefile;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeColor {
    White, // Unvisited
    Gray,  // Visiting (on stack)
    Black, // Visited
}

#[derive(Debug)]
pub struct DependencyGraph {
    pub adj: HashMap<String, Vec<String>>,
    #[allow(dead_code)]
    pub all_nodes: HashSet<String>,
}

#[derive(Debug)]
pub enum GraphError {
    CircularDependency(Vec<String>),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CircularDependency(cycle) => {
                write!(
                    f,
                    "Circular dependency detected: {}. Stop.",
                    cycle.join(" -> ")
                )
            }
        }
    }
}

impl DependencyGraph {
    pub fn from_makefile(makefile: &Makefile) -> Self {
        let mut adj = HashMap::new();
        let mut all_nodes = HashSet::new();

        // Explicit prerequisites as written: the executor resolves implicit
        // rules itself (`get_rule`), so a full rule lookup here only cost
        // time (60 ms for git's 14,800 rules). `adj` feeds the critical-path
        // report and the TUI's node count.
        for (target, rule) in &makefile.rules {
            all_nodes.insert(target.clone());
            let mut prereqs = rule.prereqs.clone();
            prereqs.extend(makefile.order_only_of(target).iter().cloned());
            for dep in &prereqs {
                all_nodes.insert(dep.clone());
            }
            adj.insert(target.clone(), prereqs);
        }

        Self { adj, all_nodes }
    }

    /// DFS-based 3-color cycle detection over explicit and pattern rules.
    pub fn check_cycles(&self, makefile: &Makefile, root: &str) -> Result<(), GraphError> {
        let mut colors: crate::fxhash::FxHashMap<String, NodeColor> = Default::default();
        let mut path: Vec<String> = Vec::new();

        self.dfs_cycle(makefile, root, &mut colors, &mut path)
    }

    fn dfs_cycle(
        &self,
        makefile: &Makefile,
        u: &str,
        colors: &mut crate::fxhash::FxHashMap<String, NodeColor>,
        path: &mut Vec<String>,
    ) -> Result<(), GraphError> {
        colors.insert(u.to_string(), NodeColor::Gray);
        path.push(u.to_string());

        let rule = makefile.get_rule(u);
        let prereqs: &[String] = match &rule {
            Some(r) => &r.prereqs,
            None => self.adj.get(u).map(Vec::as_slice).unwrap_or(&[]),
        };

        for v in prereqs.iter().chain(makefile.order_only_of(u)) {
            let color = colors.get(v).copied().unwrap_or(NodeColor::White);
            match color {
                NodeColor::Gray => {
                    let mut cycle = Vec::new();
                    if let Some(pos) = path.iter().position(|x| x == v) {
                        cycle.extend_from_slice(&path[pos..]);
                    }
                    cycle.push(v.clone());
                    return Err(GraphError::CircularDependency(cycle));
                }
                NodeColor::White => {
                    self.dfs_cycle(makefile, v, colors, path)?;
                }
                NodeColor::Black => {}
            }
        }

        path.pop();
        if let Some(c) = colors.get_mut(u) {
            *c = NodeColor::Black;
        }
        Ok(())
    }

    /// Collect all nodes reachable from `root` in the dependency graph
    pub fn reachable_subgraph(
        &self,
        makefile: &Makefile,
        root: &str,
    ) -> crate::fxhash::FxHashSet<String> {
        let mut visited = crate::fxhash::FxHashSet::default();
        let mut stack = vec![root.to_string()];

        while let Some(node) = stack.pop() {
            if visited.insert(node.clone()) {
                let rule = makefile.get_rule(&node);
                let prereqs: &[String] = match &rule {
                    Some(r) => &r.prereqs,
                    None => self.adj.get(&node).map(Vec::as_slice).unwrap_or(&[]),
                };
                for dep in prereqs.iter().chain(makefile.order_only_of(&node)) {
                    if !visited.contains(dep) {
                        stack.push(dep.clone());
                    }
                }
            }
        }

        visited
    }
}
