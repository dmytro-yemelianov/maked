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

def main (args : List String) : IO Unit := do
  match args with
  | ["--schedule", file] =>
    runSchedule file
  | ["--eval", specFile] =>
    runEval specFile
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
