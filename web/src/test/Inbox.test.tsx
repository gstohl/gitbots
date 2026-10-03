import { render, screen, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { setTransport } from "../api/client";
import { IDS } from "../mocks/fixtures";
import InboxPage from "../pages/Inbox";
import { routed, useMockServer } from "./render";

afterEach(() => setTransport(null));

function renderInbox() {
  return render(routed(InboxPage));
}

describe("Inbox", () => {
  it("renders a card per attempt awaiting review, linking to the review", async () => {
    useMockServer();
    renderInbox();
    const cards = await screen.findAllByTestId("awaiting-card");
    expect(cards).toHaveLength(3);
    const rate = cards.find((c) => c.textContent?.includes("token-bucket rate limiting"));
    expect(rate).toBeDefined();
    expect(rate!.getAttribute("href")).toBe(`/attempts/${IDS.aRate}`);
    // Agent identity, checks and diffstat on the card.
    expect(within(rate!).getByText("claude-opus-5-5")).toBeTruthy();
    expect(rate!.textContent).toContain("Checks passed");
    expect(rate!.textContent).toMatch(/\+\d+/);
    // The haiku subagent's card carries the subagent marker.
    const idem = cards.find((c) => c.textContent?.includes("idempotency keys"))!;
    expect(within(idem).getByLabelText("subagent")).toBeTruthy();
    expect(idem.textContent).toContain("No checks yet");
    const flaky = cards.find((c) => c.textContent?.includes("flaky test"))!;
    expect(flaky.textContent).toContain("ci failing");
  });

  it("lists reports with blockers first", async () => {
    useMockServer();
    const { container } = renderInbox();
    await screen.findAllByTestId("awaiting-card");
    const reports = Array.from(container.querySelectorAll(".report"));
    expect(reports.length).toBe(4);
    expect(reports[0]!.className).toContain("level-blocker");
    expect(reports[0]!.textContent).toContain("Blocked: need read access");
  });

  it("celebrates inbox zero", async () => {
    useMockServer("inbox-zero");
    renderInbox();
    expect(await screen.findByText("Inbox zero")).toBeTruthy();
    expect(screen.queryAllByTestId("awaiting-card")).toHaveLength(0);
  });
});
