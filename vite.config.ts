import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
  // 防止 Vite 清除 Rust 显示的控制台输出
  clearScreen: false,
  // Tauri 期望固定的主机地址
  server: {
    port: 1420,
    strictPort: true,
  },
  // 通过环境变量感知 Tauri 环境
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Tauri 支持 Chromium (ESNext) 和 WebKit，取较保守的目标
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    // 生产构建不使用 source map
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
