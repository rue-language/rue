import RueCore.Explain
import RueCore.Trace

/-!
# RueCore.Explain.Ledger — the identity ledger (RUE-2428)

The step table shows a run row by row; the ledger turns it sideways, one row
per **owned identity** (`Contents.own`, `Trace.lean`): the step that minted
it, every step whose trace ended it — a `drop` marker, a discarded
temporary's `dropTemp`, or a consumption, the three ends `freedIds` counts —
and every step whose destructor ran on it (`dtorIds`). "Exactly once"
(`drop_exactly_once`, `TraceExact.lean`) is then something a reader sees: one
entry in the *ended* column per identity. So is the order (`drop_order`,
`TraceOrder.lean`): the step numbers in that column say which value went
first.

Nothing here is trusted. The rows are `traceEval`'s, whose outcome
`traceEval_res` proves is `eval`'s, and the ledger only reads them.
-/

namespace RueCore
namespace Explain
namespace Ledger

/-- One owned identity's life: the step that minted it, if the run shows one,
each end with its place in the table and what ended it, and each
destructor's step (§7, §6.11). -/
structure Entry where
  id : Nat
  minted : Option Nat
  ends : List (String × String)
  dtors : List Nat

/-- (helper) What an event ends, and in words: a marker's name for the
ledger's *ended* column. -/
def endLabel : Event → String
  | .drop ℓ _ => "drop " ++ locName ℓ
  | .dropTemp _ => "discard"
  | .consume _ => "consume"
  | .dtor _ _ | .dbg _ => ""

/-- (helper) The identity a row minted: the value it produced is an owned
aggregate whose identity is the slot the row appended to the store — which is
what (D-Struct), (D-Enum-Intro), (D-Array) and the repeat form do
(`introVal`). A row that only passes such a value on comes later in the
table, so the first row that shows the identity is the one that minted it. -/
def mintedBy (D : Decls) (s : Step) : Option Nat :=
  match s.res with
  | .value v =>
      match (Contents.ofVal v).own D with
      | i :: _ =>
          if s.storeBefore.length ≤ i ∧ i + 1 = s.storeAfter.length then some i else none
      | [] => none
  | _ => none

/-- (helper) Add `i` to the list of identities seen, keeping first-seen
order. -/
def note (seen : List Nat) (i : Nat) : List Nat := if seen.contains i then seen else seen ++ [i]

/-- (helper) A row reference, as the step table numbers it. -/
def rowRef (n : Nat) : String := "[" ++ toString n ++ "]"

/-- (helper) The ends one row records, each with its place: `[n]`, or
`[n.k]` for the `k`-th end of a row that ends several values — a teardown
walking its record, or a destructure's residue — so the order inside one
step is visible too. -/
def rowEnds (D : Decls) (n : Nat) (s : Step) : List (Nat × String × String) :=
  let marks := s.events.filter (fun ev => endLabel ev != "")
  let ref := fun (k : Nat) =>
    if marks.length ≤ 1 then rowRef n else "[" ++ toString n ++ "." ++ toString k ++ "]"
  ((List.range marks.length).zip marks).flatMap (fun (k, ev) =>
    (Event.freed D ev).map (fun i => (i, ref (k + 1), endLabel ev)))

/-- The ledger of a run: one entry per owned identity the table mints or
ends, in the order the table first shows it (§7). -/
def entries (D : Decls) (rows : List (Nat × Step)) : List Entry :=
  let ids := rows.foldl (fun seen (_, s) =>
    let seen := match mintedBy D s with
      | some i => note seen i
      | none => seen
    (s.events.flatMap (Event.freed D)).foldl note seen) []
  ids.map fun i =>
    { id := i,
      minted := (rows.find? (fun (_, s) => mintedBy D s == some i)).map (·.1),
      ends := rows.flatMap (fun (n, s) =>
        (rowEnds D n s).filterMap (fun (j, r, l) => if j == i then some (r, l) else none)),
      dtors := rows.filterMap (fun (n, s) =>
        if (dtorIds s.events).contains i then some n else none) }

/-- (helper) The *ended* column: each end with its step. -/
def endsText (e : Entry) : String :=
  if e.ends.isEmpty then "—"
  else String.intercalate ", " (e.ends.map (fun (r, l) => r ++ " " ++ l))

/-- (helper) The verdict on one identity, read off the result: ended once;
still held, by the result or by a store a trap abandoned; or ended more than
once, which `no_double_free` says a checked run never does. -/
def verdict (D : Decls) (res : EvalRes) (e : Entry) : String :=
  match e.ends.length with
  | 1 => "once"
  | 0 =>
      match res with
      | .ok _ v _ | .returned _ v _ =>
          if (v.own D).contains e.id then "held by the result" else "never ended"
      | .panic _ _ => "abandoned by the trap (§6.12)"
      | .stuck _ => "the run was refused (§6)"
      | .broke _ _ _ | .outOfFuel => "the run did not finish"
  | n => toString n ++ " times"

/-- (helper) The ledger's closing line: every identity once, or which were
not and why. A refused run carries no trace (§6's stuck states), so its
ledger is only what the table recorded before the refusal. -/
def summary (D : Decls) (res : EvalRes) (es : List Entry) : String :=
  let odd := es.filter (fun e => verdict D res e != "once")
  match res with
  | .stuck _ =>
      "The run was refused (§6), which a checked program's never is (`soundness`); " ++
        "the ledger is what the table recorded before the refusal."
  | .outOfFuel | .broke _ _ _ => "The run did not finish, so the ledger is partial."
  | _ =>
      if es.isEmpty then "The run owns no identity: nothing to end."
      else if odd.isEmpty then
        "Every owned identity is ended exactly once (`drop_exactly_once`)."
      else
        "Every owned identity is ended exactly once, except " ++ String.intercalate ", "
          (odd.map (fun e => "#" ++ toString e.id ++ " (" ++ verdict D res e ++ ")")) ++ "."

end Ledger
end Explain
end RueCore
