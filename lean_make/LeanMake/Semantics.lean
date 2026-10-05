/-
  LeanMake.Semantics
  Formal Operational Semantics of Make: Filesystem state, Freshness rules,
  POSIX-compliant timestamp comparisons, and cyclic-safe DAG execution.
-/

import LeanMake.Syntax
import LeanMake.Graph

namespace LeanMake

/-- Timestamp representation as a non-negative natural number (Nat) -/
abbrev Timestamp := Nat

/-- The state of an entity on the filesystem: either nonexistent or having an mtime -/
inductive FileState where
  | missing : FileState
  | present (mtime : Timestamp) : FileState
deriving Repr, DecidableEq

/-- A Filesystem environment maps file paths to FileState -/
def Filesystem := TargetName → FileState

def Filesystem.get (fs : Filesystem) (name : TargetName) : FileState :=
  fs name

def Filesystem.set (fs : Filesystem) (name : TargetName) (mtime : Timestamp) : Filesystem :=
  fun n => if n == name then FileState.present mtime else fs n

/-- The outcome of evaluating a single target -/
inductive TargetOutcome where
  | upToDate (mtime : Timestamp) : TargetOutcome
  | rebuilt  (newMtime : Timestamp) : TargetOutcome
  | failed   (msg : String) : TargetOutcome
deriving Repr, DecidableEq

/-- Helper to extract effective timestamp from an outcome -/
def TargetOutcome.timestamp : TargetOutcome → Option Timestamp
  | upToDate t => some t
  | rebuilt t  => some t
  | failed _   => none

/-- Build execution state carrying filesystem, memoized target outcomes, visiting stack, and clock -/
structure BuildState where
  fs       : Filesystem
  outcomes : List (TargetName × TargetOutcome)
  visiting : List TargetName
  clock    : Timestamp
  /-- Targets whose recipe ran, newest first (see `LeanMake.RunOnce`). -/
  ran      : List TargetName := []

def BuildState.getOutcome (st : BuildState) (t : TargetName) : Option TargetOutcome :=
  match st.outcomes.find? (fun (name, _) => name == t) with
  | some (_, out) => some out
  | none => none

def BuildState.recordOutcome (st : BuildState) (t : TargetName) (out : TargetOutcome) : BuildState :=
  { st with outcomes := (t, out) :: st.outcomes }

/-- Compute the maximum timestamp observed among prerequisite outcomes -/
def maxDepTimestamp (deps : List (TargetName × TargetOutcome)) : Timestamp :=
  deps.foldl (fun acc (_, out) =>
    match out.timestamp with
    | some t => Nat.max acc t
    | none => acc) 0

/--
  Freshness decision (POSIX, as GNU make implements it). Target T is remade if:
  1. T is phony; OR
  2. T does not exist on the filesystem (with or without a recipe); OR
  3. Any prerequisite was rebuilt in the current run; OR
  4. Any prerequisite has a modification time strictly greater than T (dMtime > tMtime).
  `hasCommands` does not change the decision, only what remaking does (`executeRule`).
-/
def needsRebuild
    (targetState : FileState)
    (isPhony : Bool)
    (_hasCommands : Bool)
    (depOutcomes : List (TargetName × TargetOutcome)) : Bool :=
  if isPhony then true
  else
    match targetState with
    | FileState.missing => true
    | FileState.present tMtime =>
      depOutcomes.any (fun (_, out) =>
        match out with
        | TargetOutcome.rebuilt _ => true
        | TargetOutcome.upToDate dMtime => dMtime > tMtime  -- POSIX: strictly newer!
        | TargetOutcome.failed _ => false)

/-- Single step execution of a rule given already evaluated prerequisites -/
def executeRule
    (r : Rule)
    (depResults : List (TargetName × TargetOutcome))
    (st : BuildState) : (BuildState × TargetOutcome) :=
  -- If any dependency failed, propagate failure immediately
  match depResults.find? (fun (_, out) => match out with | TargetOutcome.failed _ => true | _ => false) with
  | some (_, TargetOutcome.failed msg) =>
    let out := TargetOutcome.failed s!"Prerequisite of {r.target} failed: {msg}"
    (st.recordOutcome r.target out, out)
  | _ =>
    let targetFs := st.fs.get r.target
    let depMax := maxDepTimestamp depResults
    if needsRebuild targetFs r.isPhony (!r.commands.isEmpty) depResults then
      if r.commands.isEmpty then
        -- Remade with no recipe. GNU make: a phony or nonexistent target is
        -- then taken as just updated (the `FORCE:` idiom); an existing file
        -- keeps its mtime, so its dependents compare timestamps as usual.
        match r.isPhony, targetFs with
        | false, FileState.present t =>
          let out := TargetOutcome.upToDate t
          (st.recordOutcome r.target out, out)
        | _, _ =>
          let newClock := (Nat.max st.clock depMax) + 1
          let out := TargetOutcome.rebuilt newClock
          ({ st with clock := newClock }.recordOutcome r.target out, out)
      else
        -- The recipe runs: advance the clock strictly beyond both the current
        -- clock and every prerequisite.
        let newClock := (Nat.max st.clock depMax) + 1
        let newFs := if r.isPhony then st.fs else st.fs.set r.target newClock
        let out := TargetOutcome.rebuilt newClock
        let st1 := { st with fs := newFs, clock := newClock, ran := r.target :: st.ran }
        (st1.recordOutcome r.target out, out)
    else
      -- Target is already up to date
      match targetFs with
      | FileState.present t =>
        let out := TargetOutcome.upToDate t
        (st.recordOutcome r.target out, out)
      | FileState.missing =>
        -- Unreachable: `needsRebuild` is true for a missing target.
        let out := TargetOutcome.upToDate depMax
        (st.recordOutcome r.target out, out)

/--
  Recursive evaluation of a Target in the DAG with cycle detection on the call stack
  and fuel-based certified termination.
-/
def evalTarget (mf : Makefile) (fuel : Nat) (t : TargetName) (st : BuildState) :
    (BuildState × TargetOutcome) :=
  match fuel with
  | 0 => (st, TargetOutcome.failed "Recursion fuel exhausted")
  | fuel + 1 =>
    -- Check memoized results
    match st.getOutcome t with
    | some cached => (st, cached)
    | none =>
      -- Cycle detection check on the active call stack
      if st.visiting.contains t then
        let out := TargetOutcome.failed s!"Circular dependency detected on target '{t}'"
        (st.recordOutcome t out, out)
      else
        let stVisiting := { st with visiting := t :: st.visiting }
        match mf.findRule t with
        | none =>
          -- Leaf file on filesystem
          let stUnvisiting := { stVisiting with visiting := st.visiting }
          match st.fs.get t with
          | FileState.present mtime =>
            let out := TargetOutcome.upToDate mtime
            (stUnvisiting.recordOutcome t out, out)
          | FileState.missing =>
            let out := TargetOutcome.failed s!"No rule to make target '{t}'"
            (stUnvisiting.recordOutcome t out, out)
        | some rule =>
          -- Recursively evaluate all prerequisites
          let rec evalDeps (deps : List TargetName) (currSt : BuildState) (acc : List (TargetName × TargetOutcome)) :
              (BuildState × List (TargetName × TargetOutcome)) :=
            match deps with
            | [] => (currSt, acc.reverse)
            | d :: ds =>
              let (nextSt, dOut) := evalTarget mf fuel d currSt
              evalDeps ds nextSt ((d, dOut) :: acc)

          let (stAfterDeps, depResults) := evalDeps rule.prereqs stVisiting []
          let stFinalVisiting := { stAfterDeps with visiting := st.visiting }
          -- A target settles once per run: if a prerequisite's evaluation
          -- already settled it (only possible through a cycle), keep that.
          match stAfterDeps.getOutcome t with
          | some settled => (stFinalVisiting, settled)
          | none => executeRule rule depResults stFinalVisiting

/-- Full build of the primary target in a Makefile -/
def runMake (mf : Makefile) (initialFs : Filesystem) (initialMaxMtime : Timestamp := 0) (targetName : Option TargetName := none) :
    (BuildState × TargetOutcome) :=
  let root :=
    match targetName <|> mf.defaultTarget with
    | some name => name
    | none =>
      -- First non-special target
      match mf.rules.find? (fun r => !r.target.startsWith ".") with
      | some r => r.target
      | none => ""
  let initState : BuildState := {
    fs := initialFs,
    outcomes := [],
    visiting := [],
    clock := initialMaxMtime
  }
  let fuel := mf.rules.length * 20 + 20
  evalTarget mf fuel root initState

end LeanMake
