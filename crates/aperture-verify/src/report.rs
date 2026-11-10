//! Turning test-runner reports into an [`Outcome`].
//!
//! The mapping is deliberately strict: a run with skipped tests, or with no
//! tests at all, is not a pass.

use aperture_core::Outcome;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
}

impl Counts {
    pub fn outcome(&self) -> Outcome {
        if self.failed > 0 {
            Outcome::Failed
        } else if self.skipped > 0 {
            Outcome::Skipped
        } else if self.passed > 0 {
            Outcome::Passed
        } else {
            // Nothing ran: the runner found no tests.
            Outcome::Error
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} passed, {} failed, {} skipped",
            self.passed, self.failed, self.skipped
        )
    }
}

/// Playwright's `--reporter=json` output.
pub fn playwright(json: &str) -> Option<Counts> {
    let value: Value = serde_json::from_str(json).ok()?;
    let stats = value.get("stats")?;
    let get = |key: &str| stats[key].as_u64().unwrap_or(0);
    Some(Counts {
        // A flaky test failed at least once; do not count it as a clean pass.
        passed: get("expected"),
        failed: get("unexpected") + get("flaky"),
        skipped: get("skipped"),
    })
}

/// JUnit XML, as written by Maestro and most other runners. Counts
/// `<testcase>` elements rather than trusting suite attributes.
pub fn junit(xml: &str) -> Option<Counts> {
    let mut reader = Reader::from_str(xml);
    let mut counts = Counts::default();
    let mut seen_case = false;
    // State of the test case currently open: None, or Some(failed, skipped).
    let mut open: Option<(bool, bool)> = None;
    let close = |open: &mut Option<(bool, bool)>, counts: &mut Counts| {
        if let Some((failed, skipped)) = open.take() {
            if failed {
                counts.failed += 1;
            } else if skipped {
                counts.skipped += 1;
            } else {
                counts.passed += 1;
            }
        }
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => match tag.name().as_ref() {
                b"testcase" => {
                    seen_case = true;
                    open = Some((false, false));
                }
                b"failure" | b"error" => {
                    if let Some(case) = open.as_mut() {
                        case.0 = true;
                    }
                }
                b"skipped" => {
                    if let Some(case) = open.as_mut() {
                        case.1 = true;
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(tag)) => match tag.name().as_ref() {
                b"testcase" => {
                    seen_case = true;
                    counts.passed += 1;
                }
                b"failure" | b"error" => {
                    if let Some(case) = open.as_mut() {
                        case.0 = true;
                    }
                }
                b"skipped" => {
                    if let Some(case) = open.as_mut() {
                        case.1 = true;
                    }
                }
                _ => {}
            },
            Ok(Event::End(tag)) if tag.name().as_ref() == b"testcase" => {
                close(&mut open, &mut counts)
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }
    seen_case.then_some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playwright_stats() {
        let pass = r#"{"stats":{"expected":3,"unexpected":0,"skipped":0,"flaky":0}}"#;
        assert_eq!(playwright(pass).unwrap().outcome(), Outcome::Passed);
        let fail = r#"{"stats":{"expected":2,"unexpected":1,"skipped":0,"flaky":0}}"#;
        assert_eq!(playwright(fail).unwrap().outcome(), Outcome::Failed);
        let skipped = r#"{"stats":{"expected":2,"unexpected":0,"skipped":1,"flaky":0}}"#;
        assert_eq!(playwright(skipped).unwrap().outcome(), Outcome::Skipped);
        let flaky = r#"{"stats":{"expected":2,"unexpected":0,"skipped":0,"flaky":1}}"#;
        assert_eq!(playwright(flaky).unwrap().outcome(), Outcome::Failed);
        let none = r#"{"stats":{"expected":0,"unexpected":0,"skipped":0,"flaky":0}}"#;
        assert_eq!(playwright(none).unwrap().outcome(), Outcome::Error);
        assert!(playwright("not json").is_none());
    }

    #[test]
    fn junit_cases() {
        let xml = r#"<testsuites><testsuite tests="3">
            <testcase name="a"/>
            <testcase name="b"><failure message="x">boom</failure></testcase>
            <testcase name="c"><skipped/></testcase>
        </testsuite></testsuites>"#;
        let counts = junit(xml).unwrap();
        assert_eq!(
            counts,
            Counts {
                passed: 1,
                failed: 1,
                skipped: 1
            }
        );
        assert_eq!(counts.outcome(), Outcome::Failed);

        let pass = r#"<testsuite><testcase name="a"></testcase><testcase name="b"/></testsuite>"#;
        assert_eq!(junit(pass).unwrap().outcome(), Outcome::Passed);
        // A suite that claims success but lists no cases is not a pass.
        assert!(junit(r#"<testsuite tests="2" failures="0"/>"#).is_none());
    }
}
