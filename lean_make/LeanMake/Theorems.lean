/-
  LeanMake.Theorems
  Formally verified theorems for Make semantics:
  1. Clock Monotonicity (weak).
  2. Phony Target Unconditional Rebuild Invariant.
  3. Missing Target Unconditional Rebuild Invariant.
  4. Idempotency of Up-to-Date Dependency Sets (POSIX Freshness).
  5. Cycle Path Trace Starts with the Detected Target.
  6. Strict Clock Advancement upon Rebuild (Nat Arithmetic).
  7. Rebuild Clock Strictly Exceeds Maximum Prerequisite Timestamp (Nat Arithmetic).
-/

import LeanMake.Syntax
import LeanMake.Graph
import LeanMake.Semantics

namespace LeanMake

/--
  Theorem 1: Clock Monotonicity (Weak).
  Rule execution always weakly increases the clock timestamp:
  st'.clock >= st.clock.
-/
theorem executeRule_clock_monotonic (r : Rule) (deps : List (TargetName × TargetOutcome)) (st : BuildState) :
    (executeRule r deps st).1.clock >= st.clock := by
  dsimp [executeRule]
  split
  · -- case failure
    exact Nat.le_refl st.clock
  · -- case success
    split
    · -- case needsRebuild = true
      dsimp
      split
      · -- isPhony = true
        have hMax : Nat.max st.clock (maxDepTimestamp deps) >= st.clock := Nat.le_max_left st.clock (maxDepTimestamp deps)
        exact Nat.le_trans hMax (Nat.le_succ _)
      · -- isPhony = false
        have hMax : Nat.max st.clock (maxDepTimestamp deps) >= st.clock := Nat.le_max_left st.clock (maxDepTimestamp deps)
        exact Nat.le_trans hMax (Nat.le_succ _)
    · -- case needsRebuild = false
      split
      · -- FileState.present
        exact Nat.le_refl st.clock
      · -- FileState.missing (alias)
        exact Nat.le_refl st.clock

/--
  Theorem 2: Phony Target Always Rebuilds when Commands are Present.
  If a rule is marked .PHONY and has recipe commands, it will always report needsRebuild = true,
  regardless of the presence or timestamp of any file on disk.
-/
theorem phony_always_rebuilds
    (fsState : FileState)
    (deps : List (TargetName × TargetOutcome)) :
    needsRebuild fsState true true deps = true := by
  dsimp [needsRebuild]

/--
  Theorem 3: Missing Target Always Rebuilds when Commands are Present.
  If a non-phony target is missing from the filesystem and has commands, needsRebuild evaluates to true.
-/
theorem missing_always_rebuilds
    (deps : List (TargetName × TargetOutcome)) :
    needsRebuild FileState.missing false true deps = true := by
  dsimp [needsRebuild]

/--
  Theorem 4: Idempotency of Fresh State.
  If target exists with timestamp T, is not phony, and all prerequisite outcomes
  are upToDate with timestamps strictly older than T, then needsRebuild evaluates to false.
-/
theorem upToDate_needs_no_rebuild
    (tMtime : Timestamp)
    (hasCommands : Bool)
    (deps : List (TargetName × TargetOutcome))
    (hDepsOlder : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime ∧ dMtime < tMtime) :
    needsRebuild (FileState.present tMtime) false hasCommands deps = false := by
  dsimp [needsRebuild]
  rw [List.any_eq_false]
  intro (dName, dOut) hIn
  have h := hDepsOlder (dName, dOut) hIn
  rcases h with ⟨dMtime, hdOut, hLt⟩
  dsimp
  change dOut = TargetOutcome.upToDate dMtime at hdOut
  rw [hdOut]
  dsimp
  have hNotGt : ¬ (dMtime > tMtime) := by
    intro hGt
    exact Nat.lt_asymm hLt hGt
  cases hDec : decide (dMtime > tMtime) with
  | true =>
    exfalso
    exact hNotGt (of_decide_eq_true hDec)
  | false =>
    intro h
    contradiction

/--
  Theorem 5: Cycle Path Trace Starts with the Detected Target.
-/
theorem cycle_trace_head (u : TargetName) (stack : List TargetName) :
    (u :: (stack.takeWhile (· != u)).reverse ++ [u]).head? = some u := by
  rfl

/--
  Theorem 6: Rebuild Clock Strictly Exceeds Monotonic Base.
  When a target is rebuilt, its new timestamp strictly advances past st.clock.
-/
theorem rebuild_clock_strictly_greater (stClock : Timestamp) (depMax : Timestamp) :
    (Nat.max stClock depMax) + 1 > stClock := by
  have h := Nat.le_max_left stClock depMax
  exact Nat.lt_succ_of_le h

/--
  Theorem 7: Rebuild Clock Strictly Exceeds Maximum Prerequisite Timestamp.
  When a target is rebuilt, its new timestamp strictly advances past the newest prerequisite timestamp.
-/
theorem rebuild_clock_strictly_exceeds_deps (stClock : Timestamp) (depMax : Timestamp) :
    (Nat.max stClock depMax) + 1 > depMax := by
  have h := Nat.le_max_right stClock depMax
  exact Nat.lt_succ_of_le h

/--
  Theorem 8: Freshness Guarantee upon Rebuild.
  When a target is rebuilt, its new timestamp strictly exceeds the timestamp of EVERY prerequisite.
-/
theorem rebuild_strictly_fresher_than_dep
    (stClock : Timestamp)
    (depMax : Timestamp)
    (dMtime : Timestamp)
    (hDepLeMax : dMtime ≤ depMax) :
    (Nat.max stClock depMax) + 1 > dMtime := by
  have hMaxRight := Nat.le_max_right stClock depMax
  have hTrans := Nat.le_trans hDepLeMax hMaxRight
  exact Nat.lt_succ_of_le hTrans

/--
  Theorem 9: Alias Target Up-To-Date Invariant.
  If an alias target without commands has all prerequisites upToDate, it needs no rebuild.
-/
theorem alias_no_rebuild_when_deps_up_to_date
    (isPhony : Bool)
    (deps : List (TargetName × TargetOutcome))
    (hDepsUpToDate : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime) :
    needsRebuild FileState.missing isPhony false deps = false := by
  dsimp [needsRebuild]
  split
  · -- case isPhony = true
    rw [List.any_eq_false]
    intro (dName, dOut) hIn
    rcases hDepsUpToDate (dName, dOut) hIn with ⟨dMtime, hdOut⟩
    dsimp
    change dOut = TargetOutcome.upToDate dMtime at hdOut
    rw [hdOut]
    intro h
    contradiction
  · -- case isPhony = false
    rw [List.any_eq_false]
    intro (dName, dOut) hIn
    rcases hDepsUpToDate (dName, dOut) hIn with ⟨dMtime, hdOut⟩
    dsimp
    change dOut = TargetOutcome.upToDate dMtime at hdOut
    rw [hdOut]
    intro h
    contradiction

/--
  Lemma: Up-To-Date Prerequisite List Contains No Failure.
-/
theorem deps_all_upToDate_no_failure (deps : List (TargetName × TargetOutcome))
    (hAll : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime) :
    deps.find? (fun x => match x.snd with | TargetOutcome.failed _ => true | _ => false) = none := by
  induction deps with
  | nil => rfl
  | cons head tail ih =>
    rcases head with ⟨dName, dOut⟩
    have hHeadIn : (dName, dOut) ∈ (dName, dOut) :: tail := List.Mem.head tail
    have hHead := hAll (dName, dOut) hHeadIn
    rcases hHead with ⟨dMtime, hdOut⟩
    dsimp at hdOut
    have hTailAll : ∀ d ∈ tail, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime := by
      intro d hdIn
      exact hAll d (List.Mem.tail _ hdIn)
    have ihRes := ih hTailAll
    dsimp [List.find?]
    rw [hdOut]
    dsimp
    exact ihRes

/--
  Theorem 10: Single-Rule Post-Build Idempotency.
  If a rule is not phony, its target is present on the filesystem at timestamp `tMtime`,
  and all its prerequisite outcomes are up-to-date with timestamps strictly older than `tMtime`,
  then `executeRule` produces `TargetOutcome.upToDate tMtime` with identical filesystem and clock.
-/
theorem executeRule_upToDate_idempotent
    (r : Rule)
    (hNotPhony : r.isPhony = false)
    (tMtime : Timestamp)
    (deps : List (TargetName × TargetOutcome))
    (hDepsOlder : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime ∧ dMtime < tMtime)
    (st : BuildState)
    (hFs : st.fs.get r.target = FileState.present tMtime) :
    executeRule r deps st =
      (st.recordOutcome r.target (TargetOutcome.upToDate tMtime), TargetOutcome.upToDate tMtime) := by
  dsimp [executeRule]
  have hDepsUpToDate : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime := by
    intro d hdIn
    rcases hDepsOlder d hdIn with ⟨dMtime, hdOut, _⟩
    exact ⟨dMtime, hdOut⟩
  split
  · rename_i fst msg heq
    have hNoFail := deps_all_upToDate_no_failure deps hDepsUpToDate
    have hContra := heq.symm.trans hNoFail
    contradiction
  · -- case 2
    rw [hFs]
    rw [hNotPhony]
    have hNeedsRebuildFalse := upToDate_needs_no_rebuild tMtime (!r.commands.isEmpty) deps hDepsOlder
    rw [hNeedsRebuildFalse]
    dsimp

/--
  Theorem 11: Filesystem Invariance under Up-to-Date Execution.
  When a target is up-to-date, its execution preserves the filesystem state identically.
-/
theorem executeRule_upToDate_fs_invariant
    (r : Rule)
    (hNotPhony : r.isPhony = false)
    (tMtime : Timestamp)
    (deps : List (TargetName × TargetOutcome))
    (hDepsOlder : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime ∧ dMtime < tMtime)
    (st : BuildState)
    (hFs : st.fs.get r.target = FileState.present tMtime) :
    (executeRule r deps st).1.fs = st.fs := by
  have h := executeRule_upToDate_idempotent r hNotPhony tMtime deps hDepsOlder st hFs
  rw [h]
  dsimp [BuildState.recordOutcome]

/--
  Theorem 12: Clock Invariance under Up-to-Date Execution.
  When a target is up-to-date, its execution does not advance the build clock.
-/
theorem executeRule_upToDate_clock_invariant
    (r : Rule)
    (hNotPhony : r.isPhony = false)
    (tMtime : Timestamp)
    (deps : List (TargetName × TargetOutcome))
    (hDepsOlder : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime ∧ dMtime < tMtime)
    (st : BuildState)
    (hFs : st.fs.get r.target = FileState.present tMtime) :
    (executeRule r deps st).1.clock = st.clock := by
  have h := executeRule_upToDate_idempotent r hNotPhony tMtime deps hDepsOlder st hFs
  rw [h]
  dsimp [BuildState.recordOutcome]

/--
  Theorem 13: Inductive DAG Freshness Invariant.
  In any build state where all prerequisites of a target have evaluated to `upToDate`
  with timestamps strictly bounded by the target's filesystem mtime,
  the target outcome is guaranteed to be `upToDate` with zero rebuilds.
-/
theorem dag_step_zero_rebuilds
    (r : Rule)
    (hNotPhony : r.isPhony = false)
    (tMtime : Timestamp)
    (deps : List (TargetName × TargetOutcome))
    (hDepsOlder : ∀ d ∈ deps, ∃ dMtime, d.2 = TargetOutcome.upToDate dMtime ∧ dMtime < tMtime)
    (st : BuildState)
    (hFs : st.fs.get r.target = FileState.present tMtime) :
    (executeRule r deps st).2 = TargetOutcome.upToDate tMtime := by
  have h := executeRule_upToDate_idempotent r hNotPhony tMtime deps hDepsOlder st hFs
  rw [h]

/--
  Theorem 14: Memoized Up-To-Date Target Evaluation Invariance.
  If a target outcome has been memoized as upToDate in the build state,
  re-evaluating it with any positive fuel returns identical state and the memoized outcome.
-/
theorem evalTarget_memoized_upToDate
    (mf : Makefile)
    (fuel : Nat)
    (t : TargetName)
    (st : BuildState)
    (mtime : Timestamp)
    (hMemo : st.getOutcome t = some (TargetOutcome.upToDate mtime)) :
    evalTarget mf (fuel + 1) t st = (st, TargetOutcome.upToDate mtime) := by
  unfold evalTarget
  rw [hMemo]

/--
  Theorem 15: Single-Leaf Filesystem Up-To-Date Evaluation.
  If an entity on the filesystem has no Makefile rule and exists on the filesystem with timestamp `mtime`,
  evaluating it with positive fuel produces `TargetOutcome.upToDate mtime` without rebuild.
-/
theorem evalTarget_leaf_upToDate
    (mf : Makefile)
    (fuel : Nat)
    (t : TargetName)
    (st : BuildState)
    (mtime : Timestamp)
    (hNotVisiting : st.visiting.contains t = false)
    (hNoMemo : st.getOutcome t = none)
    (hNoRule : mf.findRule t = none)
    (hFs : st.fs.get t = FileState.present mtime) :
    evalTarget mf (fuel + 1) t st =
      ({ st with outcomes := (t, TargetOutcome.upToDate mtime) :: st.outcomes }, TargetOutcome.upToDate mtime) := by
  unfold evalTarget
  rw [hNoMemo]
  dsimp
  rw [hNotVisiting]
  dsimp
  rw [hNoRule]
  dsimp
  rw [hFs]
  dsimp [BuildState.recordOutcome]

end LeanMake
