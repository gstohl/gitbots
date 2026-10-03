import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { diffTotals, fileLineCount, filePath, parseUnifiedDiff, type DiffFile, type DiffHunk } from "../lib/diff";
import { formatNumber } from "../lib/format";

const STATUS_LETTER = { added: "A", deleted: "D", modified: "M", renamed: "R", copied: "C" } as const;
/** Files bigger than this start collapsed ("Load diff"), like GitHub. */
const AUTO_COLLAPSE_LINES = 400;

export function DiffView(props: { text: string }): JSX.Element {
  const files = createMemo(() => parseUnifiedDiff(props.text));
  const totals = createMemo(() => diffTotals(files()));
  // Index -> collapsed? Unset entries use the default for that file.
  const [overrides, setOverrides] = createSignal<Record<number, boolean>>({});
  const isCollapsed = (i: number, f: DiffFile) => overrides()[i] ?? fileLineCount(f) > AUTO_COLLAPSE_LINES;
  const toggle = (i: number, f: DiffFile) => setOverrides((o) => ({ ...o, [i]: !isCollapsed(i, f) }));
  const setAll = (v: boolean) => setOverrides(Object.fromEntries(files().map((_, i) => [i, v])));
  const [showTree, setShowTree] = createSignal(true);

  const jump = (i: number) => {
    setOverrides((o) => ({ ...o, [i]: false }));
    document.getElementById(`diff-file-${i}`)?.scrollIntoView({ block: "start" });
  };

  return (
    <Show
      when={files().length}
      fallback={
        <div class="diff-empty muted">{props.text.trim() ? "Couldn't parse this diff." : "No changes between base and head."}</div>
      }
    >
      <div class={`diffview${showTree() ? "" : " no-tree"}`}>
        <div class="diff-toolbar">
          <span class="diff-totals">
            <strong>{totals().files}</strong> {totals().files === 1 ? "file" : "files"} changed <span class="add">+{formatNumber(totals().additions)}</span>{" "}
            <span class="del">−{formatNumber(totals().deletions)}</span>
          </span>
          <span class="spacer" />
          <button type="button" class="btn btn-sm btn-ghost" onClick={() => setShowTree(!showTree())} aria-pressed={showTree()}>
            Files
          </button>
          <button type="button" class="btn btn-sm btn-ghost" onClick={() => setAll(false)}>
            Expand all
          </button>
          <button type="button" class="btn btn-sm btn-ghost" onClick={() => setAll(true)}>
            Collapse all
          </button>
        </div>
        <Show when={showTree()}>
          <nav class="diff-tree" aria-label="Changed files">
            <ul role="list">
              <For each={files()}>
                {(f, i) => (
                  <li>
                    <button type="button" class="diff-tree-item" onClick={() => jump(i())} title={filePath(f)}>
                      <span class={`fstatus fs-${f.status}`} aria-label={f.status}>
                        {STATUS_LETTER[f.status]}
                      </span>
                      {/* rtl + bdi: ellipsis at the start, path punctuation intact */}
                      <span class="diff-tree-path">
                        <bdi>{filePath(f)}</bdi>
                      </span>
                      <span class="diff-tree-stat">
                        <Show when={f.additions}>
                          <span class="add">+{f.additions}</span>
                        </Show>
                        <Show when={f.deletions}>
                          <span class="del">−{f.deletions}</span>
                        </Show>
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </nav>
        </Show>
        <div class="diff-files">
          <For each={files()}>
            {(f, i) => <FileBlock file={f} index={i()} collapsed={isCollapsed(i(), f)} onToggle={() => toggle(i(), f)} />}
          </For>
        </div>
      </div>
    </Show>
  );
}

function FileBlock(props: { file: DiffFile; index: number; collapsed: boolean; onToggle: () => void }): JSX.Element {
  const f = () => props.file;
  const big = () => fileLineCount(f()) > AUTO_COLLAPSE_LINES;
  return (
    <section class="diff-file" id={`diff-file-${props.index}`} aria-label={filePath(f())}>
      <header class="diff-file-head">
        <button
          type="button"
          class="diff-toggle"
          onClick={() => props.onToggle()}
          aria-expanded={!props.collapsed}
          aria-controls={`diff-body-${props.index}`}
          aria-label={`${props.collapsed ? "Expand" : "Collapse"} ${filePath(f())}`}
        >
          <span class={`chev${props.collapsed ? "" : " open"}`} aria-hidden="true">
            ▸
          </span>
        </button>
        <span class={`fstatus fs-${f().status}`} title={f().status}>
          {STATUS_LETTER[f().status]}
        </span>
        <span class="diff-path">
          <Show when={(f().status === "renamed" || f().status === "copied") && f().oldPath}>
            <span class="muted">{f().oldPath} → </span>
          </Show>
          {filePath(f())}
        </span>
        <Show when={f().similarity !== undefined}>
          <span class="muted small">{f().similarity}% similar</span>
        </Show>
        <Show when={f().oldMode && f().newMode && f().oldMode !== f().newMode}>
          <span class="muted small mono">
            {f().oldMode} → {f().newMode}
          </span>
        </Show>
        <span class="spacer" />
        <span class="diff-file-stat">
          <span class="add">+{f().additions}</span> <span class="del">−{f().deletions}</span>
        </span>
      </header>
      <Show when={!props.collapsed}>
        <div id={`diff-body-${props.index}`} class="diff-body">
          <Show when={f().binary}>
            <div class="diff-note muted">Binary file not shown.</div>
          </Show>
          <Show when={!f().binary && f().hunks.length === 0}>
            <div class="diff-note muted">
              {f().status === "renamed" ? "File renamed without changes." : "No content changes (mode or empty file)."}
            </div>
          </Show>
          <Show when={f().hunks.length}>
            <div class="diff-scroll">
              <table class="diff-table">
                <colgroup>
                  <col class="col-ln" />
                  <col class="col-ln" />
                  <col />
                </colgroup>
                <For each={f().hunks}>{(h) => <HunkRows hunk={h} />}</For>
              </table>
            </div>
          </Show>
        </div>
      </Show>
      <Show when={props.collapsed && big()}>
        <button type="button" class="diff-load" onClick={() => props.onToggle()}>
          Large diff ({formatNumber(fileLineCount(f()))} lines) is collapsed. <u>Load diff</u>
        </button>
      </Show>
    </section>
  );
}

const SIGN = { add: "+", del: "-", ctx: " ", meta: "\\" } as const;

function HunkRows(props: { hunk: DiffHunk }): JSX.Element {
  return (
    <tbody>
      <tr class="dl dl-hunk">
        <td class="ln" />
        <td class="ln" />
        <td class="code">
          {props.hunk.header}
        </td>
      </tr>
      <For each={props.hunk.lines}>
        {(l) => (
          <tr class={`dl dl-${l.type}`}>
            <td class="ln" data-ln={l.oldNo ?? ""} />
            <td class="ln" data-ln={l.newNo ?? ""} />
            <td class="code" data-sign={SIGN[l.type]}>
              {l.type === "meta" ? `\\ ${l.text}` : l.text}
            </td>
          </tr>
        )}
      </For>
    </tbody>
  );
}
