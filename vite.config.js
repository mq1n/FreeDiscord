import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'https://discord.com',
        changeOrigin: true,
        secure: true,
        headers: {
          'Origin': 'https://discord.com',
          'Referer': 'https://discord.com/'
        },
        configure: (proxy, _options) => {
          proxy.on('error', (err, _req, _res) => {
            console.log('proxy error', err);
          });
          proxy.on('proxyReq', (proxyReq, req, _res) => {
            console.log('Sending Request to the Target:', req.method, req.url);
          });
          proxy.on('proxyRes', (proxyRes, req, _res) => {
            console.log('Received Response from the Target:', proxyRes.statusCode, req.url);
          });
        }
      },
      '/gateway': {
        target: 'wss://gateway.discord.gg',
        ws: true,
        changeOrigin: true,
        secure: true
      },
      '/remote-auth': {
        target: 'wss://remote-auth-gateway.discord.gg',
        ws: true,
        changeOrigin: true,
        secure: true
      }
    }
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
}); 