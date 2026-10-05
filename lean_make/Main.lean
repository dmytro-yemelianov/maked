import Std.Data.HashMap
import LeanMake

open LeanMake

def sampleMakefile : Makefile := {
  rules := [
    { target := "all", prereqs := ["app"], commands := ["echo Done"], isPhony := true },
    { target := "app", prereqs := ["main.o", "lib.o"], commands := ["cc -o app main.o lib.o"] },
    { target := "main.o", prereqs := ["main.c", "lib.h"], commands := ["cc -c main.c"] },
    { target := "lib.o", prereqs := ["lib.c", "lib.h"], commands := ["cc -c lib.c"] }
  ],
  variables := [("CC", "clang"), ("CFLAGS", "-O2")],
  defaultTarget := some "all"
}

-- Real-world POSIX epoch timestamps (e.g. 1700000000)
def initialFs : Filesystem := fun name =>
  match name with
  | "main.c" => FileState.present 1700000100
  | "lib.c"  => FileState.present 1700000105
  | "lib.h"  => FileState.present 1700000110
  | "main.o" => FileState.present 1700000090   -- stale! lib.h is 1700000110
  | "lib.o"  => FileState.present 1700000120  -- up to date! lib.c is 105, lib.h is 110
  | "app"    => FileState.present 1700000095   -- stale!
  | _        => FileState.missing

def parseSpec (content : String) : (Makefile × List (String × Timestamp) × Option TargetName) :=
  let lines := content.splitOn "\n"
  let rec loop (ls : List String) (rules : List Rule) (fsList : List (String × Timestamp)) (tgt : Option TargetName) :
      (List Rule × List (String × Timestamp) × Option TargetName) :=
    match ls with
    | [] => (rules.reverse, fsList, tgt)
    | line :: rest =>
      let parts := (line.trimAscii.toString.splitOn " ").filter (fun s => !s.isEmpty)
      match parts with
      | "RULE" :: target :: isPhonyStr :: hasCmdsStr :: prereqs =>
        let isPhony := isPhonyStr == "1"
        let cmds := if hasCmdsStr == "1" then ["build"] else []
        let r : Rule := { target := target, prereqs := prereqs, commands := cmds, isPhony := isPhony }
        loop rest (r :: rules) fsList tgt
      | "FS" :: name :: mtimeStr :: _ =>
        let mtime := mtimeStr.toNat?.getD 0
        loop rest rules ((name, mtime) :: fsList) tgt
      | "TARGET" :: name :: _ =>
        loop rest rules fsList (some name)
      | _ => loop rest rules fsList tgt
  let (rules, fsList, tgt) := loop lines [] [] none
  let mf : Makefile := { rules := rules, variables := [], defaultTarget := tgt }
  (mf, fsList, tgt)

def runEval (specFile : String) : IO Unit := do
  let content ← IO.FS.readFile specFile
  let (mf, fsList, tgtOpt) := parseSpec content
  let fs : Filesystem := fun name =>
    match fsList.find? (fun (n, _) => n == name) with
    | some (_, mt) => FileState.present mt
    | none => FileState.missing
  let maxInitialMtime : Timestamp := fsList.foldl (fun acc (_, mt) => Nat.max acc mt) 0

  let G := mf.toDepGraph
  match detectCycles G with
  | CycleResult.cycleDetected c =>
    IO.println s!"OUTCOME cycleDetected {c}"
  | CycleResult.fuelExhausted =>
    IO.println "OUTCOME fuelExhausted"
  | CycleResult.acyclic _ =>
    let (finalSt, outcome) := runMake mf fs maxInitialMtime tgtOpt
    let mut rebuiltList : List String := []
    let mut upToDateList : List String := []
    let mut failedList : List String := []

    for (t, out) in finalSt.outcomes do
      match out with
      | TargetOutcome.rebuilt _ => rebuiltList := t :: rebuiltList
      | TargetOutcome.upToDate _ => upToDateList := t :: upToDateList
      | TargetOutcome.failed msg => failedList := s!"{t}:{msg}" :: failedList

    let outcomeStr := match outcome with
      | TargetOutcome.rebuilt _ => "rebuilt"
      | TargetOutcome.upToDate _ => "upToDate"
      | TargetOutcome.failed m => s!"failed:{m}"

    IO.println s!"OUTCOME {outcomeStr}"
    IO.println s!"REBUILT {String.intercalate " " rebuiltList}"
    IO.println s!"UPTODATE {String.intercalate " " upToDateList}"
    -- Targets whose recipe ran (recipe-less targets can be rebuilt without
    -- running anything); this is what `make` prints for.
    IO.println s!"RAN {String.intercalate " " finalSt.ran.reverse}"
    if !failedList.isEmpty then
      IO.println s!"FAILED {String.intercalate " " failedList}"

/-- `--schedule FILE`: check a recorded schedule against the Scheduling model.
Input lines: `SLOTS m` and `JOB name start dur pred...` (times in µs). -/
def runSchedule (file : String) : IO Unit := do
  let content ← IO.FS.readFile file
  let mut m := 1
  let mut jobs : List (String × Nat × Nat × List String) := []
  for line in content.splitOn "\n" do
    let parts := (line.trimAscii.toString.splitOn " ").filter (fun s => !s.isEmpty)
    match parts with
    | "SLOTS" :: n :: _ => m := n.toNat?.getD 1
    | "JOB" :: name :: st :: d :: preds =>
      jobs := (name, st.toNat?.getD 0, d.toNat?.getD 0, preds) :: jobs
    | _ => pure ()
  let recs := jobs.reverse
  let look (u : String) := recs.find? (·.1 == u)
  let I : Scheduling.Instance := {
    jobs := recs.map (·.1)
    pred := fun u => ((look u).map (·.2.2.2)).getD []
    dur := fun u => ((look u).map (·.2.2.1)).getD 0
    m := m
  }
  let S : String → Nat := fun u => ((look u).map (·.2.1)).getD 0
  IO.println s!"VALID_PREC {Scheduling.checkPrec I S}"
  IO.println s!"VALID_CAP {Scheduling.checkCap I S}"
  IO.println s!"SLOTS {m}"
  IO.println s!"WORK {Scheduling.work I}"
  IO.println s!"CRIT {Scheduling.criticalPath I S}"
  IO.println s!"MAKESPAN {Scheduling.makespan I S}"

/-- `--pattern TPAT TARGET PREREQ...`: GNU make's match of a pattern rule
(`LeanMake.Pattern`). Prints `MATCH no`, or the prerequisites and `$*`. -/
def runPattern (tpat target : String) (prereqs : List String) : IO Unit := do
  let toPat (s : String) : Option Pattern.Pat :=
    match s.toList.splitOn '%' with
    | pre :: rest@(_ :: _) => some ⟨pre, (rest.intersperse ['%']).flatten⟩
    | _ => none
  match toPat tpat with
  | none => IO.println "MATCH no"
  | some tp =>
    match Pattern.matchTarget tp target.toList with
    | none => IO.println "MATCH no"
    | some (d, stem) =>
      let ps := prereqs.map fun p =>
        match toPat p with
        | some pp => String.ofList ((Pattern.Prereq.pat pp).inst d stem)
        | none => p
      IO.println "MATCH yes"
      IO.println s!"PREREQS {String.intercalate " " ps}"
      IO.println s!"STEM {String.ofList (Pattern.stemVar d stem)}"

/-- One `NODE` line of a `MAKED_DECISIONS` record. -/
structure Decision where
  target : String
  hasRule : Bool
  doubleColon : Bool
  phony : Bool
  cmds : Bool
  before : Option Nat
  out : Char
  outTime : Option Nat
  ran : Bool
  deps : List String

def parseDecision (line : String) : Option Decision := do
  let words := (line.splitOn " ").filter (· ≠ "")
  match words with
  | "NODE" :: target :: rest =>
    let field (k : String) : Option String :=
      (rest.find? (·.startsWith (k ++ "="))).map (·.drop (k.length + 1) |>.toString)
    let flag (k : String) : Bool := field k == some "1"
    let time (v : String) : Option Nat := if v == "-" then none else v.toNat?
    let outStr ← field "out"
    let outChar := outStr.front
    let outTime := time ((outStr.drop 2).toString)
    let deps := match rest.dropWhile (· ≠ "deps") with
      | _ :: ds => ds
      | [] => []
    some { target, hasRule := flag "rule", doubleColon := flag "dcolon",
           phony := flag "phony", cmds := flag "cmds",
           before := (field "before").bind time, out := outChar, outTime, ran := flag "ran", deps }
  | _ => none

/-- The outcome a prerequisite's record stands for, as the model sees it. -/
def Decision.outcome (d : Decision) : TargetOutcome :=
  match d.out with
  | 'R' => TargetOutcome.rebuilt (d.outTime.getD 0)
  | 'F' => TargetOutcome.failed "failed"
  | _ => TargetOutcome.upToDate (d.outTime.getD 0)

/-- Check one make process's decisions against the model: every target
    settled once, after its prerequisites, and rebuilt exactly when the
    model's `needsRebuild` says so given the same inputs. Returns the
    number checked, the number skipped (a prerequisite with no record) and
    the problems found. -/
def checkRun (alwaysMake : Bool) (nodes : Array Decision) : Nat × Nat × List String := Id.run do
  let mut seen : Std.HashMap String Decision := {}
  let mut checked := 0
  let mut skipped := 0
  let mut problems : List String := []
  for d in nodes do
    if seen.contains d.target then
      problems := s!"{d.target}: settled twice in one run" :: problems
    let depRecs := d.deps.map (seen.get? ·)
    if d.hasRule then
      -- The model has no double-colon rules (each runs on its own).
      if d.doubleColon || depRecs.any (·.isNone) then
        skipped := skipped + 1
      else
        checked := checked + 1
        let depOutcomes := (d.deps.zip (depRecs.filterMap id)).map (fun (p : String × Decision) => (p.1, p.2.outcome))
        let failedDep := depOutcomes.any (fun (_, o) => match o with | .failed _ => true | _ => false)
        if failedDep then
          if d.out != 'F' || d.ran then
            problems := s!"{d.target}: a prerequisite failed, but maked made it (out={d.out}, ran={d.ran})" :: problems
        else
          -- One step of the model (`executeRule`) on the same inputs: with
          -- a recipe, it runs exactly when the model rebuilds; without one,
          -- the outcome is "rebuilt" exactly when the model's is.
          let rule : Rule := { target := d.target, prereqs := d.deps,
                               commands := if d.cmds then ["recipe"] else [], isPhony := d.phony }
          let st : BuildState := {
            fs := fun n => if n == d.target then
                (match d.before with | some t => FileState.present t | none => FileState.missing)
              else FileState.missing,
            outcomes := [], visiting := [], clock := 0 }
          let modelRebuilt := alwaysMake ||
            (match (executeRule rule depOutcomes st).2 with | .rebuilt _ => true | _ => false)
          let makedDid := if d.cmds then d.ran else d.out == 'R'
          if modelRebuilt != makedDid then
            problems := s!"{d.target}: model rebuilds={modelRebuilt}, maked {if d.cmds then "ran the recipe" else "rebuilt"}={makedDid} (before={d.before}, deps={d.deps})" :: problems
    else
      checked := checked + 1
      -- A file without a rule: up to date if it exists, an error if not.
      let ok := match d.before, d.out with
        | some _, 'U' => true
        | none, 'F' => true
        | _, _ => false
      if !ok then
        problems := s!"{d.target}: no rule, before={d.before}, but out={d.out}" :: problems
    seen := seen.insert d.target d
  (checked, skipped, problems.reverse)

/-- `--check-decisions FILE...`: records written by maked under
    `MAKED_DECISIONS` (one file per make process, `RUN` lines between
    invocations). -/
def runCheckDecisions (files : List String) : IO UInt32 := do
  let mut checked := 0
  let mut skipped := 0
  let mut runs := 0
  let mut problems : List String := []
  for f in files do
    let content ← IO.FS.readFile f
    let mut nodes : Array Decision := #[]
    let mut always := false
    let flush := fun (nodes : Array Decision) (always : Bool) => checkRun always nodes
    for line in (content.splitOn "\n") ++ ["RUN end mode="] do
      if line.startsWith "RUN " then
        if !nodes.isEmpty then
          let (c, s, p) := flush nodes always
          checked := checked + c
          skipped := skipped + s
          problems := problems ++ p.map (s!"{f}: " ++ ·)
          runs := runs + 1
        nodes := #[]
        always := (line.splitOn "mode=").getLast? |>.any (·.contains 'B')
      else
        match parseDecision line with
        | some d => nodes := nodes.push d
        | none => pure ()
  for p in problems.take 20 do
    IO.println s!"  [-] {p}"
  IO.println s!"DECISIONS runs={runs} checked={checked} skipped={skipped} problems={problems.length}"
  return if problems.isEmpty then 0 else 1

def main (args : List String) : IO UInt32 := do
  match args with
  | "--check-decisions" :: files =>
    runCheckDecisions files
  | "--pattern" :: tpat :: target :: prereqs => do
    runPattern tpat target prereqs
    return 0
  | ["--schedule", file] => do
    runSchedule file
    return 0
  | ["--eval", specFile] => do
    runEval specFile
    return 0
  | _ => do
    IO.println "=== Lean 4 Make Formal Semantics Evaluation (Audited & Fixed) ==="
    let G := sampleMakefile.toDepGraph
    IO.println s!"Graph nodes (including leaf files): {G.nodes}"

    match detectCycles G with
    | CycleResult.acyclic topo =>
      IO.println s!"Cycle check: PASSED. Topological order: {topo}"
    | CycleResult.cycleDetected c =>
      IO.println s!"Cycle check: FAILED. Cycle trace: {c}"
    | CycleResult.fuelExhausted =>
      IO.println "Cycle check: FUEL EXHAUSTED"

    IO.println "\nTesting cyclic graph detection with forward edge trace:"
    let cyclicMakefile : Makefile := {
      rules := [
        { target := "A", prereqs := ["B"], commands := [] },
        { target := "B", prereqs := ["C"], commands := [] },
        { target := "C", prereqs := ["A"], commands := [] }
      ],
      variables := [],
      defaultTarget := some "A"
    }
    match detectCycles cyclicMakefile.toDepGraph with
    | CycleResult.acyclic _ =>
      IO.println "Unexpected: cycle not detected!"
    | CycleResult.fuelExhausted =>
      IO.println "Unexpected: fuel exhausted!"
    | CycleResult.cycleDetected c =>
      IO.println s!"Cycle successfully caught! Forward directed cycle trace: {c}"

    IO.println "\nEvaluating target 'all' with formal operational semantics and epoch timestamps:"
    let maxInitialMtime : Timestamp := 1700000120
    let (finalSt, outcome) := runMake sampleMakefile initialFs maxInitialMtime (some "all")

    match outcome with
    | TargetOutcome.upToDate t =>
      IO.println s!"Result: 'all' is up to date (timestamp {t})"
    | TargetOutcome.rebuilt t =>
      IO.println s!"Result: 'all' successfully rebuilt (timestamp {t})"
    | TargetOutcome.failed err =>
      IO.println s!"Result: build failed with error: {err}"

    IO.println "\nOutcomes for all targets evaluated:"
    for (t, out) in finalSt.outcomes do
      match out with
      | TargetOutcome.upToDate tstamp =>
        IO.println s!"  {t}: UP-TO-DATE (timestamp {tstamp})"
      | TargetOutcome.rebuilt tstamp =>
        IO.println s!"  {t}: REBUILT (timestamp {tstamp})"
      | TargetOutcome.failed msg =>
        IO.println s!"  {t}: FAILED ({msg})"

    IO.println "\nLean 4 formal model execution complete."
    return 0
