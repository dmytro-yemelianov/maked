/-
  LeanMake.Pattern
  Pattern rules (`%.o: %.c`) as GNU make matches them.

  A pattern `pre%suf` matches a name `pre ++ stem ++ suf`. When the target
  pattern has no `/`, GNU make matches it against the file name alone: the
  directory comes off first and goes back in front of every prerequisite
  made from a pattern, and `$*` is the directory plus the stem. So with
  `%.o: src/%.c`, `a/b.o` is made from `a/src/b.c`.

  maked v0.2.1 matched the whole path instead (`src/a/b.c`). The two
  readings agree on the everyday rules (`%.o: %.c`), which is why the bug
  survived: `whole_path_agrees` proves they coincide whenever both patterns
  start with `%`, and `whole_path_differs` is a rule where they do not.

  Names are `List Char`, so everything here reduces in the kernel.
-/

namespace LeanMake
namespace Pattern

abbrev Name := List Char

/-- `stripPrefix p t = some r` iff `t = p ++ r`. -/
def stripPrefix : Name → Name → Option Name
  | [], t => some t
  | _ :: _, [] => none
  | c :: cs, d :: ds => if c = d then stripPrefix cs ds else none

theorem stripPrefix_sound : ∀ (p t r : Name), stripPrefix p t = some r → t = p ++ r
  | [], t, r, h => by simp [stripPrefix] at h; simp [h]
  | _ :: _, [], _, h => by simp [stripPrefix] at h
  | c :: cs, d :: ds, r, h => by
    unfold stripPrefix at h
    split at h
    · rename_i hcd
      subst hcd
      simp [stripPrefix_sound cs ds r h]
    · contradiction

theorem stripPrefix_append : ∀ (p r : Name), stripPrefix p (p ++ r) = some r
  | [], r => rfl
  | c :: cs, r => by simp [stripPrefix, stripPrefix_append cs r]

/-- `stripSuffix s t = some r` iff `t = r ++ s`. -/
def stripSuffix (s t : Name) : Option Name :=
  (stripPrefix s.reverse t.reverse).map List.reverse

theorem stripSuffix_sound (s t r : Name) (h : stripSuffix s t = some r) : t = r ++ s := by
  unfold stripSuffix at h
  cases hp : stripPrefix s.reverse t.reverse with
  | none => simp [hp] at h
  | some q =>
    simp [hp] at h
    have := stripPrefix_sound _ _ _ hp
    subst h
    have h2 := congrArg List.reverse this
    simpa using h2

theorem stripSuffix_append (s r : Name) : stripSuffix s (r ++ s) = some r := by
  unfold stripSuffix
  simp [List.reverse_append, stripPrefix_append]

/-- A pattern `pre%suf`. -/
structure Pat where
  pre : Name
  suf : Name
deriving DecidableEq, Repr

/-- The stem when `t` matches the pattern. -/
def Pat.stem (p : Pat) (t : Name) : Option Name :=
  (stripPrefix p.pre t).bind (stripSuffix p.suf)

/-- Put a stem into the pattern. -/
def Pat.inst (p : Pat) (stem : Name) : Name := p.pre ++ stem ++ p.suf

theorem Pat.stem_sound (p : Pat) (t s : Name) (h : p.stem t = some s) : t = p.inst s := by
  unfold Pat.stem at h
  cases hp : stripPrefix p.pre t with
  | none => simp [hp] at h
  | some rest =>
    simp [hp] at h
    rw [stripPrefix_sound _ _ _ hp, stripSuffix_sound _ _ _ h]
    simp [Pat.inst]

theorem Pat.stem_inst (p : Pat) (s : Name) : p.stem (p.inst s) = some s := by
  simp [Pat.stem, Pat.inst, List.append_assoc, stripPrefix_append, stripSuffix_append]

/-- Split a path after its last `/`: the directory (with the `/`) and the name. -/
def splitDir (t : Name) : Name × Name :=
  ((t.reverse.dropWhile (· ≠ '/')).reverse, (t.reverse.takeWhile (· ≠ '/')).reverse)

theorem splitDir_append (t : Name) : (splitDir t).1 ++ (splitDir t).2 = t := by
  unfold splitDir
  rw [← List.reverse_append, List.takeWhile_append_dropWhile, List.reverse_reverse]

theorem splitDir_name_no_slash (t : Name) : '/' ∉ (splitDir t).2 := by
  unfold splitDir
  intro h
  rw [List.mem_reverse] at h
  have hall := List.all_takeWhile (p := fun x => decide (x ≠ '/')) (l := t.reverse)
  have := List.all_eq_true.mp hall '/' h
  simp at this

/-- A prerequisite of a pattern rule: a pattern, or a plain name. -/
inductive Prereq where
  | pat (p : Pat)
  | plain (n : Name)
deriving DecidableEq, Repr

/-- How GNU make matches a pattern rule's target: the directory and the stem. -/
def matchTarget (tp : Pat) (t : Name) : Option (Name × Name) :=
  if '/' ∈ tp.pre ++ tp.suf then (tp.stem t).map (fun s => ([], s))
  else
    let (d, n) := splitDir t
    (tp.stem n).map (fun s => (d, s))

/-- A prerequisite for the match `(dir, stem)`: the directory goes in front
    of pattern-made names only. -/
def Prereq.inst (dir stem : Name) : Prereq → Name
  | .pat p => dir ++ p.inst stem
  | .plain n => n

/-- `$*` for the match. -/
def stemVar (dir stem : Name) : Name := dir ++ stem

/-- **The target is what the match says it is**: directory, then the pattern
    with the stem in it. -/
theorem matchTarget_sound (tp : Pat) (t d s : Name) (h : matchTarget tp t = some (d, s)) :
    t = d ++ tp.inst s := by
  unfold matchTarget at h
  split at h
  · cases hs : tp.stem t with
    | none => simp [hs] at h
    | some s' =>
      simp [hs] at h
      obtain ⟨rfl, rfl⟩ := h
      simpa using Pat.stem_sound tp t s' hs
  · cases hs : tp.stem (splitDir t).2 with
    | none => simp [hs] at h
    | some s' =>
      simp [hs] at h
      obtain ⟨rfl, rfl⟩ := h
      rw [← Pat.stem_sound tp _ s' hs, splitDir_append]

/-- With no `/` in the target pattern, the stem never contains a `/`: the
    directory is all in `d`. -/
theorem matchTarget_stem_no_slash (tp : Pat) (t d s : Name)
    (hNoSlash : '/' ∉ tp.pre ++ tp.suf) (h : matchTarget tp t = some (d, s)) : '/' ∉ s := by
  unfold matchTarget at h
  rw [if_neg hNoSlash] at h
  cases hs : tp.stem (splitDir t).2 with
  | none => simp [hs] at h
  | some s' =>
    simp [hs] at h
    obtain ⟨rfl, rfl⟩ := h
    intro hm
    apply splitDir_name_no_slash t
    rw [Pat.stem_sound tp _ s' hs]
    simp [Pat.inst, hm]

/-- maked v0.2.1: match the whole path, no directory handling. -/
def wholePathPrereq (tp : Pat) (t : Name) (pp : Prereq) : Option Name :=
  (tp.stem t).map fun s => match pp with
    | .pat p => p.inst s
    | .plain n => n

/-- **Why the bug hid**: when both patterns start with `%` (`%.o: %.c`, by
    far the usual case), matching the whole path gives the same
    prerequisite as GNU make's rule. -/
theorem whole_path_agrees (tp p : Pat) (t d s : Name)
    (hT : tp.pre = []) (hP : p.pre = [])
    (h : matchTarget tp t = some (d, s)) :
    wholePathPrereq tp t (.pat p) = some ((Prereq.pat p).inst d s) := by
  have ht := matchTarget_sound tp t d s h
  have hW : tp.stem t = some (d ++ s) := by
    rw [ht]
    unfold Pat.stem Pat.inst
    rw [hT]
    simp only [List.nil_append, stripPrefix, Option.bind_some]
    rw [← List.append_assoc, stripSuffix_append]
  simp [wholePathPrereq, hW, Prereq.inst, Pat.inst, hP]

/-- **Where it does not**: `%.o: src/%.c` with `a/b.o`. GNU make builds it
    from `a/src/b.c`; the whole-path reading wanted `src/a/b.c`. -/
theorem whole_path_differs :
    let tp : Pat := ⟨[], ['.', 'o']⟩
    let p : Pat := ⟨['s', 'r', 'c', '/'], ['.', 'c']⟩
    let t : Name := ['a', '/', 'b', '.', 'o']
    matchTarget tp t = some (['a', '/'], ['b']) ∧
    (Prereq.pat p).inst ['a', '/'] ['b'] = ['a', '/', 's', 'r', 'c', '/', 'b', '.', 'c'] ∧
    wholePathPrereq tp t (.pat p) = some ['s', 'r', 'c', '/', 'a', '/', 'b', '.', 'c'] := by
  decide

/-- `e%t` matches `src/eat` with stem `a` in directory `src/`, so `$*` is
    `src/a` (the whole-path reading does not match at all). -/
theorem dir_stem_example :
    matchTarget ⟨['e'], ['t']⟩ ['s', 'r', 'c', '/', 'e', 'a', 't'] = some (['s', 'r', 'c', '/'], ['a']) ∧
    stemVar ['s', 'r', 'c', '/'] ['a'] = ['s', 'r', 'c', '/', 'a'] ∧
    (Pat.stem ⟨['e'], ['t']⟩ ['s', 'r', 'c', '/', 'e', 'a', 't']) = none := by
  decide

end Pattern
end LeanMake
