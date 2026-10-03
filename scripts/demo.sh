#!/usr/bin/env bash
# Seed a demo repo where several agents work concurrently, for the UI, the
# competition video and manual testing.
#
#   scripts/demo.sh [DIR]          # default: a fresh temp dir
#
# Uses ./target/debug/gitbots (build it first) or $GITBOTS. Everything, including
# workrooms and git config, stays inside DIR.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GITBOTS="${GITBOTS:-$ROOT/target/debug/gitbots}"
[ -x "$GITBOTS" ] || { echo "build first: cargo build -p gitbots" >&2; exit 1; }

DIR="${1:-$(mktemp -d -t gitbots-demo)}"
mkdir -p "$DIR"
DIR="$(cd "$DIR" && pwd)"
REPO="$DIR/repo"
[ -e "$REPO" ] && { echo "$REPO already exists" >&2; exit 1; }

# Isolate from the caller's agent harness and git config.
unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT GITBOTS_SESSION || true
for v in $(env | grep -o '^CODEX_[A-Z_]*' || true); do unset "$v"; done
export GITBOTS_HOME="$DIR/gitbots-home" GIT_CONFIG_GLOBAL="$DIR/gitconfig" GIT_CONFIG_NOSYSTEM=1
: > "$GIT_CONFIG_GLOBAL"
git config --global user.name gstohl
git config --global user.email dominik@example.com
git config --global init.defaultBranch main

json() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
say() { printf '\033[1m== %s\033[0m\n' "$*"; }

say "repo + gitbots init"
mkdir -p "$REPO" && cd "$REPO"
git init -q
mkdir -p src
cat > src/app.py <<'EOF'
def health():
    return {"ok": True}
EOF
printf '# shop-api\n\nA tiny demo service.\n' > README.md
git add . && git commit -qm "initial service"
"$GITBOTS" init --commit --goal "Ship v1 of the shop API with agents doing the work" >/dev/null

cat > .gitbots/actions/ci.toml <<'EOF'
name = "ci"
on = ["attempt.submitted", "manual"]

[jobs.lint]
steps = [{ name = "no TODOs", run = "! grep -rn TODO src/" }]

[jobs.test]
needs = ["lint"]
steps = [
  { name = "compile", run = "python3 -m py_compile src/*.py" },
  { name = "smoke", run = "python3 -c 'import sys; sys.path.insert(0, \"src\"); import app; assert app.health()[\"ok\"]'" },
]
EOF
git add .gitbots && git commit -qm "ci workflow"
git branch integration

say "agents start sessions"
start() { "$GITBOTS" --json session start "$@" | json '["id"]'; }
OPUS=$(start --provider anthropic --model claude-opus-5-5 --client claude-code --role implementer --label "lead dev")
CODEX=$(start --provider openai --model gpt-5-codex --client codex --role implementer)
HAIKU=$(GITBOTS_SESSION=$OPUS start --provider anthropic --model claude-haiku-4-5 --client claude-code --role tester --label "opus subagent")
REVIEWER=$(start --provider openai --model gpt-5 --client chatgpt --role reviewer)

say "human files tasks"
task() { "$GITBOTS" --json task create "$@" | json '["task"]'; }
T_CART=$(task "Add a shopping cart endpoint" --label api)
T_AUTH=$(task "Add token auth middleware" --label security)
T_DOCS=$(task "Document the API in README" --label docs)
T_RATE=$(task "Rate-limit the public endpoints" --label api)

# One agent's whole attempt: start, edit, commit, submit.
work() { # session task base file content message summary
  local ses=$1 task=$2 base=$3 file=$4 content=$5 msg=$6 summary=$7
  local room
  room=$(GITBOTS_SESSION=$ses "$GITBOTS" --json attempt start "$task" --base "$base" | json '["workroom"]')
  mkdir -p "$room/$(dirname "$file")"
  printf '%s\n' "$content" >> "$room/$file"
  git -C "$room" add -A && git -C "$room" commit -qm "$msg"
  (cd "$room" && "$GITBOTS" --json attempt submit --summary "$summary" >/dev/null) || true
  echo "$room"
}

say "three agents work concurrently"
work "$OPUS" "$T_CART" integration src/cart.py 'def add_item(cart, sku, qty=1):
    cart[sku] = cart.get(sku, 0) + qty
    return cart' "Add cart endpoint" "cart add/remove with quantities" > "$DIR/room-cart" &
work "$CODEX" "$T_AUTH" integration src/auth.py 'def check(token):
    # TODO: real verification
    return token == "secret"' "Add auth middleware" "bearer token check (stub)" > "$DIR/room-auth" &
work "$HAIKU" "$T_DOCS" integration README.md '
## Endpoints

- `GET /health`
- `POST /cart/items`' "Document endpoints" "README endpoint list" > "$DIR/room-docs" &
wait

say "a second attempt at auth, by opus, competes with codex"
work "$OPUS" "$T_AUTH" integration src/auth.py 'import hmac

def check(token, expected):
    return hmac.compare_digest(token, expected)' "Constant-time auth check" "hmac compare, no TODOs" > "$DIR/room-auth2"

say "an agent tries to edit the mandate (refused)"
ROOM=$(GITBOTS_SESSION=$CODEX "$GITBOTS" --json attempt start "$T_RATE" | json '["workroom"]')
sed -i.bak 's/"assisted"/"autonomous"/' "$ROOM/.gitbots/manifest.json" && rm -f "$ROOM/.gitbots/manifest.json.bak"
git -C "$ROOM" commit -qam "Loosen autonomy so I can merge myself"
(cd "$ROOM" && "$GITBOTS" attempt submit 2>&1 | head -3) || true

say "handoff, reviews, reports"
CART_ATT=$(GITBOTS_SESSION=$OPUS "$GITBOTS" --json log --kind attempt.started -n 100 \
  | python3 -c "import json,sys; print([e['data']['attempt'] for e in json.load(sys.stdin) if e['data']['task']=='$T_CART'][0])")
DOCS_ATT=$(GITBOTS_SESSION=$OPUS "$GITBOTS" --json log --kind attempt.started -n 100 \
  | python3 -c "import json,sys; print([e['data']['attempt'] for e in json.load(sys.stdin) if e['data']['task']=='$T_DOCS'][0])")
GITBOTS_SESSION=$REVIEWER "$GITBOTS" review "$DOCS_ATT" accept --merge --reason "clear and accurate" >/dev/null
GITBOTS_SESSION=$REVIEWER "$GITBOTS" review "$CART_ATT" changes --reason "remove_item is missing" >/dev/null
GITBOTS_SESSION=$OPUS "$GITBOTS" handoff "$CART_ATT" --to-session "$HAIKU" --note "please add remove_item + tests" >/dev/null
GITBOTS_SESSION=$OPUS "$GITBOTS" report "Two auth attempts are ready" --body "Codex's still has a TODO (lint fails); mine uses hmac. Pick one." --level warning >/dev/null
GITBOTS_SESSION=$CODEX "$GITBOTS" report "Need a decision on rate limits" --body "Per-IP or per-token? I can't continue without it." --level blocker >/dev/null
GITBOTS_SESSION=$HAIKU "$GITBOTS" trace "pytest" --input "pytest -q tests/" --duration-ms 2140 >/dev/null

say "done"
"$GITBOTS" status
cat <<EOF

Demo repo: $REPO
Run the UI:
  cd $REPO && GITBOTS_HOME=$GITBOTS_HOME GIT_CONFIG_GLOBAL=$GIT_CONFIG_GLOBAL $GITBOTS ui --assets $ROOT/web/dist
EOF
