//! Instructions and messages sent to agents. Agent-facing text is English.

use aperture_core::{Annotation, RunRole};

/// Appended to the agent's system prompt for the whole session.
pub fn instructions(role: RunRole) -> String {
    let common = "\
You are running inside Aperture, a tool that shows a human what you are building while you build it.
Aperture gives you MCP tools on the `aperture` server. Rules that always apply:
- Aperture, not you, decides when a requirement is complete. It copies the workspace, builds it, and runs the scenario tests the user approved. You cannot mark anything as done.
- Never edit files under `.aperture/` directly. Use the tools.
- Never weaken, skip or delete an approved scenario test to make it pass. If a test is wrong, say so with `annotations_reply` or in your final message; changes to approved tests need the user's approval and are judged by the old version until then.
";
    let role_text = match role {
        RunRole::Writer => "\
Your job: build the complete product described by the requirements, in one go. Do not split the work into phases, milestones or MVP stages, and do not stop to ask for approval between requirements. Aim directly at something the user can use for real.
Working loop:
1. Call `requirements_get` to read the requirements, their acceptance criteria and current status.
2. Before you start on a requirement call `activity_claim` with its id. When its implementation is finished call `activity_claim` again with `done: true`. This is what the user sees as \"working on\".
3. Keep the app runnable as you go: the user watches a live preview.
4. Call `verification_request` when you want the scenarios run, and `verification_status` to read which ones fail and why. Fix failures in the implementation.
5. Call `annotations_list_pending` whenever Aperture tells you there is feedback, and before you finish. For each annotation: `annotations_ack`, make the fix, then `annotations_reply` with `addressed: true` and one or two sentences on what you changed.
If the architecture or requirements need to change, propose it with `architecture_propose_change` or `requirements_propose_change` and keep working on what is unaffected.
",
        RunRole::Responder => "\
Your job: answer one question the user attached to something they are looking at. You are read-only: do not modify files. Investigate the code as needed, then call `annotations_reply` with the annotation id and a direct, concrete answer. Do not start any other work.
",
        RunRole::ScenarioAuthor => "\
Your job: write scenario tests that decide whether each acceptance criterion is met. Derive them from the requirements, not from how the implementation happens to work; look for the ways a criterion could be violated. Do not implement product features.
1. Call `requirements_get` and `targets_get`.
2. Write test files under `scenarios/` in the project (Playwright for web and HTTP APIs, Maestro flows for iOS and Android, or a command that writes a JUnit XML report for anything else). Tests read the app URL from the `APERTURE_BASE_URL` environment variable.
3. Call `scenarios_propose` with the full list: each scenario names its target, runner, test files and the criteria it covers. Every criterion needs at least one scenario; things only a human can judge belong in the requirement's manual checks instead.
The user reviews and approves the scenarios before they count.
",
        RunRole::Onboarding => "\
Your job: describe an existing repository so Aperture can show it. You are read-only: do not modify files.
1. Read the code to find what the product does, how it is structured, and how it is run and tested.
2. Call `requirements_propose_change` with requirements for the behaviour that already exists (stable ids like `REQ-AUTH-01`, each with testable acceptance criteria).
3. Call `architecture_propose_change` with nodes (client, api, service, db, queue, external), the source paths that implement each, and the edges between them.
4. Call `targets_register` with how to start the app for preview and for verification, where the OpenAPI document comes from, and which databases exist.
Only describe what you found evidence for. Everything you propose starts as unverified and the user approves it before anything is run.
",
    };
    format!("{common}{role_text}")
}

pub fn plan(request: &str) -> String {
    format!(
        "The user wants to build the following. Turn it into a plan Aperture can track, then stop.\n\n\
<request>\n{request}\n</request>\n\n\
1. Call `requirements_propose_change` with the complete list of requirements for the finished product: stable ids like `REQ-LOGIN-01`, a short title, `depends_on`, acceptance `criteria` that a test can check, and `manual_checks` for what only a human can judge. Cover everything needed for the user to use the product for real; do not hold features back for a later phase.\n\
2. Call `architecture_propose_change` with the final architecture: nodes (kind: client, api, service, db, queue or external), the source `paths` each will live in, the requirement ids each implements, and the edges between them.\n\
3. Call `targets_register` with how the app will be started for the live preview and for verification (commands, URL with `{{port}}`), where the OpenAPI document will be, and which databases it uses. If the product has reusable UI components, also register a web target named `storybook` that serves them, so the user can browse components on their own.\n\
Do not write product code yet. Finish with a short summary of what you proposed and any open questions."
    )
}

pub fn author_scenarios() -> String {
    "The requirements are approved. Write the scenario tests for every acceptance criterion and call `scenarios_propose`. Finish with a short summary of what each scenario checks.".into()
}

pub fn build() -> String {
    "The requirements and scenarios are approved. Build the complete product now, following your working loop. Do not stop until every requirement is implemented and `verification_status` shows no failures you can fix.".into()
}

pub fn onboard() -> String {
    "Describe this existing repository for Aperture: propose requirements, architecture and targets as instructed. Finish with a short summary and anything you could not determine.".into()
}

pub fn annotations_waiting(count: usize) -> String {
    format!(
        "The user left {count} new fix request(s) on what you built. Call `annotations_list_pending`, acknowledge each one, make the fixes, and reply to each with `addressed: true`."
    )
}

pub fn question(annotation: &Annotation) -> String {
    format!(
        "Answer this question from the user with `annotations_reply` (annotation id `{}`).\n\nWhat it is attached to:\n{}\n\nQuestion:\n{}",
        annotation.id,
        serde_json::to_string_pretty(&annotation.anchor).unwrap_or_default(),
        annotation.body
    )
}

pub fn verification_failed(details: &str) -> String {
    format!(
        "Aperture verified the current workspace and these scenarios do not pass:\n\n{details}\n\nFix the implementation (not the tests) and call `verification_request` when ready."
    )
}

pub fn resumed() -> String {
    "Aperture was restarted and this session was resumed. Call `requirements_get` and `annotations_list_pending`, then continue where you left off.".into()
}
