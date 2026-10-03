// Job logs for the mock runs (with ANSI colors, like real cargo output).

const G = "\x1b[1m\x1b[32m";
const R = "\x1b[1m\x1b[31m";
const Y = "\x1b[33m";
const X = "\x1b[0m";

const CRATES = [
  "proc-macro2 v1.0.101",
  "unicode-ident v1.0.19",
  "libc v0.2.176",
  "quote v1.0.41",
  "syn v2.0.106",
  "serde_core v1.0.228",
  "thiserror v2.0.17",
  "thiserror-impl v2.0.17",
  "bytes v1.10.1",
  "pin-project-lite v0.2.16",
  "tokio-macros v2.5.0",
  "mio v1.0.4",
  "socket2 v0.6.0",
  "tokio v1.47.1",
  "tracing-core v0.1.34",
  "tracing v0.1.41",
  "http v1.3.1",
  "http-body v1.0.1",
  "hyper v1.7.0",
  "tower v0.5.2",
  "axum-core v0.6.0",
  "axum v0.9.0",
  "dashmap v6.1.0",
  "sqlx-core v0.9.0",
  "sqlx v0.9.0",
  "redis v0.32.5",
  "time v0.3.44",
  "serde_json v1.0.145",
];

function compileLines(): string[] {
  const out: string[] = [];
  for (let round = 0; round < 6; round++) {
    for (const c of CRATES) out.push(`${G}   Compiling${X} ${c}`);
  }
  out.push(`${G}   Compiling${X} billing v0.4.2 (/work/billing)`);
  return out;
}

function testLines(names: string[], failing: Set<string>): string[] {
  return names.map((n) => `test ${n} ... ${failing.has(n) ? `${R}FAILED${X}` : `${G}ok${X}`}`);
}

const TESTS = [
  "config::tests::parses_env",
  "invoices::pdf::tests::streams_large_invoice",
  "invoices::pdf::tests::renders_line_items",
  "middleware::auth::tests::rejects_missing_key",
  "middleware::auth::tests::accepts_bearer",
  "middleware::request_id::tests::propagates_header",
  "refunds::tests::partial_refund",
  "refunds::tests::refund_exceeds_charge",
  "webhooks::retry::tests::backoff_caps_at_max",
  "webhooks::retry::tests::dedupes_duplicate_5xx",
  "webhooks::retry::tests::retries_on_503",
  "webhooks::retry::tests::gives_up_after_max_attempts",
  "webhooks::signature::tests::verifies_hmac",
  "webhooks::signature::tests::rejects_stale_timestamp",
];

export function failingTestLog(): string {
  const failing = new Set(["webhooks::retry::tests::retries_on_503"]);
  return [
    `${Y}+ cargo test --workspace${X}`,
    ...compileLines(),
    `${G}    Finished${X} \`test\` profile [unoptimized + debuginfo] target(s) in 1m 12s`,
    `${G}     Running${X} unittests src/lib.rs (target/debug/deps/billing-5f0c2a9d1e3b7c44)`,
    "",
    `running ${TESTS.length} tests`,
    ...testLines(TESTS, failing),
    "",
    "failures:",
    "",
    "---- webhooks::retry::tests::retries_on_503 stdout ----",
    "",
    "thread 'webhooks::retry::tests::retries_on_503' panicked at src/webhooks/retry.rs:212:9:",
    "assertion `left == right` failed",
    "  left: 2",
    " right: 3",
    "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
    "",
    "",
    "failures:",
    "    webhooks::retry::tests::retries_on_503",
    "",
    `test result: ${R}FAILED${X}. ${TESTS.length - 1} passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.21s`,
    "",
    `${R}error${X}: test failed, to rerun pass \`--lib\``,
    "",
  ].join("\n");
}

export function passingTestLog(): string {
  return [
    `${Y}+ cargo test --workspace${X}`,
    ...compileLines(),
    `${G}    Finished${X} \`test\` profile [unoptimized + debuginfo] target(s) in 58.40s`,
    `${G}     Running${X} unittests src/lib.rs (target/debug/deps/billing-5f0c2a9d1e3b7c44)`,
    "",
    `running ${TESTS.length + 4} tests`,
    ...testLines(TESTS, new Set()),
    `test rate_limit::allows_a_full_burst_then_limits ... ${G}ok${X}`,
    `test rate_limit::refills_over_time ... ${G}ok${X}`,
    `test rate_limit::keys_are_independent ... ${G}ok${X}`,
    `test rate_limit::exempt_keys_are_never_limited ... ${G}ok${X}`,
    "",
    `test result: ${G}ok${X}. ${TESTS.length + 4} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.87s`,
    "",
  ].join("\n");
}

export function fmtLog(): string {
  return [`${Y}+ cargo fmt --all --check${X}`, ""].join("\n");
}

export function clippyLog(): string {
  return [
    `${Y}+ cargo clippy --workspace --all-targets -- -D warnings${X}`,
    ...compileLines().map((l) => l.replace("Compiling", " Checking")),
    `${G}    Finished${X} \`dev\` profile [unoptimized + debuginfo] target(s) in 41.02s`,
    "",
  ].join("\n");
}

export function timeoutLog(): string {
  return [
    `${Y}+ cargo test --workspace${X}`,
    `${G}    Updating${X} crates.io index`,
    ...compileLines().slice(0, 40),
    `${R}error${X}: job exceeded timeout-secs = 900; killed after 15m 00s`,
    "",
  ].join("\n");
}
