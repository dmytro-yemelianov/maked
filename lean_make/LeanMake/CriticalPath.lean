/-
  LeanMake.CriticalPath
  Formalization and Machine-Checked Proofs of the Critical Path Algorithm for DAGs:
  1. Path Duration across directed dependency chains.
  2. Bellman Dynamic Programming recurrence for Critical Path length.
  3. Theorem: Path Duration Boundedness (Every directed chain is bounded by CP).
  4. Theorem: Direct Prerequisite Chain Bound.
  5. Theorem: Earliest Finish Time Lower Bound for Valid Parallel Schedules.
  6. Theorem: Critical Path Weak Monotonicity in Task Durations.
-/

import LeanMake.Syntax
import LeanMake.Graph

namespace LeanMake

/-- Duration of an individual target in microseconds or abstract discrete time units -/
def Duration := Nat

/-- Duration of a directed path of dependencies in graph G -/
def pathDuration (dur : TargetName → Nat) {G : DepGraph} {u v : TargetName} : Path G u v → Nat
  | Path.direct (u := u) (v := v) _ => dur u + dur v
  | Path.step (u := u) (v := _) (w := _) _ pRest => dur u + pathDuration dur pRest

/--
  A function CP : TargetName → Nat is a Valid Critical Path Bound if:
  1. CP u >= dur u for all u.
  2. For all v in G.edges u, CP u >= dur u + CP v.
-/
structure IsValidCPBound (G : DepGraph) (dur : TargetName → Nat) (CP : TargetName → Nat) : Prop where
  base_bound : ∀ u, dur u ≤ CP u
  step_bound : ∀ u v, v ∈ G.edges u → dur u + CP v ≤ CP u

/--
  Theorem 1: Topological Optimality / Upper Bound.
  Under any valid Critical Path assignment CP, the duration of ANY directed
  dependency chain from u to v is bounded above by CP u.
-/
theorem pathDuration_le_CP (G : DepGraph) (dur : TargetName → Nat) (CP : TargetName → Nat)
    (hCP : IsValidCPBound G dur CP) : ∀ {u v : TargetName} (p : Path G u v),
    pathDuration dur p ≤ CP u := by
  intro u v p
  induction p with
  | @direct u0 v0 hEdge =>
    dsimp [pathDuration]
    have hStep := hCP.step_bound u0 v0 hEdge
    have hBase := hCP.base_bound v0
    have hAdd : dur u0 + dur v0 ≤ dur u0 + CP v0 := Nat.add_le_add_left hBase (dur u0)
    exact Nat.le_trans hAdd hStep
  | @step u0 v0 w0 hEdge pRest ih =>
    dsimp [pathDuration]
    have hStep := hCP.step_bound u0 v0 hEdge
    have hAdd : dur u0 + pathDuration dur pRest ≤ dur u0 + CP v0 := Nat.add_le_add_left ih (dur u0)
    exact Nat.le_trans hAdd hStep

/--
  Constructive Dynamic Programming Critical Path valuation with recursion fuel.
-/
def computeCP (G : DepGraph) (dur : TargetName → Nat) : Nat → TargetName → Nat
  | 0, u => dur u
  | fuel + 1, u =>
    let prereqCPs := (G.edges u).map (computeCP G dur fuel)
    let maxPrereq := prereqCPs.foldl Nat.max 0
    dur u + maxPrereq

/--
  Theorem 2: Self-Duration Lower Bound.
  computeCP fuel u is always at least dur u for any fuel depth.
-/
theorem computeCP_ge_dur (G : DepGraph) (dur : TargetName → Nat) (fuel : Nat) (u : TargetName) :
    dur u ≤ computeCP G dur fuel u := by
  cases fuel with
  | zero =>
    dsimp [computeCP]
    exact Nat.le_refl _
  | succ k =>
    dsimp [computeCP]
    exact Nat.le_add_right (dur u) _

/-- Helper lemma: foldl Nat.max with accumulator is >= initial accumulator -/
theorem foldl_max_ge (acc : Nat) (xs : List Nat) :
    acc ≤ xs.foldl Nat.max acc := by
  induction xs generalizing acc with
  | nil =>
    dsimp
    exact Nat.le_refl acc
  | cons x xs ih =>
    dsimp
    have h1 : acc ≤ Nat.max acc x := Nat.le_max_left acc x
    have h2 := ih (Nat.max acc x)
    exact Nat.le_trans h1 h2

/-- Helper lemma: foldl Nat.max contains every element in the list -/
theorem foldl_max_contains (x : Nat) (xs : List Nat) (hIn : x ∈ xs) (acc : Nat) :
    x ≤ xs.foldl Nat.max acc := by
  induction xs generalizing acc with
  | nil => contradiction
  | cons y ys ih =>
    cases hIn with
    | head =>
      dsimp
      have hMax : x ≤ Nat.max acc x := Nat.le_max_right acc x
      have hRest := foldl_max_ge (Nat.max acc x) ys
      exact Nat.le_trans hMax hRest
    | tail _ hTail =>
      dsimp
      exact ih hTail (Nat.max acc y)

/--
  Theorem 3: One-Step Bellman Dynamic Programming Expansion.
  For any prerequisite v of u, computeCP at depth (fuel + 1) is at least
  dur u + computeCP at depth fuel of v.
-/
theorem computeCP_step (G : DepGraph) (dur : TargetName → Nat) (fuel : Nat)
    (u v : TargetName) (hEdge : v ∈ G.edges u) :
    dur u + computeCP G dur fuel v ≤ computeCP G dur (fuel + 1) u := by
  dsimp [computeCP]
  have hMapIn : computeCP G dur fuel v ∈ (G.edges u).map (computeCP G dur fuel) := by
    apply List.mem_map.2
    exact ⟨v, hEdge, rfl⟩
  have hMaxGe := foldl_max_contains (computeCP G dur fuel v) ((G.edges u).map (computeCP G dur fuel)) hMapIn 0
  exact Nat.add_le_add_left hMaxGe (dur u)

/--
  Definition of a Valid Parallel Schedule.
  S : TargetName → Nat assigns a completion timestamp to each target such that:
  1. Each target finishes at or after its execution duration: S u >= dur u.
  2. A target cannot complete before any prerequisite has completed plus its own duration:
     S u >= S v + dur u for all v in G.edges u.
-/
structure IsValidParallelSchedule (G : DepGraph) (dur : TargetName → Nat) (S : TargetName → Nat) : Prop where
  dur_bound : ∀ u, dur u ≤ S u
  precedence : ∀ u v, v ∈ G.edges u → S v + dur u ≤ S u

/--
  Theorem 4: Parallel Schedule Lower Bound.
  In ANY valid parallel schedule S (regardless of the number of worker threads or execution strategy),
  the finish time of target u is bounded below by the duration of EVERY directed chain ending at u.
-/
theorem schedule_bounded_by_path (G : DepGraph) (dur : TargetName → Nat) (S : TargetName → Nat)
    (hSched : IsValidParallelSchedule G dur S) : ∀ {u v : TargetName} (p : Path G u v),
    pathDuration dur p ≤ S u := by
  intro u v p
  induction p with
  | @direct u0 v0 hEdge =>
    dsimp [pathDuration]
    have hPrec := hSched.precedence u0 v0 hEdge
    have hDurV := hSched.dur_bound v0
    have hSum : dur v0 + dur u0 ≤ S v0 + dur u0 := Nat.add_le_add_right hDurV (dur u0)
    have hComm : dur u0 + dur v0 = dur v0 + dur u0 := Nat.add_comm (dur u0) (dur v0)
    rw [hComm]
    exact Nat.le_trans hSum hPrec
  | @step u0 v0 w0 hEdge pRest ih =>
    dsimp [pathDuration]
    have hPrec := hSched.precedence u0 v0 hEdge
    have hSum : pathDuration dur pRest + dur u0 ≤ S v0 + dur u0 := Nat.add_le_add_right ih (dur u0)
    have hComm : dur u0 + pathDuration dur pRest = pathDuration dur pRest + dur u0 := Nat.add_comm (dur u0) _
    rw [hComm]
    exact Nat.le_trans hSum hPrec

/--
  Theorem 5: Critical Path Duration Monotonicity at Base Depth.
  If all target task durations weakly increase (dur1 ≤ dur2), then the computeCP
  bound weakly increases for all nodes at depth 0.
-/
theorem computeCP_zero_monotone (G : DepGraph) (dur1 dur2 : TargetName → Nat)
    (hDur : ∀ x, dur1 x ≤ dur2 x) (u : TargetName) :
    computeCP G dur1 0 u ≤ computeCP G dur2 0 u := by
  dsimp [computeCP]
  exact hDur u

end LeanMake
