import { Route, Router, useNavigate, type RouteSectionProps } from "@solidjs/router";
import { createSignal, lazy, onCleanup, onMount, Show, Suspense, type JSX } from "solid-js";
import { unauthorized } from "./api/client";
import { startLive } from "./api/live";
import { ShortcutHelp } from "./components/ShortcutHelp";
import { Skeleton } from "./components/States";
import { TopBar } from "./components/TopBar";
import { Unauthorized } from "./components/Unauthorized";
import { installShortcuts } from "./lib/keyboard";
import InboxPage from "./pages/Inbox";
import { DataProvider } from "./state";

// The inbox is the landing page; everything else loads on demand.
const BoardPage = lazy(() => import("./pages/Board"));
const AttemptPage = lazy(() => import("./pages/Attempt"));
const ActivityPage = lazy(() => import("./pages/Activity"));
const AgentsPage = lazy(() => import("./pages/Agents"));
const SessionsPage = lazy(() => import("./pages/Sessions"));
const MandatePage = lazy(() => import("./pages/Mandate"));
const NotFoundPage = lazy(() => import("./pages/NotFound"));

export function Shell(props: RouteSectionProps): JSX.Element {
  const navigate = useNavigate();
  const [help, setHelp] = createSignal(false);
  onMount(() => {
    const off = installShortcuts({
      navigate: (p) => navigate(p),
      toggleHelp: () => setHelp((h) => !h),
      closeOverlay: () => {
        if (!help()) return false;
        setHelp(false);
        return true;
      },
    });
    onCleanup(off);
  });
  return (
    <Show when={!unauthorized()} fallback={<Unauthorized />}>
      <DataProvider>
        <a class="skip-link" href="#main">
          Skip to content
        </a>
        <TopBar onHelp={() => setHelp(true)} />
        <main id="main" class="main" tabindex="-1">
          <Suspense fallback={<div class="page"><Skeleton rows={6} /></div>}>{props.children}</Suspense>
        </main>
        <Show when={help()}>
          <ShortcutHelp onClose={() => setHelp(false)} />
        </Show>
      </DataProvider>
    </Show>
  );
}

export function AppRoutes(): JSX.Element {
  return (
    <>
      <Route path="/" component={InboxPage} />
      <Route path="/board" component={BoardPage} />
      <Route path="/attempts/:id" component={AttemptPage} />
      <Route path="/activity" component={ActivityPage} />
      <Route path="/agents" component={AgentsPage} />
      <Route path="/sessions" component={SessionsPage} />
      <Route path="/mandate" component={MandatePage} />
      <Route path="*404" component={NotFoundPage} />
    </>
  );
}

export default function App(): JSX.Element {
  onMount(() => onCleanup(startLive(3000)));
  return (
    <Router root={Shell}>
      <AppRoutes />
    </Router>
  );
}
