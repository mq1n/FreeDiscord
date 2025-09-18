import { invoke } from '@tauri-apps/api/core';

export const LogLevel = {
  DEBUG: 'Debug',
  INFO: 'Info',
  WARNING: 'Warning',
  ERROR: 'Error',
  CRITICAL: 'Critical'
};

const defaultConfig = {
  consoleEnabled: true,
  notificationsEnabled: true,
  fileLoggingEnabled: true,
  notificationThreshold: LogLevel.WARNING,
  consoleThreshold: LogLevel.DEBUG,
  appNotifications: true,
  appNotificationThreshold: LogLevel.WARNING,
  overrideConsole: true,
  maxEntries: 100,
  uiLogsEnabled: true,
  uiThreshold: LogLevel.INFO
};

class Logger {
  constructor() {
    this.listeners = [];
    this.config = {
      consoleEnabled: true,
      consoleThreshold: LogLevel.DEBUG,
      notificationsEnabled: true,
      notificationThreshold: LogLevel.ERROR
    };
    this.logBuffer = [];
    this.flushTimeout = null;
    this.flushInterval = 300;
    this.batchSize = 10;
    this.flushing = false;
    this.pendingEntries = [];
    this.appNotificationHandler = null;
    this._isLogging = false;
    this.logs = [];
    this.tauriAvailable = null;
    this._noTauriWarned = false;
  }

  // Initialize the logger
  async init(customConfig = {}) {
    // Merge default config with existing and custom config
    this.config = { ...defaultConfig, ...this.config, ...customConfig };
    
    // Install console overrides unless explicitly disabled
    if (this.config.overrideConsole !== false) {
      this.installConsoleOverrides();
    }
    
    // Update file logging status on the Rust side
    try {
      const isEnabled = this.config.fileLoggingEnabled !== false;
      await invoke('set_logging_enabled', { enabled: isEnabled });
      this.tauriAvailable = true;
      console.log('Logger initialized');
    } catch (error) {
      console.error('Failed to initialize logger:', error);
      this.tauriAvailable = false;
      // Disable backend/file logging gracefully when Tauri is not available
      this.config.fileLoggingEnabled = false;
    }
  }

  // Set the app notification handler
  setAppNotificationHandler(handler) {
    this.appNotificationHandler = handler;
    return this; // For chaining
  }

  // Add event listener
  addEventListener(callback) {
    this.listeners.push(callback);
    return () => {
      this.listeners = this.listeners.filter(listener => listener !== callback);
    };
  }

  // Helper to check if a level meets a threshold
  _meetsThreshold(level, threshold) {
    const levelPriority = {
      [LogLevel.DEBUG]: 0,
      [LogLevel.INFO]: 1,
      [LogLevel.WARNING]: 2,
      [LogLevel.ERROR]: 3,
      [LogLevel.CRITICAL]: 4,
    };
    
    return levelPriority[level] >= levelPriority[threshold];
  }

  // Get notification type from log level
  _getNotificationType(level) {
    switch (level) {
      case LogLevel.DEBUG:
      case LogLevel.INFO:
        return 'info';
      case LogLevel.WARNING:
        return 'warning';
      case LogLevel.ERROR:
      case LogLevel.CRITICAL:
        return 'error';
      default:
        return 'info';
    }
  }

  // Format a timestamp for notification display
  _formatTime() {
    const now = new Date();
    return `${now.getHours().toString().padStart(2, '0')}:${now.getMinutes().toString().padStart(2, '0')}`;
  }

  // Format a message for notification display
  _formatMessageForNotification(message, details) {
    // Truncate long messages to a reasonable length for notifications
    const maxLength = 120;
    let fullMessage;
    if (details === undefined || details === null) {
      fullMessage = message;
    } else if (typeof details === 'string') {
      fullMessage = `${message}: ${details}`;
    } else {
      try {
        fullMessage = `${message}: ${JSON.stringify(details)}`;
      } catch (e) {
        try {
          fullMessage = `${message}: ${String(details)}`;
        } catch {
          fullMessage = message;
        }
      }
    }
    
    if (fullMessage.length > maxLength) {
      fullMessage = fullMessage.substring(0, maxLength) + '...';
    }
    
    return fullMessage;
  }

  // Add a new internal logging function that bypasses console.log
  _internalLog(levelName, message, source, details = null) {
    // Convert level name to enum value
    const levelMap = {
      'debug': LogLevel.DEBUG,
      'info': LogLevel.INFO,
      'warning': LogLevel.WARNING,
      'error': LogLevel.ERROR,
      'critical': LogLevel.CRITICAL
    };
    
    const level = levelMap[levelName] || LogLevel.INFO;
    
    // Create log entry
    const entry = {
      timestamp: new Date().toISOString(),
      level: levelName,
      message,
      source,
      details,
    };

    // Add to log storage
    this.logs.push(entry);
    if (this.logs.length > this.config.maxEntries) {
      this.logs.shift();
    }

    // Notify listeners without using console.log
    if (this.listeners.onLogAdded) {
      this.listeners.onLogAdded(entry);
    }

    // Add to batch entries (will be sent to Rust backend)
    this.pendingEntries.push(entry);
    this._scheduleBatchSend();
    
    // Create formatted message for UI display
    const formattedMessage = `[${this._formatTime()}] ${message}`;
    
    // Add to UI logs
    if (this.config.uiLogsEnabled && this._meetsThreshold(level, this.config.uiThreshold)) {
      if (this.appNotificationHandler) {
        this.appNotificationHandler(formattedMessage);
      }
    }
  }

  // Log a message
  async log(level, message, source, details = null) {
    // Create log entry
    const entry = {
      timestamp: new Date().toISOString(),
      level,
      message,
      source,
      details,
    };

    // Console logging (ensure details are stringified to avoid [object Object])
    if (this.config.consoleEnabled && this._meetsThreshold(level, this.config.consoleThreshold)) {
      const consoleMethod = {
        [LogLevel.DEBUG]: console.debug,
        [LogLevel.INFO]: console.info,
        [LogLevel.WARNING]: console.warn,
        [LogLevel.ERROR]: console.error,
        [LogLevel.CRITICAL]: console.error,
      }[level] || console.log;

      let detailsStr = '';
      if (details !== undefined && details !== null) {
        if (typeof details === 'string') {
          detailsStr = `: ${details}`;
        } else {
          try {
            detailsStr = `: ${JSON.stringify(details)}`;
          } catch (e) {
            try {
              detailsStr = `: ${String(details)}`;
            } catch {
              detailsStr = '';
            }
          }
        }
      }

      consoleMethod(`[${entry.timestamp}] [${level}] [${source}] ${message}${detailsStr}`);
    }

    // Notify listeners
    this.listeners.forEach(listener => listener(entry));

    // Handle desktop notifications
    if (this.config.notificationsEnabled && this._meetsThreshold(level, this.config.notificationThreshold)) {
      // Check if the Notification API is available (web environment)
      if (typeof window !== 'undefined' && 'Notification' in window) {
        // Request permission if needed
        if (Notification.permission !== 'granted' && Notification.permission !== 'denied') {
          await Notification.requestPermission();
        }
        
        if (Notification.permission === 'granted') {
          new Notification(`FreeDiscord - ${level}`, {
            body: `${source}: ${message}`,
            icon: '/icon.png' // Update with your app icon path
          });
        }
      }
    }

    // Send to app notification system (toast-style notifications)
    if (this.config.appNotifications && 
        this._meetsThreshold(level, this.config.appNotificationThreshold) && 
        this.appNotificationHandler) {
          try {
            this.appNotificationHandler({
              id: Date.now(), // Unique ID using timestamp
              title: `${level}: ${source}`,
              message: this._formatMessageForNotification(message, details),
              time: this._formatTime(),
              read: false,
              type: this._getNotificationType(level)
            });
          } catch (error) {
            console.error('Failed to create app notification:', error);
          }
    }

    // Also feed UI log list independently of notifications, but avoid duplicates
    if (this.config.uiLogsEnabled && this.appNotificationHandler && this._meetsThreshold(level, this.config.uiThreshold)) {
      // If console overrides are active and console logging will happen, the UI feed
      // will already occur via _internalLog() in the console override, so skip here.
      const willBeHandledByConsole = (this.config.overrideConsole !== false) && this.config.consoleEnabled && this._meetsThreshold(level, this.config.consoleThreshold);
      if (!willBeHandledByConsole) {
        try {
          let detailsStr = '';
          if (details !== undefined && details !== null) {
            if (typeof details === 'string') {
              detailsStr = `: ${details}`;
            } else {
              try {
                detailsStr = `: ${JSON.stringify(details)}`;
              } catch (e) {
                try { detailsStr = `: ${String(details)}`; } catch { detailsStr = ''; }
              }
            }
          }
          const uiMessage = `[${this._formatTime()}] ${message}${detailsStr}`;
          this.appNotificationHandler(uiMessage);
        } catch (e) {
          // Don't let UI log formatting failures break app
        }
      }
    }

    // Buffer logs for batch processing
    if (this.config.fileLoggingEnabled !== false) {
      this._bufferLogEntry(level, message, source, details);
    }

    return entry;
  }

  // Add a log entry to the buffer and schedule flush if needed
  _bufferLogEntry(level, message, source, details) {
    let logMessage = message;
    
    // Format details if present (same logic as before)
    if (details) {
      if (typeof details === 'object') {
        try {
          const detailsStr = JSON.stringify(details, null, 2);
          logMessage = `${message}: ${detailsStr}`;
        } catch (err) {
          logMessage = `${message}: [Object details could not be stringified]`;
        }
      } else {
        logMessage = `${message}: ${details}`;
      }
    }

    // Add to buffer
    this.logBuffer.push({
      level,
      message: logMessage,
      source,
      details: null // We already merged details into the message
    });

    // Schedule a flush if not already scheduled
    if (!this.flushTimeout) {
      this.flushTimeout = setTimeout(() => this._flushLogs(), this.flushInterval);
    }

    // Flush immediately if buffer is full
    if (this.logBuffer.length >= this.batchSize) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
      this._flushLogs();
    }
  }

  // Flush logs to the backend
  async _flushLogs() {
    // Clear the timeout
    if (this.flushTimeout) {
      clearTimeout(this.flushTimeout);
      this.flushTimeout = null;
    }

    // If already flushing, skip this run
    if (this.flushing) {
      return;
    }

    // If no logs, nothing to do
    if (this.logBuffer.length === 0) {
      return;
    }

    // Set flushing flag to prevent concurrent flushes
    this.flushing = true;

    // Take the current buffer and clear it (but we'll requeue on failure)
    const entriesToFlush = [...this.logBuffer];
    this.logBuffer = [];

    try {
      // If we previously detected Tauri is not available, skip quietly and requeue
      if (this.tauriAvailable === false) {
        // Silently skip backend flushes when unavailable to avoid spam
        // Drop entries instead of requeueing to prevent infinite retry loops in dev/browser
        return;
      }
      
      // For critical and error logs, send them immediately without batching
      const criticalEntries = entriesToFlush.filter(entry => 
        entry.level === LogLevel.CRITICAL || entry.level === LogLevel.ERROR
      );

      const normalEntries = entriesToFlush.filter(entry => 
        entry.level !== LogLevel.CRITICAL && entry.level !== LogLevel.ERROR
      );

      // Process critical entries individually
      for (const entry of criticalEntries) {
        await invoke('log_from_js', entry);
      }

      // For batches of non-critical logs, use the batch API
      if (normalEntries.length > 0) {
        await invoke('batch_log_from_js', { entries: normalEntries });
      }

      // Successful flush means Tauri is available
      this.tauriAvailable = true;
    } catch (error) {
      console.error('Failed to flush logs:', error);
      // Mark as not available if the error indicates missing Tauri bridge
      this.tauriAvailable = false;
      // Put the entries back at the front of the buffer to retry later
      this.logBuffer = [...entriesToFlush, ...this.logBuffer];
    } finally {
      this.flushing = false;
      
      // If more logs arrived while we were flushing, schedule another flush
      if (this.logBuffer.length > 0) {
        this.flushTimeout = setTimeout(() => this._flushLogs(), this.flushInterval);
      }
    }
  }

  // Convenience methods for different log levels
  async debug(message, source, details = null) {
    return this.log(LogLevel.DEBUG, message, source, details);
  }
  
  async info(message, source, details = null) {
    return this.log(LogLevel.INFO, message, source, details);
  }
  
  async warning(message, source, details = null) {
    return this.log(LogLevel.WARNING, message, source, details);
  }
  
  async error(message, source, details = null) {
    return this.log(LogLevel.ERROR, message, source, details);
  }
  
  async critical(message, source, details = null) {
    return this.log(LogLevel.CRITICAL, message, source, details);
  }
  
  // Retrieve recent logs from file
  async getRecentLogs(count = 50) {
    try {
      return await invoke('get_recent_logs', { count });
    } catch (error) {
      console.error('Failed to get recent logs:', error);
      return [];
    }
  }
  
  // Get the log file path
  async getLogFilePath() {
    try {
      return await invoke('get_log_file_path');
    } catch (error) {
      console.error('Failed to get log file path:', error);
      return null;
    }
  }
  
  // Set configuration options
  async setConfig(newConfig) {
    const oldConfig = { ...this.config };
    this.config = { ...this.config, ...newConfig };
    
    // If file logging status changed, update on Rust side
    if (oldConfig.fileLoggingEnabled !== this.config.fileLoggingEnabled) {
      try {
        await invoke('set_logging_enabled', { enabled: this.config.fileLoggingEnabled });
      } catch (error) {
        console.error('Failed to update logging status:', error);
      }
    }
  }

  // Add method to force flush logs (useful before app exits)
  async forceFlushLogs() {
    await this._flushLogs();
  }

  /**
   * Export logs as a text string for bug reports
   * @param {number} count - Max number of log entries to export (default: 200)
   * @returns {Promise<string>} - Log content as a string
   */
  async exportLogs(count = 200) {
    try {
      // Try to get logs from file first if we're running in Tauri
      if (typeof invoke === 'function' && this.config.fileLoggingEnabled) {
        try {
          // Force flush any pending logs first
          await this.forceFlushLogs();
          
          // Get log file path
          const logPath = await this.getLogFilePath();
          
          // Check if the log file exists and read its content
          // Use the commands from the bug_report module
          const { exists } = await invoke('bug_report_check_file_exists', { path: logPath });
          
          if (exists) {
            const content = await invoke('bug_report_read_log_file', { path: logPath, maxLines: count });
            return content;
          }
        } catch (error) {
          console.error('Failed to read log file:', error);
          // Fall back to in-memory logs
        }
      }
      
      // If we couldn't read from file or we're in the browser, use in-memory logs
      const logs = await this.getRecentLogs(count);
      
      // Format logs as text
      return logs.map(log => {
        const timestamp = new Date(log.timestamp).toISOString();
        const source = log.source ? `[${log.source}]` : '';
        const details = log.details ? `\n  Details: ${JSON.stringify(log.details)}` : '';
        
        return `${timestamp} ${log.level.toUpperCase()} ${source} - ${log.message}${details}`;
      }).join('\n');
    } catch (error) {
      console.error('Error exporting logs:', error);
      return `Error exporting logs: ${error.message}`;
    }
  }

  /**
   * Replaces standard console methods with logger equivalents
   * Call this early in your application to ensure all console logs go through the logger
   */
  installConsoleOverrides() {
    if (typeof window === 'undefined') {
      return; // Skip in non-browser environments
    }

    const self = this;
    
    // Store original methods
    this.originalConsole = {
      log: console.log,
      debug: console.debug,
      info: console.info,
      warn: console.warn,
      error: console.error
    };
    
    // Override console.log
    console.log = function() {
      // Call original for development visibility
      self.originalConsole.log.apply(console, arguments);
      
      // Prevent recursive logging - we'll check if we're already in a logging operation
      if (self._isLogging) {
        return;
      }
      
      // Use setTimeout to avoid blocking the main thread
      setTimeout(() => {
        try {
          // Set logging flag to prevent recursion
          self._isLogging = true;
          
          // Convert arguments to string
          const args = Array.from(arguments);
          const message = args.map(arg => {
            if (typeof arg === 'object') {
              try {
                return JSON.stringify(arg);
              } catch (e) {
                return String(arg);
              }
            }
            return String(arg);
          }).join(' ');
          
          // Only log if message is not empty and not from Discord iframe
          if (message && message.trim().length > 0 && !message.includes('[console.info]')) {
            // Direct call to internal log function to avoid recursion
            self._internalLog('info', message, 'console.log', null);
          }
        } catch (e) {
          // Prevent any errors in logging from affecting the application
        } finally {
          // Reset logging flag
          self._isLogging = false;
        }
      }, 0);
    };
    
    // Override console.debug
    console.debug = function() {
      self.originalConsole.debug.apply(console, arguments);
      
      // Prevent recursive logging
      if (self._isLogging) {
        return;
      }
      
      // Use setTimeout to avoid blocking the main thread
      setTimeout(() => {
        try {
          // Set logging flag to prevent recursion
          self._isLogging = true;
          
          const args = Array.from(arguments);
          const message = args.map(arg => {
            if (typeof arg === 'object') {
              try {
                return JSON.stringify(arg);
              } catch (e) {
                return String(arg);
              }
            }
            return String(arg);
          }).join(' ');
          
          // Only log non-empty messages and filter out recursive logs
          if (message && message.trim().length > 0 && !message.includes('[console.')) {
            self._internalLog('debug', message, 'console.debug', null);
          }
        } catch (e) {
          // Prevent any errors in logging from affecting the application
        } finally {
          // Reset logging flag
          self._isLogging = false;
        }
      }, 0);
    };
    
    // Override console.info
    console.info = function() {
      self.originalConsole.info.apply(console, arguments);
      
      // Prevent recursive logging
      if (self._isLogging) {
        return;
      }
      
      // Use setTimeout to avoid blocking the main thread
      setTimeout(() => {
        try {
          // Set logging flag to prevent recursion
          self._isLogging = true;
          
          const args = Array.from(arguments);
          const message = args.map(arg => {
            if (typeof arg === 'object') {
              try {
                return JSON.stringify(arg);
              } catch (e) {
                return String(arg);
              }
            }
            return String(arg);
          }).join(' ');
          
          // Only log non-empty messages and filter out recursive logs
          if (message && message.trim().length > 0 && !message.includes('[console.')) {
            self._internalLog('info', message, 'console.info', null);
          }
        } catch (e) {
          // Prevent any errors in logging from affecting the application
        } finally {
          // Reset logging flag
          self._isLogging = false;
        }
      }, 0);
    };
    
    // Override console.warn
    console.warn = function() {
      self.originalConsole.warn.apply(console, arguments);
      
      // Prevent recursive logging
      if (self._isLogging) {
        return;
      }
      
      // Use setTimeout to avoid blocking the main thread
      setTimeout(() => {
        try {
          // Set logging flag to prevent recursion
          self._isLogging = true;
          
          const args = Array.from(arguments);
          const message = args.map(arg => {
            if (typeof arg === 'object') {
              try {
                return JSON.stringify(arg);
              } catch (e) {
                return String(arg);
              }
            }
            return String(arg);
          }).join(' ');
          
          // Only log non-empty messages and filter out recursive logs
          if (message && message.trim().length > 0 && !message.includes('[console.')) {
            self._internalLog('warning', message, 'console.warn', null);
          }
        } catch (e) {
          // Prevent any errors in logging from affecting the application
        } finally {
          // Reset logging flag
          self._isLogging = false;
        }
      }, 0);
    };
    
    // Override console.error
    console.error = function() {
      self.originalConsole.error.apply(console, arguments);
      
      // Prevent recursive logging
      if (self._isLogging) {
        return;
      }
      
      // Use setTimeout to avoid blocking the main thread
      setTimeout(() => {
        try {
          // Set logging flag to prevent recursion
          self._isLogging = true;
          
          const args = Array.from(arguments);
          const message = args.map(arg => {
            if (typeof arg === 'object') {
              try {
                return JSON.stringify(arg);
              } catch (e) {
                return String(arg);
              }
            }
            return String(arg);
          }).join(' ');
          
          // Only log non-empty messages and filter out recursive logs
          if (message && message.trim().length > 0 && !message.includes('[console.')) {
            self._internalLog('error', message, 'console.error', null);
          }
        } catch (e) {
          // Prevent any errors in logging from affecting the application
        } finally {
          // Reset logging flag
          self._isLogging = false;
        }
      }, 0);
    };
  }
  
  /**
   * Restores original console methods
   */
  restoreConsole() {
    if (this.originalConsole) {
      Object.keys(this.originalConsole).forEach(key => {
        console[key] = this.originalConsole[key];
      });
    }
  }
}

// Create and export a singleton instance
const logger = new Logger();
export default logger; 