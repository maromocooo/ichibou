// The whole loop through the real UI, against the real daemon, with the
// scripted agent building the demo project. Needs `cargo build` and
// `npm run build` first; run with `npm run e2e`.
import { type ChildProcess, spawn } from "node:child_process";
import { cpSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
let daemon: ChildProcess;
let scratch: string;
let url: string;

test.beforeAll(async () => {
  scratch = mkdtempSync(path.join(tmpdir(), "aperture-e2e-"));
  const project = path.join(scratch, "web-demo");
  cpSync(path.join(repo, "fixtures/web-demo"), project, { recursive: true });
  daemon = spawn(
    // APERTURE_BIN points the test at another build (a release binary).
    process.env.APERTURE_BIN ?? path.join(repo, "target/debug/aperture"),
    ["--project", project, "serve", "--port", "0", "--no-open", "--json", "--agent", "fake",
     "--fake-script", path.join(repo, "fixtures/web-demo-script.json")],
    // State goes to the scratch folder, not the real per-user directory.
    { env: { ...process.env, APERTURE_STATE_DIR: path.join(scratch, "state") } },
  );
  url = await new Promise<string>((resolve, reject) => {
    let out = "";
    daemon.stdout!.on("data", (chunk: Buffer) => {
      out += chunk.toString();
      const line = out.split("\n")[0];
      if (out.includes("\n") && line) resolve((JSON.parse(line) as { url: string }).url);
    });
    daemon.on("exit", (code) => reject(new Error(`daemon exited with ${code}`)));
  });
});

test.afterAll(async () => {
  daemon.kill("SIGINT");
  await new Promise((resolve) => daemon.on("exit", resolve));
  rmSync(scratch, { recursive: true, force: true });
});

test("request to verified, with feedback on the way", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("aperture-lang", "en"));
  await page.goto(url);
  const sidebar = page.locator(".sidebar");
  const next = page.locator(".next");

  // Nothing counts until the user has approved requirements, scenarios and commands.
  await expect(next).toContainText("requirements were changed outside Aperture");
  await next.getByRole("button", { name: "Accept" }).click();
  await next.getByRole("button", { name: /Review and approve the scenarios/ }).click();
  await page.getByRole("button", { name: /Approve all/ }).click();
  await expect(next).toContainText("Approve the commands Aperture will run");
  await expect(next).toContainText("node server.js");
  await next.getByRole("button", { name: "Accept" }).click();

  // The order of work is visible before anything is built.
  await expect(sidebar).toContainText("After REQ-API-01");

  await next.getByRole("button", { name: "Build everything" }).click();
  await expect(page.locator(".agentbar")).toContainText("Builder");

  // Check marks come from verification, and the diagram fills in.
  await expect(sidebar.locator(".req.done")).toHaveCount(2);
  await expect(sidebar.locator(".req.awaiting_manual")).toHaveCount(1);
  await page.getByRole("tab", { name: "Backend" }).click();
  await expect(page.locator(".arch.verified")).toHaveCount(2);
  await page.locator(".arch", { hasText: "Notes API" }).click();
  await expect(page.locator(".ops")).toContainText("/api/notes");

  // Live preview of the app that was just built.
  await page.getByRole("tab", { name: "Frontend" }).click();
  await page.getByRole("button", { name: "Start preview" }).click();
  const app = page.frameLocator("iframe.preview");
  await expect(app.getByRole("heading", { name: "Notes" })).toBeVisible();
  await expect(app.getByRole("button", { name: "Add" })).toBeVisible();

  // Pick an element in the preview and ask for a change.
  await page.getByRole("button", { name: "Annotate" }).click();
  await app.getByRole("button", { name: "Add" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Add");
  await dialog.getByRole("textbox").fill('Rename this button to "Save note".');
  await dialog.getByRole("button", { name: "Add" }).click();

  // The agent fixes it and replies; the user decides it is resolved.
  const thread = page.locator(".thread").first();
  await expect(thread).toContainText("Changed the button label");
  await page.getByRole("tab", { name: "Frontend" }).click();
  await expect(app.getByRole("button", { name: "Save note" })).toBeVisible();
  await page.getByRole("tab", { name: /Feedback/ }).click();
  await thread.getByRole("button", { name: "Resolved" }).click();

  // The fix changed the code, so everything is verified again; then the
  // one check only a person can make completes the request.
  await expect(sidebar.locator(".req.done")).toHaveCount(2);
  await expect(sidebar.locator(".req.awaiting_manual")).toHaveCount(1);
  await expect(page.locator(".topbar .pill")).not.toContainText("Verifying");
  await sidebar.getByRole("button", { name: /Notes page/ }).click();
  // The box reflects the daemon's state, so it ticks once the daemon answers.
  await sidebar.getByRole("checkbox").click();
  await expect(sidebar.getByRole("checkbox")).toBeChecked();
  await expect(page.locator(".topbar .pill")).toHaveText("Request complete");
  await expect(sidebar.locator(".req.done")).toHaveCount(3);
});
