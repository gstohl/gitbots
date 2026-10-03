import { A } from "@solidjs/router";
import type { JSX } from "solid-js";
import { EmptyState } from "../components/States";

export default function NotFoundPage(): JSX.Element {
  return (
    <div class="page">
      <EmptyState title="Nothing here" icon={<span>404</span>}>
        <p>
          That page doesn't exist. Head back to the <A href="/">inbox</A>.
        </p>
      </EmptyState>
    </div>
  );
}
