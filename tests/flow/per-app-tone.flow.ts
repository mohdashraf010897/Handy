// Run with: bunx playwright test -c playwright.flow.config.ts
import { expect, test, type Locator, type Page } from "@playwright/test";
import type { FlowSettings } from "@/bindings";
import { DEFAULT_FLOW_SETTINGS } from "@/components/flow/toneRules";
import type { FlowHarness } from "./harness";

const DEFAULTS: FlowSettings = {
  ...DEFAULT_FLOW_SETTINGS,
};

const harness = (page: Page) =>
  page.evaluate(() => structuredClone(window.__flowHarness) as FlowHarness);
const stored = async (page: Page) => (await harness(page)).stored;

const open = async (page: Page, query = "") => {
  await page.goto(`/index.html${query}`);
  await expect(
    page.getByRole("heading", { name: "Per-app tone", exact: true }),
  ).toBeVisible();
};

// Switches and tone pickers are visually-hidden native inputs under a styled
// label, so toggle them the way a keyboard user does: focus, then Space.
const toggle = (control: Locator) => control.press("Space");

const toneSwitch = (page: Page) =>
  page.getByRole("checkbox", { name: "Match tone to the app" });
const fastPathSwitch = (page: Page) =>
  page.getByRole("checkbox", { name: "Instant fast-path" });
const resetButton = (page: Page) =>
  page.getByRole("button", { name: "Reset to defaults" });
const pickTone = (page: Page, group: string, tone: string) =>
  toggle(
    page
      .getByRole("radiogroup", { name: group, exact: true })
      .getByRole("radio", { name: tone, exact: true }),
  );

test.afterEach(async ({ page }) => {
  expect((await harness(page)).unhandled).toEqual([]);
});

test("tone is off by default and enabling it is saved", async ({ page }) => {
  await open(page);
  await expect(toneSwitch(page)).not.toBeChecked();
  await expect(resetButton(page)).toBeDisabled();

  await toggle(toneSwitch(page));

  await expect(toneSwitch(page)).toBeChecked();
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      tone_enabled: true,
    });
  const { saves } = await harness(page);
  expect(saves).toHaveLength(1);
  expect(saves[0].tone_enabled).toBe(true);
});

test("overriding a built-in rule saves the override", async ({ page }) => {
  await open(page);
  await page.getByText("Show 4 built-in rules").click();
  await page
    .getByRole("button", {
      name: "Override built-in rule for com.apple.Terminal",
    })
    .click();
  await expect(
    page.getByRole("textbox", { name: "App ID for rule 1" }),
  ).toHaveValue("com.apple.Terminal");

  await pickTone(page, "Tone for com.apple.Terminal", "Formal");

  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      app_tones: [{ app_id: "com.apple.Terminal", tone: "formal" }],
    });
  await expect(page.getByText("Overridden: Formal")).toBeVisible();
});

test("custom rules can be added, edited and removed", async ({ page }) => {
  await open(page);
  const appId = page.getByRole("textbox", { name: "Add an app" });

  await appId.fill("com.example.chat");
  await pickTone(page, "Tone for the new rule", "Formal");
  await page.getByRole("button", { name: "Add rule" }).click();
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      app_tones: [{ app_id: "com.example.chat", tone: "formal" }],
    });
  await expect(appId).toHaveValue("");

  // Duplicate ids (compared case-insensitively) are rejected, not saved.
  await appId.fill("COM.EXAMPLE.CHAT");
  await page.getByRole("button", { name: "Add rule" }).click();
  await expect(page.getByRole("alert")).toHaveText(
    "There's already a rule for this app.",
  );
  await expect(appId).toHaveAttribute("aria-invalid", "true");
  expect((await harness(page)).saves).toHaveLength(1);

  const ruleId = page.getByRole("textbox", { name: "App ID for rule 1" });
  await ruleId.fill("com.example.mail");
  await ruleId.press("Enter");
  await pickTone(page, "Tone for com.example.mail", "Code");
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      app_tones: [{ app_id: "com.example.mail", tone: "code" }],
    });

  await page
    .getByRole("button", { name: "Remove rule for com.example.mail" })
    .click();
  await expect.poll(() => stored(page)).toEqual(DEFAULTS);
  await expect(
    page.getByText("No custom rules yet. Built-in presets still apply."),
  ).toBeVisible();
});

test("the last dictated app card adds a rule for that app", async ({
  page,
}) => {
  await open(page, "?lastApp=textedit");
  await expect(
    page.getByText("No rule matches, so it uses Default."),
  ).toBeVisible();

  await pickTone(page, "Tone for TextEdit", "Casual");

  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      app_tones: [{ app_id: "com.apple.TextEdit", tone: "casual" }],
    });
  await expect(
    page.getByText("Uses Casual from your rule “com.apple.TextEdit”."),
  ).toBeVisible();
  await expect(
    page.getByRole("radiogroup", { name: "Tone for TextEdit", exact: true }),
  ).toHaveCount(1);
});

test("reset restores the defaults and disables itself", async ({ page }) => {
  await open(page);
  await expect(resetButton(page)).toBeDisabled();

  await toggle(toneSwitch(page));
  await toggle(fastPathSwitch(page));
  await page.getByRole("textbox", { name: "Add an app" }).fill("slack.exe");
  await page.getByRole("button", { name: "Add rule" }).click();
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      tone_enabled: true,
      app_tones: [{ app_id: "slack.exe", tone: "casual" }],
      fast_path_enabled: false,
      snippets: [],
    });

  await resetButton(page).click();
  const dialog = page.getByRole("dialog", { name: "Reset per-app tone?" });
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Reset", exact: true }).click();

  await expect(dialog).toBeHidden();
  await expect.poll(() => stored(page)).toEqual(DEFAULTS);
  const { saves } = await harness(page);
  expect(saves[saves.length - 1]).toEqual(DEFAULTS);
  await expect(toneSwitch(page)).not.toBeChecked();
  await expect(fastPathSwitch(page)).toBeChecked();
  await expect(resetButton(page)).toBeDisabled();
});

test("instant fast-path is enabled by default and toggling saves", async ({
  page,
}) => {
  await open(page);
  await expect(fastPathSwitch(page)).toBeChecked();
  await expect(resetButton(page)).toBeDisabled();

  await toggle(fastPathSwitch(page));

  await expect(fastPathSwitch(page)).not.toBeChecked();
  await expect(resetButton(page)).toBeEnabled();
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      fast_path_enabled: false,
    });
});

test("voice snippets can be added, validated against duplicates, and removed even with post-processing off", async ({
  page,
}) => {
  await open(page, "?postProcess=off");
  await expect(page.getByText("No voice snippets yet.")).toBeVisible();

  const triggerInput = page.getByPlaceholder(
    "Spoken phrase (e.g. my calendar link)",
  );
  const expansionInput = page.getByPlaceholder(
    "Text to insert (e.g. https://cal.com/you)",
  );
  const addButton = page.getByRole("button", { name: "Add snippet" });

  await addButton.click();
  await expect(page.getByRole("alert")).toHaveText(
    "Enter the spoken trigger phrase.",
  );

  await triggerInput.fill("  my   calendar link ");
  await addButton.click();
  await expect(page.getByRole("alert")).toHaveText(
    "Enter the text this phrase should insert.",
  );

  await expansionInput.fill("https://cal.com/lowyap");
  await addButton.click();

  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      snippets: [
        { trigger: "my calendar link", expansion: "https://cal.com/lowyap" },
      ],
    });
  await expect(triggerInput).toHaveValue("");
  await expect(expansionInput).toHaveValue("");
  await expect(page.getByText("my calendar link")).toBeVisible();
  await expect(page.getByText("https://cal.com/lowyap")).toBeVisible();
  await expect(resetButton(page)).toBeEnabled();

  // Case-insensitive + whitespace-normalized duplicates are rejected.
  await triggerInput.fill("MY CALENDAR LINK");
  await expansionInput.fill("https://cal.com/other");
  await addButton.click();
  await expect(page.getByRole("alert")).toHaveText(
    "A snippet for this phrase already exists.",
  );
  expect((await harness(page)).saves).toHaveLength(1);

  // Clear add-form draft so its alert dismisses, then inline-edit the existing snippet.
  await triggerInput.fill("");
  await expansionInput.fill("");
  await page.getByText("my calendar link").click();
  const editExpansion = page.getByRole("textbox").nth(1);
  await expect(editExpansion).toHaveValue("https://cal.com/lowyap");
  await editExpansion.fill("https://cal.com/lowyap-team");
  await editExpansion.press("Enter");
  await expect
    .poll(() => stored(page))
    .toEqual({
      ...DEFAULTS,
      snippets: [
        {
          trigger: "my calendar link",
          expansion: "https://cal.com/lowyap-team",
        },
      ],
    });

  await page
    .getByRole("button", { name: "Remove snippet my calendar link" })
    .click();
  await expect.poll(() => stored(page)).toEqual(DEFAULTS);
  await expect(page.getByText("No voice snippets yet.")).toBeVisible();
});

test("warns and offers the toggle while post-processing is off", async ({
  page,
}) => {
  const warning =
    /Per-app tone only takes effect when AI post-processing is on/;
  await open(page, "?postProcess=off");
  await expect(page.getByText(warning)).toBeVisible();
  const postProcess = page.getByRole("checkbox", { name: "Post Processing" });
  await expect(postProcess).not.toBeChecked();

  await toggle(postProcess);

  await expect
    .poll(async () => (await harness(page)).postProcessEnabled)
    .toBe(true);
  await expect(page.getByText(warning)).toBeHidden();
  await expect(postProcess).toBeHidden();
});

test("no warning or toggle while post-processing is on", async ({ page }) => {
  await open(page);
  await expect(
    page.getByText("Applied during AI post-processing", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByText(/only takes effect when AI post-processing is on/),
  ).toHaveCount(0);
  await expect(
    page.getByRole("checkbox", { name: "Post Processing" }),
  ).toHaveCount(0);
});
