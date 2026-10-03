import { For, onMount, type JSX } from "solid-js";
import { SHORTCUT_HELP } from "../lib/keyboard";

export function ShortcutHelp(props: { onClose: () => void }): JSX.Element {
  let closeBtn: HTMLButtonElement | undefined;
  onMount(() => closeBtn?.focus());
  return (
    <div class="overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="dialog shortcut-dialog" role="dialog" aria-modal="true" aria-labelledby="shortcut-title">
        <div class="dialog-head">
          <h2 id="shortcut-title">Keyboard shortcuts</h2>
          <button ref={closeBtn} type="button" class="icon-btn" onClick={() => props.onClose()} aria-label="Close">
            ✕
          </button>
        </div>
        <div class="shortcut-grid">
          <For each={SHORTCUT_HELP}>
            {(g) => (
              <section>
                <h3>{g.title}</h3>
                <dl>
                  <For each={g.items}>
                    {(it) => (
                      <div class="shortcut-row">
                        <dt>
                          <For each={it.keys}>{(k) => <kbd class="kbd">{k}</kbd>}</For>
                        </dt>
                        <dd>{it.description}</dd>
                      </div>
                    )}
                  </For>
                </dl>
              </section>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}
