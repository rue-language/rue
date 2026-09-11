//! Canonical projection of the `rue test --format json` event stream.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestExecutionProjection {
    pub output_sha256: String,
    pub tests_executed_count: u64,
    pub projection: Vec<Value>,
}

/// Parse a complete test event stream and derive its deterministic behavior
/// projection. Timing fields are excluded; the raw stream remains execution
/// proof and is preserved by the caller.
pub fn project_test_execution_output(bytes: &[u8]) -> Result<TestExecutionProjection, String> {
    let mut projection = Vec::new();
    let mut run_started = 0_u32;
    let mut run_finished = 0_u32;
    let mut started_ids = BTreeSet::new();
    let mut finished_ids = BTreeSet::new();
    let mut verdicts = std::collections::BTreeMap::<String, u64>::new();
    let mut selected = None;
    let mut terminal_seen = false;
    let mut tests_executed_count = 0;
    for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_slice(line)
            .map_err(|error| format!("event line {} is not JSON: {error}", line_number + 1))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| format!("event line {} is not an object", line_number + 1))?;
        let event = object
            .get("event")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("event line {} has no event name", line_number + 1))?;
        match event {
            "run_started" => {
                if terminal_seen || run_started != 0 {
                    return Err(format!(
                        "event line {} repeats or follows the run header",
                        line_number + 1
                    ));
                }
                run_started = 1;
                selected = Some(
                    object
                        .get("plan")
                        .and_then(Value::as_object)
                        .and_then(|plan| plan.get("selected"))
                        .and_then(Value::as_u64)
                        .ok_or_else(|| {
                            format!(
                                "event line {} has no numeric plan.selected",
                                line_number + 1
                            )
                        })?,
                );
            }
            "test_started" => {
                if terminal_seen || run_started != 1 {
                    return Err(format!(
                        "event line {} starts a test outside the run",
                        line_number + 1
                    ));
                }
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| format!("event line {} has no test id", line_number + 1))?;
                if !started_ids.insert(id.to_owned()) {
                    return Err(format!(
                        "event line {} repeats test id {id:?}",
                        line_number + 1
                    ));
                }
            }
            "test_finished" => {
                if terminal_seen || run_started != 1 {
                    return Err(format!(
                        "event line {} finishes a test outside the run",
                        line_number + 1
                    ));
                }
                let id = object
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| format!("event line {} has no test id", line_number + 1))?;
                if !started_ids.contains(id) {
                    return Err(format!(
                        "event line {} finishes unstarted test {id:?}",
                        line_number + 1
                    ));
                }
                if !finished_ids.insert(id.to_owned()) {
                    return Err(format!(
                        "event line {} repeats test result {id:?}",
                        line_number + 1
                    ));
                }
                let verdict = object
                    .get("verdict")
                    .and_then(Value::as_str)
                    .filter(|verdict| {
                        matches!(
                            *verdict,
                            "pass"
                                | "fail"
                                | "timeout"
                                | "crash"
                                | "compile_error"
                                | "xfail"
                                | "xpass"
                        )
                    })
                    .ok_or_else(|| {
                        format!("event line {} has no recognized verdict", line_number + 1)
                    })?;
                let count_name = match verdict {
                    "pass" => "passed",
                    "fail" => "failed",
                    other => other,
                };
                *verdicts.entry(count_name.to_owned()).or_default() += 1;
                // Expected compile failures render as `xfail`, but still
                // have a compile_error failure record and spawn no child.
                let compile_error = verdict == "compile_error"
                    || object
                        .get("failure")
                        .and_then(|failure| failure.get("kind"))
                        .and_then(Value::as_str)
                        == Some("compile_error");
                if !compile_error {
                    tests_executed_count += 1;
                }
            }
            "run_finished" => {
                if terminal_seen || run_started != 1 || run_finished != 0 {
                    return Err(format!(
                        "event line {} repeats or misplaces the run footer",
                        line_number + 1
                    ));
                }
                if started_ids != finished_ids {
                    return Err(format!(
                        "event line {} closes before every test finished",
                        line_number + 1
                    ));
                }
                let count = |name: &str| {
                    object.get(name).and_then(Value::as_u64).ok_or_else(|| {
                        format!("event line {} has no numeric {name}", line_number + 1)
                    })
                };
                for name in [
                    "passed",
                    "failed",
                    "timeout",
                    "crash",
                    "compile_error",
                    "xfail",
                    "xpass",
                ] {
                    let expected = verdicts.get(name).copied().unwrap_or(0);
                    if count(name)? != expected {
                        return Err(format!(
                            "event line {} has a {name} count inconsistent with test results",
                            line_number + 1
                        ));
                    }
                }
                run_finished = 1;
                terminal_seen = true;
            }
            "run_canceled" => return Err("test event stream was canceled".into()),
            other => {
                return Err(format!(
                    "event line {} has unknown event {other:?}",
                    line_number + 1
                ));
            }
        }
        object.remove("wall_ms");
        object.remove("duration_ms");
        projection.push(value);
    }
    if run_started != 1 || run_finished != 1 || started_ids.is_empty() {
        return Err(format!(
            "incomplete test event stream (run_started={run_started}, run_finished={run_finished}, test_finished={})",
            finished_ids.len()
        ));
    }
    if selected != Some(started_ids.len() as u64) {
        return Err("run plan.selected does not match dispatched tests".into());
    }
    let canonical = crate::canonical_json(&projection)
        .map_err(|error| format!("could not canonicalize test events: {error}"))?;
    let output_sha256 = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    Ok(TestExecutionProjection {
        output_sha256,
        tests_executed_count,
        projection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_nondeterministic_event_clocks() {
        let one = br#"{"event":"run_started","plan":{"selected":1},"wall_ms":1}
{"event":"test_started","id":"0000000000000000"}
{"event":"test_finished","id":"0000000000000000","verdict":"pass","duration_ms":3}
{"event":"run_finished","passed":1,"failed":0,"timeout":0,"crash":0,"compile_error":0,"xfail":0,"xpass":0,"wall_ms":4}"#;
        let two = br#"{"event":"run_started","plan":{"selected":1},"wall_ms":99}
{"event":"test_started","id":"0000000000000000"}
{"event":"test_finished","id":"0000000000000000","verdict":"pass","duration_ms":300}
{"event":"run_finished","passed":1,"failed":0,"timeout":0,"crash":0,"compile_error":0,"xfail":0,"xpass":0,"wall_ms":400}"#;
        assert_eq!(
            project_test_execution_output(one).unwrap().output_sha256,
            project_test_execution_output(two).unwrap().output_sha256
        );
    }

    #[test]
    fn rejects_listing_and_partial_streams() {
        assert!(project_test_execution_output(br#"{"event":"test","id":"x"}"#).is_err());
        assert!(project_test_execution_output(br#"{"event":"run_canceled"}"#).is_err());
    }

    #[test]
    fn failed_and_uncompiled_results_keep_footer_and_process_counts_distinct() {
        let stream = br#"{"event":"run_started","plan":{"selected":3}}
{"event":"test_started","id":"fails"}
{"event":"test_finished","id":"fails","verdict":"fail"}
{"event":"test_started","id":"uncompiled"}
{"event":"test_finished","id":"uncompiled","verdict":"compile_error"}
{"event":"test_started","id":"expected"}
{"event":"test_finished","id":"expected","verdict":"xfail","failure":{"kind":"compile_error"}}
{"event":"run_finished","passed":0,"failed":1,"timeout":0,"crash":0,"compile_error":1,"xfail":1,"xpass":0}"#;
        assert_eq!(
            project_test_execution_output(stream)
                .unwrap()
                .tests_executed_count,
            1
        );
        let wrong_footer = std::str::from_utf8(stream)
            .unwrap()
            .replace("\"failed\":1", "\"failed\":0");
        assert!(project_test_execution_output(wrong_footer.as_bytes()).is_err());
    }

    #[test]
    fn rejects_unknown_events_missing_verdicts_and_unfinished_tests() {
        let unknown = br#"{"event":"run_started","plan":{"selected":1}}
{"event":"unexpected"}
{"event":"test_started","id":"x"}
{"event":"test_finished","id":"x","verdict":"pass"}
{"event":"run_finished","passed":1,"failed":0,"timeout":0,"crash":0,"compile_error":0,"xfail":0,"xpass":0}"#;
        assert!(project_test_execution_output(unknown).is_err());

        let reversed = br#"{"event":"run_started","plan":{"selected":1}}
{"event":"test_finished","id":"x","verdict":"pass"}
{"event":"test_started","id":"x"}
{"event":"run_finished","passed":1,"failed":0,"timeout":0,"crash":0,"compile_error":0,"xfail":0,"xpass":0}"#;
        assert!(project_test_execution_output(reversed).is_err());

        let missing_verdict = br#"{"event":"run_started","plan":{"selected":1}}
{"event":"test_started","id":"x"}
{"event":"test_finished","id":"x"}
{"event":"run_finished","passed":1,"failed":0,"timeout":0,"crash":0,"compile_error":0,"xfail":0,"xpass":0}"#;
        assert!(project_test_execution_output(missing_verdict).is_err());
    }
}
