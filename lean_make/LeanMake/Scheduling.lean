/-
  LeanMake.Scheduling
  Bounded-parallelism (`-j m`) schedules and how good a greedy scheduler is.

  Model: discrete time. A schedule gives every job a start time `S u`; job
  `u` occupies one of `m` slots during `[S u, S u + dur u)`. A schedule is
  valid when every prerequisite finishes before its dependent starts and at
  most `m` jobs run at any instant.

  Results:
  1. `work_le_slots_mul_makespan`: any valid schedule that finishes by `C`
     has `W ≤ m * C`, where `W` is the total work. With
     `chain_dur_le_finish` (a dependency chain cannot run faster than its
     total duration), this gives the lower bound
     `C ≥ max (W / m) (critical path)` on every schedule, so on the optimum.
  2. `greedy_makespan_bound` (Graham's list-scheduling bound): a valid
     schedule that never leaves a ready job waiting while a slot is free
     satisfies `m * C ≤ W + m * L`, i.e. `C ≤ W / m + L`, where `L` bounds
     the longest dependency chain. Because the optimum is at least
     `max (W / m) L` when `L` is the exact critical path, a greedy schedule
     is within 2× of optimal.

  What this does not say: the theorems are about this model. That maked's
  executor is greedy in this sense is an argument about the Rust code
  (ready targets go straight to a shared queue served by `m` workers), not a
  proof. `maked --profile` reports the measured makespan against both
  bounds.
-/

import LeanMake.Syntax

namespace LeanMake.Scheduling

/-- A scheduling instance: jobs, their prerequisites, durations and `m` slots. -/
structure Instance where
  jobs : List TargetName
  pred : TargetName → List TargetName
  dur  : TargetName → Nat
  m    : Nat

/-- `Σ_{t < n} f t`. -/
def sumTo (f : Nat → Nat) : Nat → Nat
  | 0 => 0
  | n + 1 => sumTo f n + f n

/-- `Σ_{u ∈ l} f u`. -/
def sumList (l : List TargetName) (f : TargetName → Nat) : Nat :=
  (l.map f).sum

variable (I : Instance) (S : TargetName → Nat)

/-- Finish time of a job. -/
def fin (u : TargetName) : Nat := S u + I.dur u

/-- 1 if job `u` occupies a slot at time `t`, else 0. -/
def runningAt (t : Nat) (u : TargetName) : Nat :=
  if S u ≤ t ∧ t < S u + I.dur u then 1 else 0

/-- Number of jobs running at time `t`. -/
def busy (t : Nat) : Nat := sumList I.jobs (runningAt I S t)

/-- Total work. -/
def work : Nat := sumList I.jobs I.dur

/-- 1 if some slot is free at time `t`. -/
def idleAt (t : Nat) : Nat := if busy I S t < I.m then 1 else 0

/-- Number of instants before `n` with a free slot. -/
def idleBefore (n : Nat) : Nat := sumTo (idleAt I S) n

/-- Job `v` may start at `t`: all of its prerequisites have finished. -/
def Ready (t : Nat) (v : TargetName) : Prop :=
  ∀ p ∈ I.pred v, fin I S p ≤ t

/-- Precedence and the `m`-slot capacity. Prerequisites of jobs are jobs. -/
structure Valid : Prop where
  pred_mem : ∀ v ∈ I.jobs, ∀ p ∈ I.pred v, p ∈ I.jobs
  prec     : ∀ v ∈ I.jobs, ∀ p ∈ I.pred v, fin I S p ≤ S v
  cap      : ∀ t, busy I S t ≤ I.m

/-- List scheduling: a ready job only waits while every slot is busy. -/
def Greedy : Prop :=
  ∀ v ∈ I.jobs, ∀ t, t < S v → Ready I S t v → busy I S t = I.m

/-! ### Finite sums -/

theorem sumList_add (l : List TargetName) (f g : TargetName → Nat) :
    sumList l (fun u => f u + g u) = sumList l f + sumList l g := by
  induction l with
  | nil => simp [sumList]
  | cons a l ih => simp [sumList] at *; omega

theorem sumList_zero (l : List TargetName) : sumList l (fun _ => 0) = 0 := by
  induction l with
  | nil => simp [sumList]
  | cons a l ih => simp [sumList] at *; omega

theorem sumTo_sumList_swap (l : List TargetName) (g : Nat → TargetName → Nat) (n : Nat) :
    sumTo (fun t => sumList l (g t)) n = sumList l (fun u => sumTo (fun t => g t u) n) := by
  induction n with
  | zero => simp [sumTo, sumList_zero]
  | succ n ih =>
    simp only [sumTo]
    rw [ih, ← sumList_add]

theorem sumList_le_mono (l : List TargetName) (f g : TargetName → Nat)
    (h : ∀ u ∈ l, f u ≤ g u) : sumList l f ≤ sumList l g := by
  induction l with
  | nil => simp [sumList]
  | cons a l ih =>
    simp only [sumList, List.map_cons, List.sum_cons] at *
    have h1 := h a (by simp)
    have h2 := ih (fun u hu => h u (by simp [hu]))
    omega

theorem sumList_congr (l : List TargetName) (f g : TargetName → Nat)
    (h : ∀ u ∈ l, f u = g u) : sumList l f = sumList l g := by
  have h1 := sumList_le_mono l f g (fun u hu => by rw [h u hu]; exact Nat.le_refl _)
  have h2 := sumList_le_mono l g f (fun u hu => by rw [h u hu]; exact Nat.le_refl _)
  omega

theorem sumTo_le_mono (f g : Nat → Nat) (h : ∀ t, f t ≤ g t) (n : Nat) :
    sumTo f n ≤ sumTo g n := by
  induction n with
  | zero => simp [sumTo]
  | succ n ih => simp only [sumTo]; have := h n; omega

theorem sumTo_const (c n : Nat) : sumTo (fun _ => c) n = c * n := by
  induction n with
  | zero => simp [sumTo]
  | succ n ih => simp only [sumTo]; rw [ih, Nat.mul_succ]

theorem sumTo_add (f g : Nat → Nat) (n : Nat) :
    sumTo (fun t => f t + g t) n = sumTo f n + sumTo g n := by
  induction n with
  | zero => simp [sumTo]
  | succ n ih => simp only [sumTo]; rw [ih]; omega

theorem sumTo_mul (c : Nat) (f : Nat → Nat) (n : Nat) :
    sumTo (fun t => c * f t) n = c * sumTo f n := by
  induction n with
  | zero => simp [sumTo]
  | succ n ih => simp only [sumTo]; rw [ih, Nat.mul_add]

/-- Summing an interval indicator counts the overlap of `[s, s + d)` with `[0, n)`. -/
theorem sumTo_interval (s d n : Nat) :
    sumTo (fun t => if s ≤ t ∧ t < s + d then 1 else 0) n = min n (s + d) - min n s := by
  induction n with
  | zero => simp [sumTo]
  | succ n ih =>
    simp only [sumTo]
    rw [ih]
    by_cases h : s ≤ n ∧ n < s + d
    · simp only [h, and_self, if_true]; omega
    · simp only [h, if_false]; omega

/-- A 0/1 function adds at most `k` over an interval of length `k`. -/
theorem sumTo_le_add_of_le_one (f : Nat → Nat) (hf : ∀ t, f t ≤ 1) (a k : Nat) :
    sumTo f (a + k) ≤ sumTo f a + k := by
  induction k with
  | zero => simp
  | succ k ih =>
    rw [← Nat.add_assoc]
    simp only [sumTo]
    have := hf (a + k)
    omega

/-- A function that vanishes on `[a, b)` adds nothing there. -/
theorem sumTo_eq_of_zero_between (f : Nat → Nat) (a b : Nat) (hab : a ≤ b)
    (hz : ∀ t, a ≤ t → t < b → f t = 0) : sumTo f b = sumTo f a := by
  induction b with
  | zero =>
    have : a = 0 := by omega
    subst this; rfl
  | succ b ih =>
    by_cases h : a ≤ b
    · simp only [sumTo]
      rw [ih h (fun t h1 h2 => hz t h1 (by omega)), hz b h (by omega), Nat.add_zero]
    · have : a = b + 1 := by omega
      subst this; rfl

/-! ### 1. Lower bounds on every valid schedule -/

/-- Over a horizon that contains every job, total busy time is the total work. -/
theorem sumTo_busy_eq_work (C : Nat) (hC : ∀ u ∈ I.jobs, fin I S u ≤ C) :
    sumTo (busy I S) C = work I := by
  unfold busy work
  rw [sumTo_sumList_swap]
  apply sumList_congr
  intro u hu
  have hfin := hC u hu
  unfold fin at hfin
  have := sumTo_interval (S u) (I.dur u) C
  simp only [runningAt]
  rw [this]
  omega

/-- Lower bound 1: with `m` slots, a valid schedule finishing by `C` has `W ≤ m * C`. -/
theorem work_le_slots_mul_makespan (hV : Valid I S) (C : Nat)
    (hC : ∀ u ∈ I.jobs, fin I S u ≤ C) : work I ≤ I.m * C := by
  rw [← sumTo_busy_eq_work I S C hC, ← sumTo_const]
  exact sumTo_le_mono _ _ hV.cap C

/-- A dependency chain `[u₀, u₁, …, uₖ]`, each a prerequisite of the next. -/
def IsChain : List TargetName → Prop
  | [] => True
  | [_] => True
  | a :: b :: rest => a ∈ I.pred b ∧ IsChain (b :: rest)

/-- Lower bound 2: a chain cannot finish before the sum of its durations. -/
theorem chain_dur_le_finish (hV : Valid I S) :
    ∀ (a : TargetName) (rest : List TargetName),
      (∀ u ∈ a :: rest, u ∈ I.jobs) → IsChain I (a :: rest) →
      sumList (a :: rest) I.dur ≤ fin I S ((a :: rest).getLast (by simp)) := by
  intro a rest
  induction rest generalizing a with
  | nil =>
    intro _ _
    simp [sumList, fin]
  | cons b rest ih =>
    intro hmem hchain
    have hab : a ∈ I.pred b := hchain.1
    have hrest : IsChain I (b :: rest) := hchain.2
    have hmemb : ∀ u ∈ b :: rest, u ∈ I.jobs := fun u hu => hmem u (by simp [hu])
    have ihb := ih b hmemb hrest
    have hprec := hV.prec b (hmem b (by simp)) a hab
    have hlast : (a :: b :: rest).getLast (by simp) = (b :: rest).getLast (by simp) := by
      simp [List.getLast_cons]
    rw [hlast]
    have hsum : sumList (a :: b :: rest) I.dur = I.dur a + sumList (b :: rest) I.dur := by
      simp [sumList]
    rw [hsum]
    -- dur a ≤ fin a ≤ S b, and the rest of the chain needs S b + its work.
    have key : ∀ (c : TargetName) (r : List TargetName), (∀ u ∈ c :: r, u ∈ I.jobs) →
        IsChain I (c :: r) →
        sumList (c :: r) I.dur + S c ≤ fin I S ((c :: r).getLast (by simp)) := by
      intro c r
      induction r generalizing c with
      | nil => intro _ _; simp [sumList, fin]; omega
      | cons d r ihr =>
        intro hm hc
        have hcd : c ∈ I.pred d := hc.1
        have hp := hV.prec d (hm d (by simp)) c hcd
        have ihd := ihr d (fun u hu => hm u (by simp [hu])) hc.2
        have hl : (c :: d :: r).getLast (by simp) = (d :: r).getLast (by simp) := by
          simp [List.getLast_cons]
        rw [hl]
        have hs : sumList (c :: d :: r) I.dur = I.dur c + sumList (d :: r) I.dur := by
          simp [sumList]
        rw [hs]
        unfold fin at hp
        omega
    have := key b rest hmemb hrest
    unfold fin at hprec
    omega

/-! ### 2. Graham's bound for greedy (list) schedules -/

/-- `idleAt` is 0/1. -/
theorem idleAt_le_one (t : Nat) : idleAt I S t ≤ 1 := by
  unfold idleAt; split <;> omega

/-- Some prerequisite finishes last. -/
theorem exists_last_finisher (l : List TargetName) (h : l ≠ []) :
    ∃ p ∈ l, ∀ q ∈ l, fin I S q ≤ fin I S p := by
  induction l with
  | nil => exact absurd rfl h
  | cons a l ih =>
    by_cases hl : l = []
    · subst hl; exact ⟨a, by simp, fun q hq => by simp at hq; subst hq; exact Nat.le_refl _⟩
    · obtain ⟨p, hp, hmax⟩ := ih hl
      by_cases hap : fin I S p ≤ fin I S a
      · refine ⟨a, by simp, fun q hq => ?_⟩
        simp at hq
        rcases hq with rfl | hq
        · exact Nat.le_refl _
        · exact Nat.le_trans (hmax q hq) hap
      · refine ⟨p, by simp [hp], fun q hq => ?_⟩
        simp at hq
        rcases hq with rfl | hq
        · omega
        · exact hmax q hq

/--
  Key lemma: in a greedy schedule, the instants before job `v` starts with a
  free slot are at most `H v`, for any `H` that dominates the longest chain
  of prerequisites ending just before `v` (`H p + dur p ≤ H v`). Durations
  are positive (every job takes at least one tick).
-/
theorem idleBefore_start_le (hV : Valid I S) (hG : Greedy I S)
    (hpos : ∀ u ∈ I.jobs, 1 ≤ I.dur u)
    (H : TargetName → Nat) (hH : ∀ v ∈ I.jobs, ∀ p ∈ I.pred v, H p + I.dur p ≤ H v) :
    ∀ n, ∀ v ∈ I.jobs, S v = n → idleBefore I S (S v) ≤ H v := by
  intro n
  induction n using Nat.strongRecOn with
  | _ n ih =>
    intro v hv hSv
    by_cases hnil : I.pred v = []
    · -- No prerequisites: v is ready from time 0, so no instant before it is idle.
      have hz : ∀ t, 0 ≤ t → t < S v → idleAt I S t = 0 := by
        intro t _ ht
        have hready : Ready I S t v := by intro p hp; rw [hnil] at hp; simp at hp
        have := hG v hv t ht hready
        unfold idleAt; rw [this]; simp
      have := sumTo_eq_of_zero_between (idleAt I S) 0 (S v) (Nat.zero_le _) hz
      unfold idleBefore; rw [this]; simp [sumTo]
    · obtain ⟨p, hp, hmax⟩ := exists_last_finisher I S (I.pred v) hnil
      have hpj : p ∈ I.jobs := hV.pred_mem v hv p hp
      have hfp : fin I S p ≤ S v := hV.prec v hv p hp
      -- From fin p on, v is ready, so every slot is busy until v starts.
      have hz : ∀ t, fin I S p ≤ t → t < S v → idleAt I S t = 0 := by
        intro t h1 h2
        have hready : Ready I S t v := fun q hq => Nat.le_trans (hmax q hq) h1
        have := hG v hv t h2 hready
        unfold idleAt; rw [this]; simp
      have hEq := sumTo_eq_of_zero_between (idleAt I S) (fin I S p) (S v) hfp hz
      -- Between S p and fin p at most dur p instants are idle.
      have hStep := sumTo_le_add_of_le_one (idleAt I S) (idleAt_le_one I S) (S p) (I.dur p)
      have hpos_p := hpos p hpj
      have hlt : S p < n := by unfold fin at hfp; omega
      have ihp := ih (S p) hlt p hpj rfl
      have hHp := hH v hv p hp
      unfold idleBefore at *
      unfold fin at hEq
      rw [hEq]
      omega

/--
  Graham's bound. Let `C` be the makespan, reached by job `j`, and `L` bound
  every chain (`H u + dur u ≤ L`). A greedy valid schedule satisfies
  `m * C ≤ W + m * L`: at most `L` instants have a free slot, and every
  other instant does `m` units of the `W` total work.
-/
theorem greedy_makespan_bound (hV : Valid I S) (hG : Greedy I S)
    (hpos : ∀ u ∈ I.jobs, 1 ≤ I.dur u)
    (H : TargetName → Nat) (hH : ∀ v ∈ I.jobs, ∀ p ∈ I.pred v, H p + I.dur p ≤ H v)
    (L : Nat) (hL : ∀ u ∈ I.jobs, H u + I.dur u ≤ L)
    (C : Nat) (hC : ∀ u ∈ I.jobs, fin I S u ≤ C)
    (j : TargetName) (hj : j ∈ I.jobs) (hjC : fin I S j = C) :
    I.m * C ≤ work I + I.m * L := by
  -- Every instant: busy + m * idle ≥ m (a full instant is busy = m by capacity).
  have hpt : ∀ t, I.m ≤ busy I S t + I.m * idleAt I S t := by
    intro t
    unfold idleAt
    split
    · omega
    · omega
  have hsum : sumTo (fun _ => I.m) C ≤ sumTo (fun t => busy I S t + I.m * idleAt I S t) C :=
    sumTo_le_mono _ _ hpt C
  rw [sumTo_const, sumTo_add, sumTo_mul, sumTo_busy_eq_work I S C hC] at hsum
  -- Idle instants before C: those before j starts, plus at most dur j.
  have hidle_j := idleBefore_start_le I S hV hG hpos H hH (S j) j hj rfl
  have hstep := sumTo_le_add_of_le_one (idleAt I S) (idleAt_le_one I S) (S j) (I.dur j)
  unfold fin at hjC
  rw [hjC] at hstep
  have hLj := hL j hj
  unfold idleBefore at hidle_j
  have hidle : sumTo (idleAt I S) C ≤ L := by omega
  have := Nat.mul_le_mul_left I.m hidle
  omega

/-! ### 3. An executable checker for recorded schedules

`maked --trace` records when every job started and how long it ran. The
fuzzer feeds those schedules to `lean_make --schedule`, which uses the
functions below: the same `busy`, `fin` and `Valid` the theorems are about.
Capacity is checked only at job start times. `checkValid_sound` shows that
this suffices: `busy` only rises when some job starts. -/

/-- Some element of a non-empty list maximizes `k`. -/
theorem exists_max_by (k : TargetName → Nat) (l : List TargetName) (h : l ≠ []) :
    ∃ p ∈ l, ∀ q ∈ l, k q ≤ k p := by
  induction l with
  | nil => exact absurd rfl h
  | cons a l ih =>
    by_cases hl : l = []
    · subst hl; exact ⟨a, by simp, fun q hq => by simp at hq; subst hq; exact Nat.le_refl _⟩
    · obtain ⟨p, hp, hmax⟩ := ih hl
      by_cases hap : k p ≤ k a
      · refine ⟨a, by simp, fun q hq => ?_⟩
        simp at hq
        rcases hq with rfl | hq
        · exact Nat.le_refl _
        · exact Nat.le_trans (hmax q hq) hap
      · refine ⟨p, by simp [hp], fun q hq => ?_⟩
        simp at hq
        rcases hq with rfl | hq
        · omega
        · exact hmax q hq

/-- If no more than `m` jobs run at any job's start, none runs over `m` ever. -/
theorem cap_of_cap_at_starts (hs : ∀ u ∈ I.jobs, busy I S (S u) ≤ I.m) :
    ∀ t, busy I S t ≤ I.m := by
  intro t
  let R := I.jobs.filter (fun w => decide (S w ≤ t ∧ t < S w + I.dur w))
  by_cases hR : R = []
  · have hz : ∀ w ∈ I.jobs, runningAt I S t w ≤ 0 := by
      intro w hw
      unfold runningAt
      split
      · rename_i hrun
        have : w ∈ R := List.mem_filter.mpr ⟨hw, by simp [hrun]⟩
        rw [hR] at this; simp at this
      · exact Nat.le_refl _
    have := sumList_le_mono I.jobs (runningAt I S t) (fun _ => 0) hz
    rw [sumList_zero] at this
    unfold busy; omega
  · obtain ⟨u, huR, hmax⟩ := exists_max_by S R hR
    have hu := List.mem_filter.mp huR
    have hsu : S u ≤ t := by simpa using (of_decide_eq_true hu.2).1
    have hmono : ∀ w ∈ I.jobs, runningAt I S t w ≤ runningAt I S (S u) w := by
      intro w hw
      unfold runningAt
      split
      · rename_i hrun
        have hwR : w ∈ R := List.mem_filter.mpr ⟨hw, by simp [hrun]⟩
        have := hmax w hwR
        have hrun' : S w ≤ S u ∧ S u < S w + I.dur w := ⟨this, by omega⟩
        simp [hrun']
      · exact Nat.zero_le _
    have := sumList_le_mono I.jobs _ _ hmono
    have hcap := hs u hu.1
    unfold busy at *
    omega

/-- Executable precedence check (prerequisites are jobs and finish first). -/
def checkPrec : Bool :=
  I.jobs.all (fun v => (I.pred v).all (fun p => decide (p ∈ I.jobs) && decide (fin I S p ≤ S v)))

/-- Executable capacity check, at job start times only. -/
def checkCap : Bool := I.jobs.all (fun u => decide (busy I S (S u) ≤ I.m))

def checkValid : Bool := checkPrec I S && checkCap I S

/-- Soundness: a schedule the checker accepts is valid in the model. -/
theorem checkValid_sound (h : checkValid I S = true) : Valid I S := by
  unfold checkValid at h
  simp only [Bool.and_eq_true] at h
  obtain ⟨hp, hc⟩ := h
  unfold checkPrec at hp
  unfold checkCap at hc
  simp only [List.all_eq_true, Bool.and_eq_true, decide_eq_true_eq] at hp hc
  exact {
    pred_mem := fun v hv p hpv => (hp v hv p hpv).1
    prec := fun v hv p hpv => (hp v hv p hpv).2
    cap := cap_of_cap_at_starts I S hc
  }

/-- Longest chain ending in each job, processing jobs in start order (a
topological order for any valid schedule with positive durations). Returns
the critical path `max (head v + dur v)`. -/
def criticalPath : Nat :=
  let order := I.jobs.mergeSort (fun a b => decide (S a ≤ S b))
  let heads := order.foldl (fun (acc : List (TargetName × Nat)) v =>
    let h := (I.pred v).foldl (fun m p =>
      max m (((acc.find? (·.1 == p)).map (·.2)).getD 0 + I.dur p)) 0
    (v, h) :: acc) []
  heads.foldl (fun m (v, h) => max m (h + I.dur v)) 0

/-- Latest finish time. -/
def makespan : Nat := I.jobs.foldl (fun m u => max m (fin I S u)) 0

end LeanMake.Scheduling
