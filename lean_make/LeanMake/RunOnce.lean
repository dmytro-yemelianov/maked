/-
  LeanMake.RunOnce
  No recipe runs twice in one make invocation, including the phase in which
  make first brings its makefiles up to date.

  GNU make reads the makefiles, remakes those that have rules (included
  files such as git's `GIT-VERSION-FILE: FORCE`), and then builds the goals
  with the same record of what is already done: a target remade in the
  first phase counts as done in the second. maked v0.2.1 started the second
  phase from scratch and ran such recipes twice. `runPhases` models the two
  phases sharing one `BuildState`; `no_recipe_runs_twice` is the property
  that would have flagged the bug.

  (When a makefile changed, make re-executes itself. That is a new
  invocation and outside this model.)
-/

import LeanMake.Semantics

namespace LeanMake
namespace RunOnce

/-- Every outcome recorded in `st` is still recorded, unchanged, in `st'`. -/
def Extends (st st' : BuildState) : Prop :=
  ∀ x o, st.getOutcome x = some o → st'.getOutcome x = some o

/-- The recipe log has no repeats, and every target in it has settled. -/
def Inv (st : BuildState) : Prop :=
  st.ran.Nodup ∧ ∀ x ∈ st.ran, st.getOutcome x ≠ none

theorem Extends.refl (st : BuildState) : Extends st st := fun _ _ h => h

theorem Extends.trans {a b c : BuildState} (h₁ : Extends a b) (h₂ : Extends b c) :
    Extends a c := fun x o h => h₂ x o (h₁ x o h)

theorem inv_empty (st : BuildState) (h : st.ran = []) : Inv st := by
  simp [Inv, h]

theorem getOutcome_record (st : BuildState) (u x : TargetName) (o : TargetOutcome) :
    (st.recordOutcome u o).getOutcome x = if u = x then some o else st.getOutcome x := by
  unfold BuildState.getOutcome BuildState.recordOutcome
  by_cases h : u = x
  · subst h; simp
  · simp [h]

/-- `getOutcome` reads only the outcome list. -/
theorem getOutcome_of_outcomes {a b : BuildState} (h : a.outcomes = b.outcomes) (x : TargetName) :
    a.getOutcome x = b.getOutcome x := by
  unfold BuildState.getOutcome
  rw [h]

/-- Recording an outcome for a target that has none keeps every other one. -/
theorem record_extends (st : BuildState) (u : TargetName) (o : TargetOutcome)
    (h : st.getOutcome u = none) : Extends st (st.recordOutcome u o) := by
  intro x o' hx
  rw [getOutcome_record]
  by_cases hux : u = x
  · subst hux; rw [h] at hx; contradiction
  · simp [hux, hx]

/-- Recording an outcome (without running a recipe) keeps the invariant. -/
theorem record_inv (st : BuildState) (u : TargetName) (o : TargetOutcome) (hI : Inv st) :
    Inv (st.recordOutcome u o) := by
  refine ⟨hI.1, fun x hx => ?_⟩
  rw [getOutcome_record]
  by_cases hux : u = x
  · simp [hux]
  · simpa [hux] using hI.2 x hx

/-- One rule step, for a target that has not settled yet. -/
theorem executeRule_inv (r : Rule) (deps : List (TargetName × TargetOutcome)) (st : BuildState)
    (hI : Inv st) (hNone : st.getOutcome r.target = none) :
    Inv (executeRule r deps st).1 ∧ Extends st (executeRule r deps st).1 := by
  unfold executeRule
  split
  · exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hNone⟩
  · dsimp only
    split
    · split
      · split
        · exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hNone⟩
        · -- remade without a recipe: the clock moves, nothing runs
          refine ⟨record_inv _ _ _ ⟨hI.1, fun x hx => ?_⟩, ?_⟩
          · exact hI.2 x hx
          · intro x o hx
            exact record_extends _ _ _ hNone x o hx
      · -- the recipe runs: `r.target` joins the log for the first time
        have hNotIn : r.target ∉ st.ran := fun hIn => hI.2 _ hIn hNone
        refine ⟨⟨List.nodup_cons.mpr ⟨hNotIn, hI.1⟩, fun x hx => ?_⟩, ?_⟩
        · rw [getOutcome_record]
          by_cases hux : r.target = x
          · simp [hux]
          · simp only [hux, if_false]
            rcases List.mem_cons.mp hx with h | h
            · exact absurd h.symm hux
            · exact hI.2 x h
        · intro x o hx
          exact record_extends _ _ _ hNone x o hx
    · split <;> exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hNone⟩

/-- The rule `findRule` returns is the rule for that target. -/
theorem findRule_target (mf : Makefile) (t : TargetName) (r : Rule)
    (h : mf.findRule t = some r) : r.target = t := by
  unfold Makefile.findRule at h
  have := List.find?_some h
  simpa using this

theorem evalDeps_inv (mf : Makefile) (fuel : Nat)
    (ih : ∀ t st, Inv st → Inv (evalTarget mf fuel t st).1 ∧ Extends st (evalTarget mf fuel t st).1) :
    ∀ (deps : List TargetName) (st : BuildState) (acc : List (TargetName × TargetOutcome)),
      Inv st → Inv (evalTarget.evalDeps mf fuel deps st acc).1 ∧
        Extends st (evalTarget.evalDeps mf fuel deps st acc).1
  | [], st, acc, hI => by
    rw [evalTarget.evalDeps.eq_1]
    exact ⟨hI, Extends.refl st⟩
  | d :: ds, st, acc, hI => by
    rw [evalTarget.evalDeps.eq_2]
    obtain ⟨h₁, e₁⟩ := ih d st hI
    obtain ⟨h₂, e₂⟩ := evalDeps_inv mf fuel ih ds (evalTarget mf fuel d st).1
      ((d, (evalTarget mf fuel d st).2) :: acc) h₁
    exact ⟨h₂, e₁.trans e₂⟩

/--
  Evaluating any target keeps the invariant and never changes an outcome
  that was already recorded.
-/
theorem evalTarget_inv (mf : Makefile) :
    ∀ (fuel : Nat) (t : TargetName) (st : BuildState),
      Inv st → Inv (evalTarget mf fuel t st).1 ∧ Extends st (evalTarget mf fuel t st).1
  | 0, t, st, hI => by
    unfold evalTarget
    exact ⟨hI, Extends.refl st⟩
  | fuel + 1, t, st, hI => by
    have ih := evalTarget_inv mf fuel
    unfold evalTarget
    split
    · exact ⟨hI, Extends.refl st⟩
    · rename_i hNone
      dsimp only
      split
      · exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hNone⟩
      · split
        · -- no rule: a file on disk, or an error; recorded on `st` with
          -- its visiting stack restored, which `getOutcome` ignores
          have hN : ({ st with visiting := st.visiting } : BuildState).getOutcome t = none := hNone
          split
          · exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hN⟩
          · exact ⟨record_inv _ _ _ hI, record_extends _ _ _ hN⟩
        · rename_i rule hRule
          have hT := findRule_target mf t rule hRule
          obtain ⟨hD, eD⟩ := evalDeps_inv mf fuel ih rule.prereqs
            { st with visiting := t :: st.visiting } [] hI
          generalize evalTarget.evalDeps mf fuel rule.prereqs
            { st with visiting := t :: st.visiting } [] = p at hD eD ⊢
          obtain ⟨sa, dr⟩ := p
          dsimp only at hD eD ⊢
          have eD' : Extends st { sa with visiting := st.visiting } := eD
          split
          · exact ⟨hD, eD'⟩
          · rename_i hSaNone
            have hN : ({ sa with visiting := st.visiting } : BuildState).getOutcome rule.target = none := by
              rw [hT]; exact hSaNone
            obtain ⟨hE, eE⟩ := executeRule_inv rule dr { sa with visiting := st.visiting } hD hN
            exact ⟨hE, eD'.trans eE⟩

/-- A settled target is never evaluated again: the recorded outcome comes back. -/
theorem evalTarget_settled (mf : Makefile) (fuel : Nat) (t : TargetName) (st : BuildState)
    (o : TargetOutcome) (h : st.getOutcome t = some o) :
    evalTarget mf (fuel + 1) t st = (st, o) := by
  unfold evalTarget
  rw [h]

/-- Build each target in turn, threading one state. -/
def runTargets (mf : Makefile) (fuel : Nat) : List TargetName → BuildState → BuildState
  | [], st => st
  | g :: gs, st => runTargets mf fuel gs (evalTarget mf fuel g st).1

/-- Remake the makefiles first, then build the goals, with one shared state. -/
def runPhases (mf : Makefile) (fuel : Nat) (makefiles goals : List TargetName)
    (st : BuildState) : BuildState :=
  runTargets mf fuel goals (runTargets mf fuel makefiles st)

theorem runTargets_inv (mf : Makefile) (fuel : Nat) :
    ∀ (gs : List TargetName) (st : BuildState),
      Inv st → Inv (runTargets mf fuel gs st) ∧ Extends st (runTargets mf fuel gs st)
  | [], st, hI => ⟨hI, Extends.refl st⟩
  | g :: gs, st, hI => by
    obtain ⟨h₁, e₁⟩ := evalTarget_inv mf fuel g st hI
    obtain ⟨h₂, e₂⟩ := runTargets_inv mf fuel gs _ h₁
    exact ⟨h₂, e₁.trans e₂⟩

/--
  **No recipe runs twice** in one invocation, across both phases: the log
  of recipes that ran has no repeats.
-/
theorem no_recipe_runs_twice (mf : Makefile) (fuel : Nat) (makefiles goals : List TargetName)
    (st : BuildState) (h : st.ran = []) :
    (runPhases mf fuel makefiles goals st).ran.Nodup := by
  have hI := inv_empty st h
  obtain ⟨h₁, _⟩ := runTargets_inv mf fuel makefiles st hI
  exact (runTargets_inv mf fuel goals _ h₁).1.1

/--
  **A target remade with the makefiles is done for the goals**: whatever it
  settled to in the first phase is its outcome at the end of the run.
-/
theorem remade_with_makefiles_is_done (mf : Makefile) (fuel : Nat)
    (makefiles goals : List TargetName) (st : BuildState) (hI : Inv st)
    (x : TargetName) (o : TargetOutcome)
    (h : (runTargets mf fuel makefiles st).getOutcome x = some o) :
    (runPhases mf fuel makefiles goals st).getOutcome x = some o := by
  have h₁ := (runTargets_inv mf fuel makefiles st hI).1
  exact (runTargets_inv mf fuel goals _ h₁).2 x o h

/-! ### git's case, concretely

`ver.mk: FORCE` has a recipe; `FORCE:` has neither prerequisites nor a
recipe. Run as v0.2.1 did (the goals phase starting with no record of the
first), `ver.mk`'s recipe runs twice; with one shared state, once. These two
checks evaluate the model with `native_decide`, which trusts the Lean
compiler; the theorems above do not depend on them. -/

def gitLike : Makefile := {
  rules := [
    { target := "all", prereqs := ["ver.mk"], commands := ["build"], isPhony := true },
    { target := "ver.mk", prereqs := ["FORCE"], commands := ["gen"] },
    { target := "FORCE", prereqs := [], commands := [] } ],
  variables := [], defaultTarget := some "all" }

def gitLikeStart : BuildState :=
  { fs := fun n => if n = "ver.mk" then FileState.present 5 else FileState.missing,
    outcomes := [], visiting := [], clock := 5 }

/-- v0.2.1: the goals phase started with no record of the makefile phase. -/
def runPhasesFresh (mf : Makefile) (fuel : Nat) (makefiles goals : List TargetName)
    (st : BuildState) : BuildState :=
  let mid := runTargets mf fuel makefiles st
  runTargets mf fuel goals { mid with outcomes := [] }

example : (runPhasesFresh gitLike 10 ["ver.mk"] ["all"] gitLikeStart).ran =
    ["all", "ver.mk", "ver.mk"] := by native_decide

example : (runPhases gitLike 10 ["ver.mk"] ["all"] gitLikeStart).ran =
    ["all", "ver.mk"] := by native_decide

end RunOnce
end LeanMake
