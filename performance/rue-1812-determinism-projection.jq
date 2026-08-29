def computed_reused:
  if type == "object" then
    reduce to_entries[] as $entry ({};
      if ($entry.key == "computed" or $entry.key == "reused") then
        .[$entry.key] = $entry.value
      else
        ($entry.value | computed_reused) as $value
        | if ($value == {} or $value == [] or $value == null) then
            .
          else
            .[$entry.key] = $value
          end
      end)
  elif type == "array" then
    [.[] | computed_reused | select(. != {} and . != [] and . != null)]
  else
    null
  end;

if $projection == "cold" then
  [.workloads[]
    | {
        workload,
        worker_setting,
        resolved_workers,
        native_output_sha256: [.samples[].boundary_evidence[].runner.output_sha256]
      }
      + if .resolved_workers == 1 then {work} else {} end]
elif $projection == "warm" then
  [.rows[]
    | {
        workload,
        scenario,
        worker_mode,
        samples: [.samples[]
          | {
              sample_index,
              outcome: {
                kind: .outcome.kind,
                diagnostics: .outcome.diagnostics,
                warnings: .outcome.warnings,
                executable: .outcome.executable
              },
              work: (.work | computed_reused)
            }]
      }]
else
  error("projection must be cold or warm")
end
