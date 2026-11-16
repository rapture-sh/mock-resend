import { resolve } from "node:path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

/**
 * The console is served by the mock-resend binary from `dist`, under `/_admin/console/`.
 *
 * That base path is not cosmetic. mock-resend serves the real Resend API on the same
 * port, and Resend owns `/emails/{id}` — so a console route of the same shape at the
 * root would collide with the API it exists to show. Everything under `/_admin` is ours
 * by definition, which is the whole reason the admin surface lives there.
 *
 * In development Vite serves the app and forwards `/_admin` to a running instance, so
 * the app talks to one origin either way and needs no CORS.
 */
export default defineConfig({
  base: "/_admin/console/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": resolve(import.meta.dirname, "./src"),
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/_admin": {
        target: "http://localhost:9100",
        changeOrigin: true,
      },
    },
  },
  build: {
    // Read by `rust-embed` in the admin crate, and by `cargo xtask dist`.
    outDir: "dist",
    emptyOutDir: true,
  },
})
