import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Dev flow: `pointlock inspect --serve` hosts the data plane; the dev
// server proxies /api and /evidence to it (set POINTLOCK_HOST to the
// printed origin). Production: `pnpm build` → dist/, served by the Rust
// host via `--ui dist` (hash routing, no SPA fallback needed).
const host = process.env.POINTLOCK_HOST ?? "http://127.0.0.1:8317";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": host,
      "/evidence": host,
    },
  },
});
