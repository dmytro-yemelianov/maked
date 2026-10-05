/-
  LeanMake.Cache
  Formal specification and machine-checked proofs of Content-Addressable
  Build Caching (CAS), verifying cache hit idempotency, cache soundness,
  and tamper invalidation.
-/

import LeanMake.Syntax
import LeanMake.Semantics

namespace LeanMake

/-- A cryptographic content hash is represented as a Nat identifier -/
abbrev ContentHash := Nat

/--
  A CacheKey uniquely identifies an action in the Content-Addressable Storage (CAS).
  It encapsulates:
  1. The target name.
  2. The exact list of recipe commands.
  3. The list of prerequisite target names and their verified content hashes.
-/
structure CacheKey where
  target       : TargetName
  commands     : List Command
  prereqHashes : List (TargetName × ContentHash)
deriving Repr, DecidableEq, Inhabited

/--
  The Content-Addressable Storage (CAS) repository mapping CacheKeys to
  the resulting artifact content hash.
-/
abbrev CASStore := List (CacheKey × ContentHash)

/-- Query the CAS repository for an existing compiled artifact hash -/
def lookupCAS (store : CASStore) (key : CacheKey) : Option ContentHash :=
  match store.find? (fun (k, _) => k == key) with
  | some (_, art) => some art
  | none => none

/-- Store an artifact hash into the CAS repository under the specified CacheKey -/
def storeCAS (store : CASStore) (key : CacheKey) (art : ContentHash) : CASStore :=
  (key, art) :: store

/--
  Execution result with CAS caching:
  Contains the updated CAS repository, the produced artifact content hash,
  and a boolean flag indicating whether the artifact was restored from cache.
-/
structure CacheExecutionResult where
  store       : CASStore
  artifact    : ContentHash
  wasRestored : Bool
deriving Repr, DecidableEq

/--
  Execute a rule with CAS caching enabled:
  1. Computes the action CacheKey from the rule and prerequisite hashes.
  2. If a matching artifact exists in CAS, restores it instantly (wasRestored = true).
  3. Otherwise, invokes the recipe runner (modeled deterministically as `recipeRunner`),
     stores the newly produced artifact into CAS, and returns (wasRestored = false).
-/
def executeWithCAS
    (r : Rule)
    (depHashes : List (TargetName × ContentHash))
    (store : CASStore)
    (recipeRunner : Unit → ContentHash) : CacheExecutionResult :=
  let key : CacheKey := ⟨r.target, r.commands, depHashes⟩
  match lookupCAS store key with
  | some cachedArt =>
    { store := store, artifact := cachedArt, wasRestored := true }
  | none =>
    let freshArt := recipeRunner ()
    { store := storeCAS store key freshArt, artifact := freshArt, wasRestored := false }

/--
  Theorem 1: Lookup Hit After Store.
  Storing an artifact under key `k` guarantees that an immediate lookup for `k` returns `some art`.
-/
theorem lookupCAS_storeCAS_hit (store : CASStore) (k : CacheKey) (art : ContentHash) :
    lookupCAS (storeCAS store k art) k = some art := by
  dsimp [storeCAS, lookupCAS]
  have hEq : (k == k) = true := by exact decide_eq_true rfl
  dsimp [List.find?]
  rw [hEq]

/--
  Theorem 2: Cache Hit Invariant.
  If an artifact is present in CAS, `executeWithCAS` restores it directly without
  invoking the recipe runner and leaves the CAS store unchanged.
-/
theorem executeWithCAS_hit_invariant
    (r : Rule) (depHashes : List (TargetName × ContentHash)) (store : CASStore)
    (runner : Unit → ContentHash) (cachedArt : ContentHash)
    (hHit : lookupCAS store ⟨r.target, r.commands, depHashes⟩ = some cachedArt) :
    executeWithCAS r depHashes store runner =
      { store := store, artifact := cachedArt, wasRestored := true } := by
  dsimp [executeWithCAS]
  rw [hHit]

/--
  Theorem 3: Cache Miss Invariant.
  If an artifact is absent in CAS, `executeWithCAS` invokes the runner, stores the result,
  and marks `wasRestored = false`.
-/
theorem executeWithCAS_miss_invariant
    (r : Rule) (depHashes : List (TargetName × ContentHash)) (store : CASStore)
    (runner : Unit → ContentHash)
    (hMiss : lookupCAS store ⟨r.target, r.commands, depHashes⟩ = none) :
    executeWithCAS r depHashes store runner =
      { store := storeCAS store ⟨r.target, r.commands, depHashes⟩ (runner ()),
        artifact := runner (),
        wasRestored := false } := by
  dsimp [executeWithCAS]
  rw [hMiss]

/--
  Theorem 4: Cache Idempotency.
  Executing a rule with CAS caching guarantees that any subsequent execution with the same
  inputs hits the cache (`wasRestored = true`) and reproduces the exact same artifact.
-/
theorem executeWithCAS_idempotent
    (r : Rule) (depHashes : List (TargetName × ContentHash)) (store : CASStore)
    (runner : Unit → ContentHash) :
    let res1 := executeWithCAS r depHashes store runner
    let res2 := executeWithCAS r depHashes res1.store runner
    res2.wasRestored = true ∧ res2.artifact = res1.artifact := by
  intro res1 res2
  dsimp [res1, res2, executeWithCAS]
  cases hLookup : lookupCAS store ⟨r.target, r.commands, depHashes⟩ with
  | some cachedArt =>
    dsimp
    rw [hLookup]
    exact ⟨rfl, rfl⟩
  | none =>
    dsimp
    have hHit := lookupCAS_storeCAS_hit store ⟨r.target, r.commands, depHashes⟩ (runner ())
    rw [hHit]
    exact ⟨rfl, rfl⟩

/--
  Theorem 5: Recipe Tamper Invalidation.
  Modifying recipe commands provably invalidates the CacheKey, preventing stale artifact restoration.
-/
theorem recipe_tamper_invalidates_key
    (t : TargetName) (cmds1 cmds2 : List Command)
    (deps : List (TargetName × ContentHash))
    (hDiff : cmds1 ≠ cmds2) :
    (⟨t, cmds1, deps⟩ : CacheKey) ≠ ⟨t, cmds2, deps⟩ := by
  intro hEq
  cases hEq
  exact hDiff rfl

/--
  Theorem 6: Prerequisite Content Tamper Invalidation.
  Modifying any prerequisite content hash provably invalidates the CacheKey.
-/
theorem dep_tamper_invalidates_key
    (t : TargetName) (cmds : List Command)
    (deps1 deps2 : List (TargetName × ContentHash))
    (hDiff : deps1 ≠ deps2) :
    (⟨t, cmds, deps1⟩ : CacheKey) ≠ ⟨t, cmds, deps2⟩ := by
  intro hEq
  cases hEq
  exact hDiff rfl

/--
  Theorem 7: Deterministic Cache Soundness.
  If the artifact stored in CAS was computed from a deterministic execution of the recipe runner,
  restoring it from cache yields an artifact indistinguishable from fresh execution.
-/
theorem cache_soundness_deterministic
    (r : Rule) (depHashes : List (TargetName × ContentHash)) (store : CASStore)
    (runner : Unit → ContentHash)
    (hPopulated : lookupCAS store ⟨r.target, r.commands, depHashes⟩ = some (runner ())) :
    (executeWithCAS r depHashes store runner).artifact = runner () := by
  dsimp [executeWithCAS]
  rw [hPopulated]

end LeanMake
