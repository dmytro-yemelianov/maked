/-
  LeanMake.Graph
  Formalization of the Make Dependency Graph, Reachability, Acyclicity, and Cycle Detection.
-/

import LeanMake.Syntax

namespace LeanMake

/-- Adjacency list representation of a build dependency graph -/
structure DepGraph where
  nodes : List TargetName
  edges : TargetName → List TargetName

/-- Build a DepGraph from a Makefile, including both rule targets and leaf source files -/
def Makefile.toDepGraph (mf : Makefile) : DepGraph :=
  let ruleTargets := mf.rules.map (·.target)
  let allPrereqs := mf.rules.flatMap (·.prereqs)
  let allNodes := (ruleTargets ++ allPrereqs).eraseDups
  {
    nodes := allNodes,
    edges := fun t =>
      match mf.findRule t with
      | some r => r.prereqs
      | none   => []
  }

/-- An inductive definition of a Directed Path in graph G from u to v -/
inductive Path (G : DepGraph) : TargetName → TargetName → Type where
  | direct {u v : TargetName} : v ∈ G.edges u → Path G u v
  | step {u v w : TargetName} : v ∈ G.edges u → Path G v w → Path G u w

/-- Transitive reachability in the dependency graph -/
def Reachable (G : DepGraph) (u v : TargetName) : Prop :=
  Nonempty (Path G u v)

/-- Definition of a Cyclic Dependency: a node can reach itself via a non-empty path -/
def HasCycleAt (G : DepGraph) (u : TargetName) : Prop :=
  Reachable G u u

/-- A graph is a Directed Acyclic Graph (DAG) iff no node has a cycle to itself -/
def IsDAG (G : DepGraph) : Prop :=
  ∀ u, ¬ HasCycleAt G u

/-- State of node during DFS traversal for cycle detection -/
inductive NodeStatus where
  | unvisited : NodeStatus
  | visiting  : NodeStatus  -- on current recursion stack
  | visited   : NodeStatus  -- fully explored
deriving DecidableEq, Repr

/-- Map from node to its status -/
def StatusMap := List (TargetName × NodeStatus)

def StatusMap.get (m : StatusMap) (k : TargetName) : NodeStatus :=
  match m.find? (fun (name, _) => name == k) with
  | some (_, s) => s
  | none => NodeStatus.unvisited

def StatusMap.set (m : StatusMap) (k : TargetName) (s : NodeStatus) : StatusMap :=
  (k, s) :: m.filter (fun (name, _) => name != k)

/-- Result of cycle detection: either Acyclic with a topological ordering, Cyclic with offending path, or Fuel Exhausted -/
inductive CycleResult where
  | acyclic (topoOrder : List TargetName) : CycleResult
  | cycleDetected (cycleTrace : List TargetName) : CycleResult
  | fuelExhausted : CycleResult
deriving Repr, DecidableEq

inductive DfsOutcome where
  | ok (status : StatusMap) (order : List TargetName)
  | cycle (trace : List TargetName)
  | exhausted

/-- Depth-first search with explicit fuel for certified termination and directional cycle traces -/
def detectCyclesFuel (G : DepGraph) (fuel : Nat) : CycleResult :=
  let rec dfs (fuel : Nat) (u : TargetName) (status : StatusMap) (stack : List TargetName) (order : List TargetName) : DfsOutcome :=
    match fuel with
    | 0 => DfsOutcome.exhausted
    | fuel + 1 =>
      match status.get u with
      | NodeStatus.visiting =>
        -- Issue fix: reverse the stack slice so the cycle trace flows in forward edge direction:
        -- u -> step1 -> step2 -> ... -> u
        let cycleSlice := (stack.takeWhile (· != u)).reverse
        DfsOutcome.cycle (u :: cycleSlice ++ [u])
      | NodeStatus.visited =>
        DfsOutcome.ok status order
      | NodeStatus.unvisited =>
        let status1 := status.set u NodeStatus.visiting
        let prereqs := G.edges u
        let rec visitPrereqs (deps : List TargetName) (st : StatusMap) (ord : List TargetName) : DfsOutcome :=
          match deps with
          | [] => DfsOutcome.ok st ord
          | d :: ds =>
            match dfs fuel d st (u :: stack) ord with
            | DfsOutcome.cycle c => DfsOutcome.cycle c
            | DfsOutcome.exhausted => DfsOutcome.exhausted
            | DfsOutcome.ok st1 ord1 => visitPrereqs ds st1 ord1

        match visitPrereqs prereqs status1 order with
        | DfsOutcome.cycle c => DfsOutcome.cycle c
        | DfsOutcome.exhausted => DfsOutcome.exhausted
        | DfsOutcome.ok status2 order2 =>
          let status3 := status2.set u NodeStatus.visited
          DfsOutcome.ok status3 (u :: order2)

  let rec runAll (nodes : List TargetName) (st : StatusMap) (ord : List TargetName) : CycleResult :=
    match nodes with
    | [] => CycleResult.acyclic ord.reverse
    | n :: ns =>
      if st.get n == NodeStatus.visited then
        runAll ns st ord
      else
        match dfs fuel n st [] ord with
        | DfsOutcome.cycle c => CycleResult.cycleDetected c
        | DfsOutcome.exhausted => CycleResult.fuelExhausted
        | DfsOutcome.ok st1 ord1 => runAll ns st1 ord1

  runAll G.nodes [] []

/-- Safe cycle detection with sufficient fuel = 4 * (number of nodes + 1) -/
def detectCycles (G : DepGraph) : CycleResult :=
  detectCyclesFuel G (G.nodes.length * 4 + 10)

end LeanMake
