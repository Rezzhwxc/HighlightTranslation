import { defineConfig } from 'vite';
import { resolve } from 'node:path';

// Чистый Vite без фреймворков: нужен только для резолва npm-импортов (@tauri-apps/*)
// и сборки двух HTML-страниц (index + overlay).
export default defineConfig({
  root: 'src',
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    outDir: '../dist',
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'src/index.html'),
        overlay: resolve(__dirname, 'src/overlay.html'),
      },
    },
  },
});
