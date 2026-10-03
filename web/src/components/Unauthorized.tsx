import { createSignal, type JSX } from "solid-js";
import { TOKEN_KEY } from "../api/client";

/** Shown after any 401: the token is missing, stale (gitbots ui restarted) or wrong. */
export function Unauthorized(): JSX.Element {
  const [value, setValue] = createSignal("");
  const devPort = () => (location.port && location.port !== "7777" ? location.port : null);
  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const v = value().trim();
    const m = /token=([^&\s]+)/.exec(v);
    const token = m ? m[1]! : v;
    if (!token) return;
    try {
      sessionStorage.setItem(TOKEN_KEY, token);
    } catch {
      /* ignore */
    }
    location.hash = `token=${token}`;
    location.reload();
  };
  return (
    <main class="unauth" id="main">
      <div class="unauth-card">
        <div class="unauth-mark" aria-hidden="true">
          ⚿
        </div>
        <h1>Open the link printed by <code>gitbots ui</code></h1>
        <p>
          This page needs the one-time token from the terminal where you started <code>gitbots ui</code>. It looks like{" "}
          <code>http://127.0.0.1:7777/#token=…</code>. A new token is issued every time <code>gitbots ui</code> starts, so an old
          tab stops working after a restart.
        </p>
        <p class="muted">Hosted dashboard? Use your dashboard link with the project owner key (<code>#token=…</code>).</p>
        {devPort() && (
          <p class="muted">
            Running the Vite dev server? Open <code>http://localhost:{devPort()}/#token=…</code> with the same token.
          </p>
        )}
        <form class="unauth-form" onSubmit={submit}>
          <label for="token-input">Or paste the link or token</label>
          <div class="input-row">
            <input
              id="token-input"
              class="input mono"
              autocomplete="off"
              spellcheck={false}
              placeholder="http://127.0.0.1:7777/#token=…"
              value={value()}
              onInput={(e) => setValue(e.currentTarget.value)}
            />
            <button class="btn btn-primary" type="submit" disabled={!value().trim()}>
              Use token
            </button>
          </div>
        </form>
      </div>
    </main>
  );
}
