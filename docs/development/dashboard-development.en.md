# Dashboard Development

Frontend development guide for Echo Agent Dashboard.

---

## Tech Stack

- React 19, TypeScript 5.7, Vite 6
- Tailwind CSS 4, Zustand 5, React Router 8
- i18next, Recharts, @dnd-kit
- Vitest 3 + Testing Library

## Setup

```bash
cd web
pnpm install --frozen-lockfile
pnpm dev    # Start dev server with HMR
```

Requires Node.js 24+ and pnpm 10+.

## Commands

```bash
pnpm dev          # Dev server
pnpm build        # Production build (tsc -b && vite build)
pnpm test --run   # Run tests
pnpm preview      # Preview build output
```

## Architecture

The Dashboard is a React SPA communicating with Gateway via:

- HTTP REST API: `/api/v1/*`
- WebSocket: `/ws` (dashboard management stream)

## Build Output

Built SPA goes to `web/dist/`, bundled into the wheel at `echo_agent/_bundled/dashboard/index.html` by `hatch_build.py`.

Users trigger first build via `echo-agent dashboard build`.

## Testing

```bash
pnpm test --run     # Single run
pnpm test           # Watch mode
```

Uses Vitest + @testing-library/react with jsdom environment.
