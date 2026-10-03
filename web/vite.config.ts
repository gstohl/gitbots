/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// `gitbots ui` listens on 127.0.0.1:7777 by default (override with
// GITBOTS_API_TARGET=http://127.0.0.1:7799). `changeOrigin` rewrites the Host
// header to the target so the server's DNS-rebinding guard accepts proxied
// requests.
const env = (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};
const API_TARGET = env.GITBOTS_API_TARGET ?? "http://127.0.0.1:7777";

export default defineConfig(({ mode }) => ({
  plugins: [solid()],
  // Root-relative assets: served from `/` by `gitbots ui --assets web/dist` and
  // by the hosted Worker (static assets with SPA fallback).
  base: "/",
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": { target: API_TARGET, changeOrigin: true },
    },
    // Tests read crates/gitbots-core/src/event.rs to check the kind list hasn't drifted.
    fs: mode === "test" ? { allow: [".", "../crates/gitbots-core/src"] } : undefined,
  },
  build: {
    outDir: "dist",
    target: "es2022",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test/setup.ts"],
  },
}));
