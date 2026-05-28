# nodelite-server web

Vue 3 + Vite + TypeScript front-end for NodeLite, replacing the embedded `assets/*.html` UI.
See [docs/frontend-vue-refactor-plan.md](../../docs/frontend-vue-refactor-plan.md) for the full migration plan.

## Prerequisites

- Node.js **20+**
- pnpm **10.11+** (`corepack enable && corepack prepare pnpm@10.11.0 --activate`, or `brew install pnpm`)

## Scripts

| Command | What it does |
|---|---|
| `pnpm dev` | Vite dev server on `:5173`, proxies `/api` and `/ws` to `NODELITE_DEV_BACKEND` (default `http://localhost:8080`) |
| `pnpm build` | Type-check + Vite production build to `dist/` |
| `pnpm typecheck` | `vue-tsc --noEmit` |
| `pnpm lint` | ESLint with `--max-warnings=0` |
| `pnpm format` | Prettier write |
| `pnpm test` | Vitest one-shot |
| `pnpm test:watch` | Vitest watch mode |
| `pnpm e2e` | Playwright against the legacy backend (see `e2e/README.md`) |

## Local development

Start the Rust backend in one terminal and the Vite dev server in another:

```bash
# Terminal A
cargo run -p nodelite-server

# Terminal B
pnpm --dir nodelite-server/web dev
```

Open `http://localhost:5173` — the browser will prompt for Basic Auth (same credentials as the legacy UI).
The dev proxy forwards `/api/*` and `/ws` to the backend, so cookies and `Authorization` headers travel naturally.

## Stage 0 status

This directory currently contains the scaffold only: a hello-world `App.vue` that calls `/api/bootstrap` and prints the result.
Real infrastructure (router, Pinia, vue-i18n, WebSocket client, theme system) lands in Stage 1.
