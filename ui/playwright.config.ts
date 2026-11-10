import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  timeout: 180_000,
  expect: { timeout: 60_000 },
  reporter: "list",
  use: { viewport: { width: 1500, height: 900 } },
});
