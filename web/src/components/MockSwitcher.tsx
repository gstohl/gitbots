import { createResource, For, Show, type JSX } from "solid-js";
import type { Scenario } from "../mocks/server";

/** Mock mode only: pick a scenario (viewer role, inbox zero, 401...). Reloads the page. */
export function MockSwitcher(): JSX.Element {
  const [mod] = createResource(() => import("../mocks/server"));
  return (
    <Show when={mod()}>
      {(m) => (
        <label class="mock-switch" title="Mock data (VITE_GITBOTS_MOCK=1). Pick a scenario.">
          <span class="mock-badge">mock</span>
          <select
            class="select select-sm"
            aria-label="Mock scenario"
            value={m().getScenario()}
            onChange={(e) => {
              m().setScenario(e.currentTarget.value as Scenario);
              location.reload();
            }}
          >
            <For each={m().SCENARIOS}>{(s) => <option value={s.id}>{s.label}</option>}</For>
          </select>
        </label>
      )}
    </Show>
  );
}
