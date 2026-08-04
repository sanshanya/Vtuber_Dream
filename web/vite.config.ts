import react from "@vitejs/plugin-react";
import viteCompression from "vite-plugin-compression";
import { defineConfig } from "vitest/config";

/** 开发代理落点 = live-server 默认端口（crates/live-server/src/app.rs DEFAULT_PORT）。 */
const DEV_PROXY_TARGET = "http://127.0.0.1:3781";

export default defineConfig({
  plugins: [
    react(),
    // Z4/P0-7：双算法预压缩产物（.gz + .br 与源文件同目录同名后缀）。
    // 服务端 live-server ServeDir::precompressed_* 按 Accept-Encoding 协商取用；
    // 不删原文件（未知客户端回落 identity）。阈值 1KB：小件不值得开编码协商。
    viteCompression({ algorithm: "gzip", ext: ".gz", threshold: 1024 }),
    viteCompression({ algorithm: "brotliCompress", ext: ".br", threshold: 1024 }),
  ],
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
