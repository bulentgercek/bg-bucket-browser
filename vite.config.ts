import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The app starts this server itself and points its window at a fixed port, so
// moving to another port when that one is busy would leave the window loading
// nothing. Failing loudly is the useful behaviour here.
export default defineConfig({
  plugins: [react()],
  // The build output of both sides shares one terminal.
  clearScreen: false,
  // Dependencies are found by crawling from the app's own page only. The UI
  // harness page elsewhere in the project holds a placeholder the crawl
  // cannot resolve.
  optimizeDeps: { entries: ["index.html"] },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // The build writes to these constantly, and watching a file while it is
      // being written crashes the watcher on Windows.
      ignored: ["**/src-tauri/target/**", "**/src-tauri/gen/**"],
    },
  },
});
