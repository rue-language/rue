import RueCore.Corpus

/-- `lake exe ruecore-corpus` prints the bridge corpus as JSON on stdout
(helper). -/
def main : IO Unit :=
  IO.print RueCore.Corpus.json
