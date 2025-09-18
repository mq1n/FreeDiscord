import React from 'react';
import logger from './logger';
import { invoke } from '@tauri-apps/api/core';

class ErrorCatcher {
  constructor() {
    this.enabled = false;
    this.prevComponentDidCatch = null;
    this.maxErrors = 10;
    this.errorCount = 0;
    this.errorHistory = [];
    this.reportedErrors = new Set();
    this.originalFetch = null;
    this.originalXhrOpen = null;
    this.originalXhrSend = null;
  }

  init() {
    if (this.enabled) return;
    this.enabled = true;
    
    window.addEventListener('error', this.handleError.bind(this));
    
    window.addEventListener('unhandledrejection', this.handleRejection.bind(this));
    
    this.prevComponentDidCatch = React.Component.prototype.componentDidCatch;
    
    if (this.prevComponentDidCatch) {
      React.Component.prototype.componentDidCatch = function(error, errorInfo) {
        errorCatcher.captureException(error, {
          source: 'React',
          details: JSON.stringify(errorInfo, null, 2)
        });
        
        if (this.prevComponentDidCatch) {
          this.prevComponentDidCatch.call(this, error, errorInfo);
        }
      };
    }
    
    // Patch fetch API to catch network errors
    this.patchFetchAPI();
    
    // Patch XMLHttpRequest to catch API errors
    this.patchXMLHttpRequest();
    
    logger.info('Error catcher initialized', 'ErrorCatcher');
    return this;
  }

  /**
   * Handle window.onerror events
   */
  handleError(event) {
    const { message, filename, lineno, colno, error } = event;
    
    const errorInfo = {
      message,
      source: 'Window.onerror',
      details: JSON.stringify({
        filename,
        line: lineno,
        column: colno,
        stack: error?.stack || 'No stack trace available'
      }, null, 2)
    };
    
    this.reportToBackend(error || new Error(message), errorInfo);
    
    // Log to the local logger as well
    logger.error(message, errorInfo.source, errorInfo.details);
  }

  /**
   * Handle unhandled promise rejections
   */
  handleRejection(event) {
    const error = event.reason;
    const message = error?.message || 'Unhandled Promise Rejection';
    
    const errorInfo = {
      message,
      source: 'UnhandledRejection',
      details: JSON.stringify({
        stack: error?.stack || 'No stack trace available'
      }, null, 2)
    };
    
    this.reportToBackend(error || new Error(message), errorInfo);
    
    // Log to the local logger as well
    logger.error(message, errorInfo.source, errorInfo.details);
  }

  /**
   * Manually capture an exception and send it to Sentry
   */
  captureException(error, options = {}) {
    const message = error?.message || 'Unknown error';
    const source = options.source || 'JavaScript';
    const details = options.details || JSON.stringify({
      stack: error?.stack || 'No stack trace available'
    }, null, 2);
    
    this.reportToBackend(error, { message, source, details });
    
    // Log to the local logger as well
    logger.error(message, source, details);
  }

  /**
   * Report an error to the backend server
   * @param {Error} error - The error object
   * @param {object} errorInfo - Additional information about the error
   */
  async reportToBackend(error, errorInfo) {
    try {
      // Check if we're in development mode
      const isDev = import.meta.env.VITE_NODE_ENV === 'development';
      
      // In development, only log to console
      if (isDev) {
        logger.debug('Development mode: Error would be reported to backend', 'ErrorCatcher');
        return;
      }
      
      // Only attempt reporting if Tauri is available
      const isTauri = typeof window !== 'undefined' && (
        // v1 bridge
        window.__TAURI_IPC__ !== undefined ||
        // v2 namespace
        (window.__TAURI__ && window.__TAURI__.core && typeof window.__TAURI__.core.invoke === 'function')
      );

      if (!isTauri) {
        logger.debug('Tauri not available, skipping backend logging', 'ErrorCatcher');
        return;
      }

      // Try to report the error using Tauri's JS API
      try {
        await invoke('capture_exception_from_js', {
          message: error.message || 'Unknown error',
          stack: error.stack || '',
          source: errorInfo.source || 'JavaScript',
          details: errorInfo.details || ''
        });
        logger.debug('Error reported to backend', 'ErrorCatcher');
      } catch (e) {
        // If the command doesn't exist or fails, log it but don't throw
        logger.debug(`Error reporting failed: ${e}`, 'ErrorCatcher');
      }
    } catch (reportError) {
      logger.error('Failed to report error to backend', 'ErrorCatcher', reportError.toString());
    }
  }

  /**
   * Clean up and remove the error handlers
   */
  cleanup() {
    if (!this.enabled) {
      return;
    }
    
    window.removeEventListener('error', this.handleError);
    window.removeEventListener('unhandledrejection', this.handleRejection);
    
    // Restore original React error handling
    if (this.prevComponentDidCatch) {
      React.Component.prototype.componentDidCatch = this.prevComponentDidCatch;
    }
    
    // Restore original fetch API if we patched it
    if (this.originalFetch) {
      window.fetch = this.originalFetch;
    }
    
    // Restore original XMLHttpRequest methods if we patched them
    if (this.originalXhrOpen && this.originalXhrSend) {
      XMLHttpRequest.prototype.open = this.originalXhrOpen;
      XMLHttpRequest.prototype.send = this.originalXhrSend;
    }
    
    this.enabled = false;
    logger.info('Error catcher cleaned up', 'ErrorCatcher');
  }

  /**
   * Patch the fetch API to catch network errors
   */
  patchFetchAPI() {
    this.originalFetch = window.fetch;
    const self = this;
    
    window.fetch = function(...args) {
      return self.originalFetch.apply(this, args)
        .catch(error => {
          // Capture API errors
          const url = typeof args[0] === 'string' ? args[0] : args[0]?.url || 'unknown';
          self.captureException(error, {
            source: 'FetchAPI',
            details: JSON.stringify({
              url,
              method: args[1]?.method || 'GET',
              error: error.toString()
            }, null, 2)
          });
          
          // Re-throw the error to maintain original behavior
          throw error;
        });
    };
  }
  
  /**
   * Patch XMLHttpRequest to catch API errors
   */
  patchXMLHttpRequest() {
    this.originalXhrSend = XMLHttpRequest.prototype.send;
    this.originalXhrOpen = XMLHttpRequest.prototype.open;
    const self = this;
    
    // Store request info
    XMLHttpRequest.prototype.open = function(method, url) {
      this._errorCatcherData = {
        method,
        url
      };
      return self.originalXhrOpen.apply(this, arguments);
    };
    
    // Monitor for errors
    XMLHttpRequest.prototype.send = function() {
      const xhr = this;
      
      // Listen for network errors
      xhr.addEventListener('error', function(e) {
        const requestData = xhr._errorCatcherData || { method: 'unknown', url: 'unknown' };
        self.captureException(new Error('XMLHttpRequest failed'), {
          source: 'XMLHttpRequest',
          details: JSON.stringify({
            url: requestData.url,
            method: requestData.method,
            status: xhr.status,
            statusText: xhr.statusText
          }, null, 2)
        });
      });
      
      // Listen for timeout errors
      xhr.addEventListener('timeout', function() {
        const requestData = xhr._errorCatcherData || { method: 'unknown', url: 'unknown' };
        self.captureException(new Error('XMLHttpRequest timeout'), {
          source: 'XMLHttpRequest',
          details: JSON.stringify({
            url: requestData.url,
            method: requestData.method,
            timeout: xhr.timeout
          }, null, 2)
        });
      });
      
      return self.originalXhrSend.apply(this, arguments);
    };
  }
}

// Create a singleton instance
const errorCatcher = new ErrorCatcher();

export default errorCatcher; 