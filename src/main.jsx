import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import logger, { LogLevel } from "./utils/logger.js";
import errorCatcher from "./utils/errorCatcher.js";
import { emit } from '@tauri-apps/api/event';

Promise.all([
  logger.init({
    consoleEnabled: true,
    fileLoggingEnabled: true,
    notificationsEnabled: true,
    overrideConsole: true,
    notificationThreshold: LogLevel.WARNING,
    consoleThreshold: LogLevel.DEBUG
  }).then(() => {
    logger.info('Application initialized', 'main');
  }),
  
  Promise.resolve().then(() => {
    setTimeout(() => {
      errorCatcher.init();
    }, 500);
  })
]).catch(error => {
  // We still need to use console.error here since logger isn't initialized yet
  console.error('Failed to initialize application:', error);
});

// Start rendering the app as soon as possible
// The ReactDOM.createRoot call is what starts the render, so move this up
const root = ReactDOM.createRoot(document.getElementById("root"));

// Render with a loading state first for better LCP
root.render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

// When React is mounted, hide the static loading overlay and notify backend
try {
  if (typeof window.__hideAppLoading === 'function') window.__hideAppLoading();
  // Notify backend that renderer is ready (non-blocking)
  try { emit('renderer-ready', {}); } catch(e) { console.debug('renderer-ready emit failed', e); }
} catch (e) {
  console.debug('Error hiding loading overlay', e);
}

// Test error catching by adding a test error event after 5 seconds
if (import.meta.env.DEV) {
  setTimeout(() => {
    try {
      // Only in dev mode, for testing purposes
      logger.info('Testing error catching mechanism', 'ErrorTest');
      throw new Error('This is a test error to verify error capturing works');
    } catch (error) {
      errorCatcher.captureException(error, { 
        source: 'ErrorTest',
        details: 'This error was intentionally thrown to test the error capturing system'
      });
    }
  }, 5000);
}
