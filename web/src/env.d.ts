/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** "1" serves fixtures from src/mocks instead of calling `gitbots ui`. */
  readonly VITE_GITBOTS_MOCK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
