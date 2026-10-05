-- CI: the headline theorems must rest only on Lean's standard axioms.
-- Run with `lake env lean scripts/axioms.lean`; CI fails on `sorryAx`.
import LeanMake
open LeanMake LeanMake.Scheduling LeanMake.RunOnce LeanMake.Pattern
#print axioms work_le_slots_mul_makespan
#print axioms chain_dur_le_finish
#print axioms greedy_makespan_bound
#print axioms greedy_makespan_bound_tight
#print axioms checkValid_sound
#print axioms schedule_bounded_by_path
#print axioms executeWithCAS_idempotent
#print axioms executeRule_upToDate_idempotent
#print axioms no_recipe_runs_twice
#print axioms remade_with_makefiles_is_done
#print axioms evalTarget_inv
#print axioms missing_target_always_remade
#print axioms force_rule_rebuilt
#print axioms recipeless_present_keeps_mtime
#print axioms matchTarget_sound
#print axioms matchTarget_stem_no_slash
#print axioms whole_path_agrees
#print axioms whole_path_differs
#print axioms Pat.stem_inst
