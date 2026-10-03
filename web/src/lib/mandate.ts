// Who may decide what, per the mandate (docs/ARCHITECTURE.md, "Manifest").

import type { Actor, Approver, PrincipalRole, ProjectInfo } from "../api/types";

const RANK: Record<PrincipalRole, number> = { reviewer: 1, maintainer: 2, owner: 3 };

/** The viewer's principal role, if the viewer is a listed human principal. */
export function viewerRole(p: ProjectInfo): PrincipalRole | null {
  const v = p.viewer;
  if (!v || v.type !== "human") return null;
  const handle = v.handle.toLowerCase();
  const email = v.email?.toLowerCase();
  const match = p.manifest.mandate.principals.find(
    (x) => x.handle.toLowerCase() === handle || (email !== undefined && x.email?.toLowerCase() === email),
  );
  return match?.role ?? null;
}

/** Whether an actor with `role` (null = not a principal) satisfies `approver`. */
export function satisfies(approver: Approver, actor: Actor | null, role: PrincipalRole | null): boolean {
  if (approver === "any") return actor !== null && actor.type !== "unknown";
  if (!actor || actor.type !== "human" || !role) return false;
  return RANK[role] >= RANK[approver];
}

export function approverText(a: Approver): string {
  return a === "any" ? "anyone (agent or human)" : `a human ${a}${a === "owner" ? "" : " or above"}`;
}

/** Simple glob match supporting `**`, `*` and `?`, case-insensitive like the mandate. */
export function globMatch(glob: string, path: string): boolean {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]!;
    if (c === "*") {
      if (glob[i + 1] === "*") {
        i++;
        if (glob[i + 1] === "/") {
          i++;
          re += "(?:.*/)?";
        } else re += ".*";
      } else re += "[^/]*";
    } else if (c === "?") re += "[^/]";
    else re += c.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  }
  return new RegExp(`^${re}$`, "i").test(path);
}
