import { fileURLToPath, URL } from "node:url";

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri 在 dev 模式下会连本地固定端口，端口被占用时直接失败，避免加载到错误的页面。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    watch: {
      // 其他工具/编辑器原子写文件时会先建临时目录再改名，watcher 扫到临时文件就 EBUSY 崩溃，
      // 一旦崩溃 tauri dev 会整体退出，所以把这些路径排除掉。
      ignored: [
        "**/src-tauri/**",
        "**/.*.tmpdir/**",
        "**/*.tmp",
        "**/*.swp",
        "**/*~",
        "**/.storydesk/**",
        "**/.story/**",
      ],
    },
  },
  build: {
    target: "chrome105",
    minify: "esbuild",
    sourcemap: false,
    // CodeMirror 一家就占了半条命：单独切出去，主包和编辑器包各自缓存
    rollupOptions: {
      output: {
        manualChunks: {
          react: ["react", "react-dom", "zustand"],
          codemirror: [
            "@codemirror/state",
            "@codemirror/view",
            "@codemirror/commands",
            "@codemirror/language",
            "@codemirror/lang-markdown",
            "@codemirror/search",
          ],
        },
      },
    },
  },
});
