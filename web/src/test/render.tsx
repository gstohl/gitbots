// Helpers for component tests: run the app's data layer against the mock server.
import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import type { Component, JSX } from "solid-js";
import { setTransport } from "../api/client";
import { buildWorld, type MockWorld } from "../mocks/fixtures";
import { mockFetch, resetWorld, type Scenario } from "../mocks/server";
import { DataProvider } from "../state";

export const NOW = Date.parse("2026-10-03T12:00:00Z");

export function useMockServer(scenario: Scenario = "default", tweak?: (w: MockWorld) => void): MockWorld {
  const w = buildWorld(NOW);
  tweak?.(w);
  const world = resetWorld(w, scenario);
  setTransport(mockFetch);
  return world;
}

/** A page at `path` inside a memory router and the app's data provider. */
export function routed(page: Component, path = "/", pattern = path): () => JSX.Element {
  const history = createMemoryHistory();
  history.set({ value: path });
  return () => (
    <MemoryRouter history={history} root={(p) => <DataProvider>{p.children}</DataProvider>}>
      <Route path={pattern} component={page} />
    </MemoryRouter>
  );
}
