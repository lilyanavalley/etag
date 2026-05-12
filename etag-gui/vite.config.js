import { defineConfig } from "vite";
import tailwindcss from "@tailwindcss/vite";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [tailwindcss()],

  // Vite dev server settings aligned with Tauri's expected dev URL.
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Avoid unnecessary reloads when Rust sources change.
      ignored: ["**/src-tauri/**"],
    },
  },

  // Tauri-specific environment variable prefixes.
  envPrefix: ["VITE_", "TAURI_"],

  build: {
    // Tauri supports Chromium 105+.
    target: "chrome105",
    // Disable minification in debug builds, enable source maps.
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
