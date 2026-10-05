/-
  LeanMake.Syntax
  Formal AST and Syntax specification of the Make utility.
-/

namespace LeanMake

/-- A Target identifier is represented as a String -/
abbrev TargetName := String

/-- A shell command recipe -/
abbrev Command := String

/-- A Make Rule specifies a target, its prerequisite dependencies,
    associated recipe commands, and whether it is marked .PHONY. -/
structure Rule where
  target   : TargetName
  prereqs  : List TargetName
  commands : List Command
  isPhony  : Bool := false
deriving Repr, DecidableEq, Inhabited

/-- Variable environment mapping variable names to their expanded string values -/
abbrev VarEnv := List (String × String)

/-- Lookup a variable in the environment, returning default empty string if absent -/
def VarEnv.lookup (env : VarEnv) (key : String) : String :=
  match env.find? (fun (k, _) => k == key) with
  | some (_, v) => v
  | none => ""

/-- A Makefile consists of a set of rules, variable definitions, and an optional default target -/
structure Makefile where
  rules         : List Rule
  variables     : VarEnv
  defaultTarget : Option TargetName
deriving Repr, Inhabited

/-- Find the rule corresponding to a target name in the Makefile -/
def Makefile.findRule (mf : Makefile) (t : TargetName) : Option Rule :=
  mf.rules.find? (fun r => r.target == t)

/-- Total (non-partial) variable expansion bounded by input character count -/
def expandVars (env : VarEnv) (s : String) : String :=
  let chars := s.toList
  let rec loop (fuel : Nat) (cs : List Char) (acc : List Char) : List Char :=
    match fuel, cs with
    | 0, _ => acc.reverse
    | _, [] => acc.reverse
    | fuel + 1, '$' :: '(' :: rest =>
      let rec takeVar (f : Nat) (rem : List Char) (vname : List Char) : (List Char × List Char) :=
        match f, rem with
        | 0, _ => (vname.reverse, rem)
        | _, [] => (vname.reverse, [])
        | _, ')' :: afterParen => (vname.reverse, afterParen)
        | f + 1, c :: afterC => takeVar f afterC (c :: vname)
      let (vnameChars, remainder) := takeVar fuel rest []
      let val := (env.lookup (String.ofList vnameChars)).toList
      loop fuel remainder (val.reverse ++ acc)
    | fuel + 1, c :: rest => loop fuel rest (c :: acc)
  String.ofList (loop (chars.length * 2 + 10) chars [])

end LeanMake
