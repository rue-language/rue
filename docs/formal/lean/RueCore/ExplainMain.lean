import RueCore.Explain.Text
import RueCore.Explain.Html

/-!
# `lake exe ruecore-explain` — the derivation and trace visualizer (RUE-2246)

```
ruecore-explain <case>        the plain-text explanation of one corpus case
ruecore-explain --all         every case, in corpus order
ruecore-explain --list        the case names, one per line
ruecore-explain --text <dir>  one <case>.txt per case into <dir>
ruecore-explain --html <dir>  one <case>.html per case, plus index.html
```

The checked-in renderings under `docs/formal/lean/explain/` are
`--text explain`'s output; the HTML is generated on demand.
-/

open RueCore

/-- (helper) How to call the executable. -/
def usage : String :=
  "usage: ruecore-explain <case> | --all | --list | --text <dir> | --html <dir>"

/-- (helper) The available case names, for an unknown argument. -/
def nameList : String :=
  String.intercalate "\n" (Corpus.cases.map (fun c => "  " ++ c.name))

/-- (helper) Write one file per corpus case into `dir`, naming each file
with `ext` and rendering it with `render`. -/
def writeAll (dir : String) (ext : String) (render : Corpus.Case → String) : IO Unit := do
  IO.FS.createDirAll dir
  for c in Corpus.cases do
    IO.FS.writeFile (System.FilePath.mk dir / (c.name ++ ext)) (render c)

/-- (helper) Render the bridge corpus as readable §5 derivations and §6
step tables. -/
def main (args : List String) : IO UInt32 := do
  match args with
  | ["--list"] =>
      for c in Corpus.cases do IO.println c.name
      pure 0
  | ["--all"] =>
      for c in Corpus.cases do IO.print (Explain.Text.renderCase c)
      pure 0
  | ["--text", dir] =>
      writeAll dir ".txt" Explain.Text.renderCase
      IO.eprintln s!"wrote {Corpus.cases.length} text renderings to {dir}"
      pure 0
  | ["--html", dir] =>
      writeAll dir ".html" Explain.Html.renderCase
      IO.FS.writeFile (System.FilePath.mk dir / "index.html") (Explain.Html.index Corpus.cases)
      IO.eprintln s!"wrote {Corpus.cases.length} pages and an index to {dir}"
      pure 0
  | [name] =>
      match Corpus.cases.find? (fun c => c.name == name) with
      | some c => IO.print (Explain.Text.renderCase c); pure 0
      | none =>
          IO.eprintln s!"unknown case: {name}"
          IO.eprintln "available cases:"
          IO.eprintln nameList
          pure 1
  | _ => IO.eprintln usage; pure 1
