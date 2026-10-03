import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api/client";
import { pendingReview } from "../api/pending";
import type { BoardAttempt, ProjectInfo } from "../api/types";
import { ReviewPanel } from "../components/ReviewPanel";
import { buildWorld, IDS } from "../mocks/fixtures";
import { NOW } from "./render";

function fixture(canDecide: boolean, attemptId = IDS.aRate): { project: ProjectInfo; attempt: BoardAttempt } {
  const w = buildWorld(NOW);
  w.project.can_decide = canDecide;
  return { project: w.project, attempt: w.attempts.find((a) => a.id === attemptId)! };
}

const submitButton = () => screen.getByRole("button", { name: /^(Accept|Reject|Request changes|Submitting)/ }) as HTMLButtonElement;

describe("ReviewPanel", () => {
  it("is disabled, with an explanation, when can_decide is false", () => {
    const { project, attempt } = fixture(false);
    const submit = vi.fn();
    render(() => <ReviewPanel attempt={attempt} project={project} submit={submit} />);
    expect(submitButton().disabled).toBe(true);
    for (const r of screen.getAllByRole("radio")) expect(r.matches(":disabled")).toBe(true);
    expect(screen.getByTestId("review-disabled").textContent).toContain("Read-only");
    fireEvent.click(submitButton());
    expect(submit).not.toHaveBeenCalled();
  });

  it("is disabled when the attempt isn't submitted", () => {
    const { project, attempt } = fixture(true, IDS.aAxum);
    render(() => <ReviewPanel attempt={attempt} project={project} />);
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByTestId("review-disabled").textContent).toContain("merged");
  });

  it("shows who may decide per the mandate", () => {
    const { project, attempt } = fixture(true);
    render(() => <ReviewPanel attempt={attempt} project={project} />);
    expect(screen.getByText(/Merge into main/)).toBeTruthy();
    expect(screen.getByText(/a human maintainer or above \(protected\)/)).toBeTruthy();
    expect(screen.getByText(/@gstohl \(owner\)/)).toBeTruthy();
  });

  it("posts accept + merge and reports the outcome", async () => {
    const { project, attempt } = fixture(true);
    const submit = vi.fn(async () => ({ attempt: attempt.id, decision: "accept" as const, merged: "0123456789abcdef" }));
    const onDecided = vi.fn();
    render(() => <ReviewPanel attempt={attempt} project={project} submit={submit} onDecided={onDecided} />);
    fireEvent.click(screen.getByLabelText(/Merge into/));
    expect(submitButton().textContent).toBe("Accept and merge into main");
    fireEvent.input(screen.getByRole("textbox"), { target: { value: "LGTM" } });
    fireEvent.click(submitButton());
    expect(await screen.findByText(/merged as/)).toBeTruthy();
    expect(submit).toHaveBeenCalledWith(attempt.id, { decision: "accept", reason: "LGTM", merge: true });
    expect(onDecided).toHaveBeenCalled();
  });

  it("shows the server's error verbatim", async () => {
    const { project, attempt } = fixture(true);
    const submit = vi.fn(async () => {
      throw new ApiError(403, "merging into main needs a human with role maintainer");
    });
    render(() => <ReviewPanel attempt={attempt} project={project} submit={submit} />);
    fireEvent.click(screen.getByLabelText("Reject"));
    fireEvent.click(submitButton());
    const err = await screen.findByTestId("review-error");
    expect(err.textContent).toBe("merging into main needs a human with role maintainer");
    expect(err.closest("[aria-live]")).toBeTruthy();
  });

  it("hosted: a 202 queued answer shows pending sync and locks the form", async () => {
    const { project, attempt } = fixture(true);
    project.hosted = true;
    const submit = vi.fn(async () => ({ queued: true as const, outbox: "obx_42" }));
    render(() => <ReviewPanel attempt={attempt} project={project} submit={submit} />);
    fireEvent.click(submitButton());
    const q = await screen.findByTestId("review-queued");
    expect(q.textContent).toContain("Queued: applied on the next gitbots sync");
    expect(pendingReview(attempt.id)?.outbox).toBe("obx_42");
    expect(submitButton().disabled).toBe(true);
  });
});
