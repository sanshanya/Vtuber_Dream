import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

/** 开发代理落点 = live-server 默认端口（crates/live-server/src/app.rs DEFAULT_PORT）。 */
const DEV_PROXY_TARGET = "http://127.0.0.1:3781";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": { target: DEV_PROXY_TARGET, changeOrigin: true },
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    // globals:true 供 @testing-library 的自动 cleanup（每个用例后卸载，避免跨例污染）。
    globals: true,
  },
});
