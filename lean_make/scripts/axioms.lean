-- CI: the headline theorems must rest only on Lean's standard axioms.
-- Run with `lake env lean scripts/axioms.lean`; CI fails on `sorryAx`.
import LeanMake
open LeanMake LeanMake.Scheduling
#print axioms work_le_slots_mul_makespan
#print axioms chain_dur_le_finish
#print axioms greedy_makespan_bound
#print axioms checkValid_sound
#print axioms schedule_bounded_by_path
#print axioms executeWithCAS_idempotent
#print axioms executeRule_upToDate_idempotent
