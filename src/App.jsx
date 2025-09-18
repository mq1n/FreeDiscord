import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import logger from './utils/logger';
import './App.css';

let __fd_unsubscribers = [];
if (typeof window !== 'undefined') {
  if (!('___fd_listenersBound' in window)) {
    window.___fd_listenersBound = false;
  }
  if (!window.__fd_beforeUnloadHookInstalled) {
    window.addEventListener('beforeunload', () => {
      try { __fd_unsubscribers.forEach((u) => { try { u(); } catch {} }); } catch {}
      __fd_unsubscribers = [];
      window.___fd_listenersBound = false;
    });
    window.__fd_beforeUnloadHookInstalled = true;
  }
}

function App() {
  const [proxies, setProxies] = useState([]);
  const [filteredProxies, setFilteredProxies] = useState([]);
  const [activeProxy, setActiveProxy] = useState(null);
  const [proxyStatus, setProxyStatus] = useState('Stopped');
  const [message, setMessage] = useState('');
  const [searchTerm, setSearchTerm] = useState('');
  const [countryFilter, setCountryFilter] = useState('');
  const [proxyTypeFilter, setProxyTypeFilter] = useState('');
  const [onlyWorking, setOnlyWorking] = useState(false);
  const [logMessages, setLogMessages] = useState([]);
  const [detailedLogs, setDetailedLogs] = useState([]);
  const detailedLogContainerRef = useRef(null);
  const [autoScrollDetailedLogs, setAutoScrollDetailedLogs] = useState(true);
  const [currentPage, setCurrentPage] = useState(0);
  const [totalPages, setTotalPages] = useState(0);
  const pageSize = 50;
  const [testingStatus, setTestingStatus] = useState({ count: 0, active: false });
  const [cacheStatus, setCacheStatus] = useState('');
  const [discordLoadFailed, setDiscordLoadFailed] = useState(false);
  const proxiesUpdatedTimer = useRef(null);
  const logContainerRef = useRef(null);
  const [autoScrollLogs, setAutoScrollLogs] = useState(true);
  const lastLogRef = useRef({ text: null, ts: 0 });

  const [testingProxies, setTestingProxies] = useState(new Set());
  const [startingProxies, setStartingProxies] = useState(new Set());
  const [isTestingDetailed, setIsTestingDetailed] = useState(false);
  const isTestingDetailedRef = useRef(false);
  useEffect(() => { isTestingDetailedRef.current = isTestingDetailed; }, [isTestingDetailed]);
  const [showAddCustomModal, setShowAddCustomModal] = useState(false);
  
  const normalizeLogEntry = (entry) => {
    try {
      let text = '';
      let ts = null;
      if (typeof entry === 'string') {
        // Try to extract leading [HH:MM] if present
        const m = /^\[(\d{2}):(\d{2})\]\s*(.*)$/.exec(entry);
        if (m) {
          const now = new Date();
          const yyyy = now.getFullYear();
          const mm = String(now.getMonth() + 1).padStart(2, '0');
          const dd = String(now.getDate()).padStart(2, '0');
          ts = new Date(`${yyyy}-${mm}-${dd}T${m[1]}:${m[2]}:00`).toISOString();
          text = m[3];
        } else {
          // Try to extract leading locale date like 18/09/2025, 11:50:41
          const m2 = /^(\d{2})\/(\d{2})\/(\d{4}),\s*(\d{2}):(\d{2}):(\d{2})(.*)$/.exec(entry);
          if (m2) {
            const [_, d, mo, y, h, mi, s, rest] = m2;
            const isoLocal = new Date(`${y}-${mo}-${d}T${h}:${mi}:${s}`);
            ts = isNaN(isoLocal.getTime()) ? null : isoLocal.toISOString();
            text = (rest || '').trim();
          } else {
            text = entry;
          }
        }
      } else if (entry && typeof entry === 'object') {
        // Prefer explicit fields; else pretty-print the object
        const candidate = entry.text ?? entry.message ?? entry.msg ?? entry.data ?? null;
        if (candidate != null) {
          if (typeof candidate === 'string') {
            text = candidate;
          } else {
            try { text = JSON.stringify(candidate, null, 2); } catch { text = String(candidate); }
          }
        } else {
          try { text = JSON.stringify(entry, null, 2); } catch { text = String(entry); }
        }
        ts = entry.timestamp || entry.time || entry.ts || null;
        if (ts) {
          try { ts = new Date(ts).toISOString(); } catch { ts = null; }
        }
      } else {
        text = String(entry);
      }
      // Ensure non-string text is stringified
      if (text && typeof text !== 'string') {
        try { text = JSON.stringify(text, null, 2); } catch { text = String(text); }
      }
      // Drop meaningless object placeholder or empty lines
      if (typeof text === 'string' && (text.trim() === '' || text.trim() === '[object Object]')) {
        return null;
      }
      const timestamp = ts || new Date().toISOString();
      return { timestamp, text };
    } catch {
      return { timestamp: new Date().toISOString(), text: String(entry) };
    }
  };

  const addLog = (entry) => {
    const normalized = normalizeLogEntry(entry);
    if (!normalized) return;
    // Skip duplicates within a short window (e.g., caused by double event listeners in dev)
    try {
      const now = Date.now();
      const text = normalized.text;
      const last = lastLogRef.current;
      if (last && last.text === text && now - (last.ts || 0) < 1200) {
        return;
      }
      lastLogRef.current = { text, ts: now };
    } catch {}
    if (isTestingDetailedRef.current) {
      setDetailedLogs((prev) => {
        const next = [...prev, normalized];
        return next.length > 500 ? next.slice(-500) : next;
      });
    } else {
      setLogMessages((prev) => {
        const next = [...prev, normalized];
        return next.length > 500 ? next.slice(-500) : next;
      });
    }
  };

  const addLogs = (entries) => {
    const toAddRaw = entries.map(normalizeLogEntry).filter(Boolean);
    // Apply dedupe across the incoming batch too
    const toAdd = [];
    const now = Date.now();
    const last = lastLogRef.current;
    for (const item of toAddRaw) {
      if (last && last.text === item.text && now - (last.ts || 0) < 1200) {
        continue;
      }
      lastLogRef.current = { text: item.text, ts: now };
      toAdd.push(item);
    }
    if (toAdd.length === 0) return;
    if (isTestingDetailedRef.current) {
      setDetailedLogs((prev) => {
        const next = [...prev, ...toAdd];
        return next.length > 500 ? next.slice(-500) : next;
      });
    } else {
      setLogMessages((prev) => {
        const next = [...prev, ...toAdd];
        return next.length > 500 ? next.slice(-500) : next;
      });
    }
  };

  // Auto-scroll logs
  useEffect(() => {
    if (!autoScrollLogs) return;
    const el = logContainerRef.current;
    if (!el) return;
    // Scroll on next frame to ensure DOM updated
    const id = requestAnimationFrame(() => {
      try { el.scrollTop = el.scrollHeight; } catch {}
    });
    return () => cancelAnimationFrame(id);
  }, [logMessages, autoScrollLogs]);

  // Auto-scroll detailed modal logs
  useEffect(() => {
    if (!autoScrollDetailedLogs) return;
    const el = detailedLogContainerRef.current;
    if (!el) return;
    const id = requestAnimationFrame(() => {
      try { el.scrollTop = el.scrollHeight; } catch {}
    });
    return () => cancelAnimationFrame(id);
  }, [detailedLogs, autoScrollDetailedLogs]);

  const formatDateTime = (iso) => {
    try {
      const d = new Date(iso);
      return d.toLocaleString(undefined, {
        year: 'numeric', month: '2-digit', day: '2-digit',
        hour: '2-digit', minute: '2-digit', second: '2-digit',
        hour12: false
      });
    } catch {
      return '';
    }
  };
  
  // Convert any error-like value into a string
  const toErrorString = (error) => {
    if (!error) return 'Unknown error';
    if (typeof error === 'string') return error;
    if (error && typeof error.message === 'string') return error.message;
    try { return JSON.stringify(error); } catch { return String(error); }
  };
  
  // Safe read for nested activeProxy fields
  const safeGetActiveProxyInfo = (field) => {
    try {
      return activeProxy && activeProxy[field] ? activeProxy[field] : 'Unknown';
    } catch (error) {
      logger.error(`Error accessing activeProxy.${field}`, 'App', toErrorString(error));
      return 'Error';
    }
  };
  
  // View state and proxy origin for direct navigation
  const [currentView, setCurrentView] = useState('proxy-manager'); // 'proxy-manager' or 'discord'
  const [controllerExpanded, setControllerExpanded] = useState(true);
  const [discordUrl, setDiscordUrl] = useState('');
  const [proxyOrigin, setProxyOrigin] = useState(''); // dynamic proxy base like http://localhost:<port>

  // Compute proxy base origin from state/URL
  const getProxyBase = () => {
    try {
      if (proxyOrigin) return proxyOrigin;
      if (discordUrl) return new URL(discordUrl).origin;
    } catch {}
    return '';
  };

  // Ask backend for current proxy origin (dynamic port)
  const fetchProxyBase = async () => {
    try {
      const url = await invoke('get_proxy_base_url');
      if (typeof url === 'string' && url.startsWith('http')) {
        setProxyOrigin(url);
        return url;
      }
    } catch (e) {
      logger.warning('Could not get proxy base URL from backend', 'App', toErrorString(e));
    }
    return '';
  };

  // Custom proxy input state
  const [customProxyIp, setCustomProxyIp] = useState('');
  const [customProxyPort, setCustomProxyPort] = useState('');
  const [customProxyType, setCustomProxyType] = useState('http');
  const [customProxyUser, setCustomProxyUser] = useState('');
  const [customProxyPass, setCustomProxyPass] = useState('');
  const [showPassword, setShowPassword] = useState(false);
  const [isAddingCustomProxy, setIsAddingCustomProxy] = useState(false);

  // One-time bootstrap: listeners, cache restore, status polling
  useEffect(() => {
    // Setup listeners first, then perform initial fetches to avoid race conditions
    let cleanupFunction = null;
    let interval = null;

    (async () => {
  // Pipe logger notifications into UI logs
      try {
        logger.setAppNotificationHandler((entry) => {
          // We only add raw string UI feed entries here.
          // Object-shaped entries are toast notifications with truncated message ("...").
          if (typeof entry === 'string') {
            addLog(entry);
          }
        });
      } catch {}
  // Subscribe to backend events
      const setupListeners = async () => {
        // If we've already bound listeners (e.g., StrictMode re-mount), skip re-binding
        if (typeof window !== 'undefined' && window.___fd_listenersBound) {
          return () => {};
        }
  // Log messages
        const logUnlisten = await listen('proxy-log', (event) => {
          addLog(event.payload);
        });
        __fd_unsubscribers.push(logUnlisten);

  // Proxies updated -> re-fetch current page (debounced)
        const proxyUpdateUnlisten = await listen('proxies-updated', () => {
          if (proxiesUpdatedTimer.current) clearTimeout(proxiesUpdatedTimer.current);
          proxiesUpdatedTimer.current = setTimeout(() => {
            getPage(currentPage, { silent: true });
          }, 400);
        });
        __fd_unsubscribers.push(proxyUpdateUnlisten);

  // Testing status (count, active)
        const testingStatusUnlisten = await listen('proxy-testing-status', (event) => {
          setTestingStatus({ count: event.payload[0], active: event.payload[1] });
        });
        __fd_unsubscribers.push(testingStatusUnlisten);

  // Backend navigation -> switch to Discord view
        const navigateUnlisten = await listen('navigate-main-window', (event) => {
          setDiscordUrl(event.payload);
          try { setProxyOrigin(new URL(event.payload).origin); } catch {}
          setCurrentView('discord');
        });
        __fd_unsubscribers.push(navigateUnlisten);

  // Switch back to manager view
        const returnToProxyUnlisten = await listen('return-to-proxy-manager', () => {
          setCurrentView('proxy-manager');
        });
        __fd_unsubscribers.push(returnToProxyUnlisten);

  // Update list with background-tested proxy
        const proxyTestedUnlisten = await listen('proxy-tested', (event) => {
          try {
            const tested = event.payload;
            setProxies(prev => {
              const idx = prev.findIndex(p => p.ip === tested.ip && p.port === tested.port);
              if (idx >= 0) {
                const next = [...prev];
                next[idx] = tested;
                return next;
              }
              return [tested, ...prev];
            });

            setActiveProxy(prev => (prev && prev.ip === tested.ip && prev.port === tested.port) ? tested : prev);
          } catch (e) {
            logger.error('Error handling proxy-tested event', 'App', e?.message || String(e));
          }
        });
        __fd_unsubscribers.push(proxyTestedUnlisten);

        if (typeof window !== 'undefined') {
          window.___fd_listenersBound = true;
        }

  // Event unsubscription
        // We intentionally do not unbind here to avoid StrictMode double-mount duplicates.
        // Cleanup will occur on window beforeunload.
        return () => {};
      };

      try {
        cleanupFunction = await setupListeners();

  // After listeners, restore cache and initial state
        try {
          await loadProxiesFromCache();
        } catch (e) {
          logger.debug('loadProxiesFromCache failed (non-fatal)', 'App', e?.message || String(e));
        }

        try { await checkProxyStatus(); } catch (e) { logger.debug('checkProxyStatus failed', 'App', e?.message || String(e)); }
        try { await getActiveProxy(); } catch (e) { logger.debug('getActiveProxy failed', 'App', e?.message || String(e)); }
        try { await getPage(0); } catch (e) { logger.debug('getPage(0) failed', 'App', e?.message || String(e)); }

  // Poll status every 5s
        interval = setInterval(() => {
          try { checkProxyStatus(); } catch (e) { logger.debug('checkProxyStatus poll failed', 'App', e?.message || String(e)); }
          try { getActiveProxy(); } catch (e) { logger.debug('getActiveProxy poll failed', 'App', e?.message || String(e)); }
        }, 5000);
      } catch (error) {
        logger.error('Error during startup initialization', 'App', error?.message || String(error));
      }
    })();

    // Cleanup on unmount
    return () => {
      if (interval) clearInterval(interval);
      if (cleanupFunction) cleanupFunction();
    };
  }, []);

  // Fetch current page when page index changes
  useEffect(() => {
    getPage(currentPage);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentPage]);

  useEffect(() => {
  // Apply UI filters
    let filtered = [...proxies];
    
    if (searchTerm) {
      filtered = filtered.filter(proxy => 
        proxy.ip.includes(searchTerm) || 
        proxy.city.toLowerCase().includes(searchTerm.toLowerCase()) ||
        proxy.country.toLowerCase().includes(searchTerm.toLowerCase())
      );
    }
    
    if (countryFilter) {
      filtered = filtered.filter(proxy => 
        proxy.country.toLowerCase().includes(countryFilter.toLowerCase())
      );
    }
    
    if (proxyTypeFilter) {
      filtered = filtered.filter(proxy => 
        proxy.proxy_type === proxyTypeFilter
      );
    }
    
    if (onlyWorking) {
      filtered = filtered.filter(proxy => proxy.is_working);
    }
    
    setFilteredProxies(filtered);
  }, [proxies, searchTerm, countryFilter, proxyTypeFilter, onlyWorking]);

  // Toggle body class for Discord view
  useEffect(() => {
    if (currentView === 'discord') {
      document.body.classList.add('discord-mode');
    } else {
      document.body.classList.remove('discord-mode');
    }
    
    return () => {
      document.body.classList.remove('discord-mode');
    };
  }, [currentView]);

  async function checkProxyStatus() {
    try {
      const status = await invoke('get_proxy_status');
      setProxyStatus(status ? 'Running' : 'Stopped');
    } catch (error) {
      logger.error('Error checking proxy status', 'ProxyStatus', toErrorString(error));
    }
  }

  async function getActiveProxy() {
    try {
      const proxy = await invoke('get_active_proxy');
      setActiveProxy(proxy);
    } catch (error) {
      logger.error('Error getting active proxy', 'ActiveProxy', toErrorString(error));
    }
  }
  
  async function getPage(page, { silent = false } = {}) {
    try {
      if (!silent) setMessage(`Loading page ${page + 1}...`);
      
      const result = await invoke('get_proxies_paginated', { 
        page,
        pageSize
      });
      
      setProxies(result.proxies);
      setTotalPages(result.total_pages);
      setCurrentPage(result.current_page);
      
      // Clear the message after loading
      if (!silent) setMessage('');
    } catch (error) {
      const msg = toErrorString(error);
      logger.error('Error getting paginated proxies', 'Pagination', msg);
      setMessage(`Error loading page: ${msg}`);
    }
  }

  // Step-based pipeline
  const [stepLoading, setStepLoading] = useState({ fetch:false, parse:false, test:false });
  // Debounce pipeline steps to prevent double calls
  const debounceRef = useRef({ fetch: false, parse: false, test: false });
  const runStepFetch = async () => {
    if (debounceRef.current.fetch) return;
    debounceRef.current.fetch = true;
    try {
      setStepLoading(s => ({...s, fetch:true}));
      setMessage('Step 1: Fetching sources…');
      // Clear logs once (avoid duplicate clears when HMR/StrictMode)
      setLogMessages((prev) => (prev.length ? [] : prev));
      await invoke('pipeline_reset');
      const res = await invoke('pipeline_fetch_sources');
      addLog(`[Pipeline] ${res}`);
      setMessage('Step 1 complete. Now run Step 2 (Parse).');
    } catch (e) {
      const msg = toErrorString(e);
      addLog(`[Pipeline] Step 1 failed: ${msg}`);
      setMessage(`Step 1 failed: ${msg}`);
    } finally {
      setStepLoading(s => ({...s, fetch:false}));
      debounceRef.current.fetch = false;
    }
  };

  const runStepParse = async () => {
    if (debounceRef.current.parse) return;
    debounceRef.current.parse = true;
    try {
      setStepLoading(s => ({...s, parse:true}));
      setMessage('Step 2: Parsing fetched results…');
      const total = await invoke('pipeline_parse_fetched');
      addLog(`[Pipeline] Parsed proxies: ${total}`);
      setMessage(`Parsed ${total} proxies. Now run Step 3 (Test).`);
      await getPage(0, { silent: true });
    } catch (e) {
      const msg = toErrorString(e);
      addLog(`[Pipeline] Step 2 failed: ${msg}`);
      setMessage(`Step 2 failed: ${msg}`);
    } finally {
      setStepLoading(s => ({...s, parse:false}));
      debounceRef.current.parse = false;
    }
  };

  const runStepTest = async () => {
    if (debounceRef.current.test) return;
    debounceRef.current.test = true;
    try {
      setStepLoading(s => ({...s, test:true}));
      setMessage('Step 3: Testing proxies…');
      await invoke('pipeline_test_all');
      setMessage('Testing complete');
    } catch (e) {
      const msg = toErrorString(e);
      addLog(`[Pipeline] Step 3 failed: ${msg}`);
      setMessage(`Step 3 failed: ${msg}`);
    } finally {
      setStepLoading(s => ({...s, test:false}));
      debounceRef.current.test = false;
    }
  };

  async function testProxy(proxy) {
    const key = `${proxy.ip}:${proxy.port}`;
    setTestingProxies(prev => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
    try {
      const proxyAddr = `${proxy.ip}:${proxy.port}`;
      setMessage(`Testing proxy ${proxyAddr}...`);
      logger.info(`Testing proxy ${proxyAddr}`, 'ProxyTester');
      
      const result = await invoke('test_proxy', { 
        ip: proxy.ip, 
        port: proxy.port 
      });
      
  // Merge result into list
      const newProxies = proxies.map(p => 
        p.ip === proxy.ip && p.port === proxy.port ? result : p
      );
      setProxies(newProxies);
      
      const statusMsg = `Proxy ${proxyAddr} - ${result.is_working ? 'Working' : 'Not working'}`;
      setMessage(statusMsg);
      logger.info(statusMsg, 'ProxyTester', { 
        status: result.is_working ? 'working' : 'not_working', 
        ping: result.ping_ms 
      });
    } catch (error) {
      const errorMsg = `Error testing proxy ${proxy.ip}:${proxy.port}`;
      const msg = toErrorString(error);
      setMessage(`${errorMsg}: ${msg}`);
      logger.error(errorMsg, 'ProxyTester', msg);
    } finally {
      setTestingProxies(prev => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  }

  async function testCustomProxyDetailed() {
    setDetailedLogs([]);
    setIsTestingDetailed(true);
    try {
      if (!customProxyIp || !customProxyPort) {
        setMessage('Please enter both IP and port for testing');
        return;
      }

      setMessage('Running detailed proxy test...');
      // Do not log sensitive info like password
      logger.info(`Testing custom proxy ${customProxyIp}:${customProxyPort} (${customProxyType})`, 'App');

      const result = await invoke('test_custom_proxy_detailed', {
        ip: customProxyIp.trim(),
        port: parseInt(customProxyPort, 10),
        proxyType: customProxyType,
        username: customProxyUser ? customProxyUser : null,
        password: customProxyPass ? customProxyPass : null,
      });

      // Show detailed results
      setMessage('Detailed test completed. Check logs for results.');
      logger.info('Detailed proxy test results:', 'ProxyTester', result);
      
  // Show results in modal logs (mask credentials if echoed)
      const sanitize = (text) => {
        if (!text) return text;
        try {
          let out = String(text);
          if (customProxyUser) {
            const reUser = new RegExp(customProxyUser.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g');
            out = out.replace(reUser, '***');
          }
          if (customProxyPass) {
            const rePass = new RegExp(customProxyPass.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g');
            out = out.replace(rePass, '********');
          }
          // common patterns user:pass@host:port
          out = out.replace(/:\s*([^\s@]{3,})@/g, ':********@');
          return out;
        } catch { return text; }
      };
      const resultLines = result.split('\n').map(sanitize);
      addLogs(resultLines);

    } catch (error) {
      const msg = toErrorString(error);
      setMessage(`Detailed proxy test failed: ${msg}`);
      logger.error('Failed to run detailed proxy test', 'App', msg);
    } finally {
      setIsTestingDetailed(false);
    }
  }

  async function addCustomProxy() {
    try {
      if (!customProxyIp || !customProxyPort) {
        setMessage('Please enter both IP and port for the custom proxy');
        return;
      }

      setIsAddingCustomProxy(true);
      setMessage('Adding custom proxy...');

      const result = await invoke('add_custom_proxy', {
        ip: customProxyIp.trim(),
        port: parseInt(customProxyPort, 10),
        proxyType: customProxyType,
        username: customProxyUser ? customProxyUser : null,
        password: customProxyPass ? customProxyPass : null,
      });

  // Insert or replace in list
      setProxies(prev => {
        const exists = prev.some(p => p.ip === result.ip && p.port === result.port);
        if (exists) {
          return prev.map(p => (p.ip === result.ip && p.port === result.port ? result : p));
        }
        return [result, ...prev];
      });

  setMessage(`Custom proxy added: ${result.ip}:${result.port}`);
  // Clear form and close modal
  setCustomProxyIp('');
  setCustomProxyPort('');
  setCustomProxyUser('');
  setCustomProxyPass('');
  setShowAddCustomModal(false);
    } catch (error) {
      const msg = toErrorString(error);
      setMessage(`Failed to add custom proxy: ${msg}`);
      logger.error('Failed to add custom proxy', 'App', msg);
    } finally {
      setIsAddingCustomProxy(false);
    }
  }

  async function deleteProxy(proxy) {
    try {
      const confirmation = window.confirm(`Are you sure you want to delete proxy ${proxy.ip}:${proxy.port}?`);
      if (!confirmation) return;

      setMessage(`Deleting proxy ${proxy.ip}:${proxy.port}...`);
      logger.info(`Deleting proxy ${proxy.ip}:${proxy.port}`, 'App');

      const result = await invoke('delete_proxy', {
        ip: proxy.ip,
        port: proxy.port
      });

  // Remove from local list
      setProxies(prev => prev.filter(p => !(p.ip === proxy.ip && p.port === proxy.port)));
      
      setMessage(result);
      logger.info('Proxy deleted successfully', 'App', result);

    } catch (error) {
      const msg = toErrorString(error);
      setMessage(`Failed to delete proxy: ${msg}`);
      logger.error('Failed to delete proxy', 'App', msg);
    }
  }

  async function startProxy(proxy) {
    const key = `${proxy.ip}:${proxy.port}`;
    setStartingProxies(prev => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
    try {
      const proxyAddr = `${proxy.ip}:${proxy.port}`;
      setMessage(`Starting proxy ${proxyAddr}...`);
      logger.info(`Starting proxy ${proxyAddr}`, 'ProxyServer');
      addLog(`[Use] Starting proxy ${proxyAddr} (${proxy.proxy_type})`);
      
      const response = await invoke('start_proxy', { 
        ip: proxy.ip, 
        port: proxy.port 
      });
      
      setMessage(response);
      logger.info(`Proxy server response: ${response}`, 'ProxyServer');
  addLog(`[Use] ${response}`);
      checkProxyStatus();
      getActiveProxy();
      
  // Persist working proxies when starting one
      saveProxiesToCache();
    } catch (error) {
      const errorMsg = `Error starting proxy ${proxy.ip}:${proxy.port}`;
      const msg = toErrorString(error);
      setMessage(`${errorMsg}: ${msg}`);
      logger.error(errorMsg, 'ProxyServer', msg);
      addLog(`[Use] Failed to start proxy ${proxy.ip}:${proxy.port} - ${msg}`);
    } finally {
      setStartingProxies(prev => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    }
  }

  async function stopProxy() {
    try {
      setMessage('Stopping proxy...');
      logger.info('Stopping proxy server', 'ProxyServer');
      addLog('[Use] Stopping active proxy');
      
      const response = await invoke('stop_proxy');
      
      setMessage(response);
      logger.info(`Proxy server response: ${response}`, 'ProxyServer');
      addLog(`[Use] ${response}`);
      checkProxyStatus();
      setActiveProxy(null);
    } catch (error) {
      const errorMsg = 'Error stopping proxy';
      const msg = toErrorString(error);
      setMessage(`${errorMsg}: ${msg}`);
      logger.error(errorMsg, 'ProxyServer', msg);
      addLog(`[Use] Failed to stop proxy - ${msg}`);
    }
  }

  const launchDiscord = async () => {
    try {
      if (proxyStatus === 'Stopped') {
        logger.error('Cannot launch Discord', 'App', 'Proxy is not running');
        setMessage('Error: Proxy is not running. Please start a proxy first.');
        return;
      }

      logger.info('Launching Discord directly in main window', 'App');
      setMessage('Opening Discord in main window...');

      // Resolve proxy URL dynamically from backend (selected port)
      let proxyUrl = await fetchProxyBase();
      if (!proxyUrl) {
        // fallback: try to read from stored state or default
        const assumed = getProxyBase() || 'http://localhost:8080';
        proxyUrl = assumed;
      }
      setProxyOrigin(proxyUrl);
      // Set Discord URL to the Discord app path through the proxy
      const discordAppUrl = `${proxyUrl}/app`;
      setDiscordUrl(discordAppUrl);
      setCurrentView('discord');

      logger.info(`Discord will load through proxy at ${discordAppUrl}`, 'App');
      addLog(`[Use] Launching Discord via ${discordAppUrl}`);

    } catch (error) {
      const msg = toErrorString(error);
      setMessage(`Error: ${msg}`);
      logger.error('Error launching Discord', 'App', msg);
      addLog(`[Use] Failed to launch Discord - ${msg}`);
    }
  };

  const returnToProxyManager = async () => {
    try {
      logger.info('Returning to proxy manager', 'App');
      setCurrentView('proxy-manager');
      
      // Also notify the backend
      await invoke('return_to_proxy_manager');
    } catch (error) {
      logger.error('Error returning to proxy manager', 'App', toErrorString(error));
    }
  };

  const reloadDiscord = () => {
    try {
      if (currentView === 'discord') {
  // Reload by resetting the iframe src
        const tempUrl = discordUrl;
        setDiscordUrl('');
        // Use a short timeout to ensure the URL change is detected
        setTimeout(() => {
          setDiscordUrl(tempUrl);
          logger.info('Reloading Discord in main window', 'App');
          setMessage('Reloading Discord...');
        }, 100);
      }
    } catch (error) {
      const msg = toErrorString(error);
      logger.error('Error reloading Discord', 'App', msg);
      setMessage(`Error reloading Discord: ${msg}`);
    }
  };

  const toggleController = () => {
    setControllerExpanded(!controllerExpanded);
  };

  const clearDiscordSession = async () => {
    try {
      logger.info('Clearing Discord session and storage', 'App');
      setMessage('Clearing Discord session...');

  // Clear local/session storage
      if (typeof Storage !== 'undefined') {
        try {
          // Clear all local storage
          localStorage.clear();
          // Clear all session storage
          sessionStorage.clear();
          logger.info('Cleared browser storage', 'App');
        } catch (storageError) {
          logger.warning('Could not clear browser storage', 'App', toErrorString(storageError));
        }
      }

  // Ask backend to clear session (best-effort)
      try {
        const backendResult = await invoke('clear_discord_session_data');
        logger.info('Backend session clearing result', 'App', backendResult);
        setMessage('Cleared Discord session data...');
      } catch (backendError) {
        logger.warning('Backend session clearing failed', 'App', toErrorString(backendError));
        setMessage('Backend clearing failed, continuing...');
      }

  // Hit Discord logout endpoint
      setMessage('Logging out of Discord...');
      setDiscordUrl('https://discord.com/api/auth/logout');

      // Wait longer for the logout to process
      await new Promise(resolve => setTimeout(resolve, 2000));

  // Blank page to flush state
      setMessage('Cleaning session state...');
      setDiscordUrl('about:blank');

      // Wait for blank page to load
      await new Promise(resolve => setTimeout(resolve, 1000));

  // Navigate back to login
      const base = (await fetchProxyBase()) || getProxyBase() || 'http://localhost:8080';
      setDiscordUrl(`${base}/login`);
      logger.info('Discord session cleared and reloaded', 'App');
      setMessage('Discord session cleared. You should now be logged out.');

    } catch (error) {
      const msg = toErrorString(error);
      logger.error('Error clearing Discord session', 'App', msg);
      setMessage(`Error clearing session: ${msg}`);
    }
  };

  function getProxyTypes() {
    const types = new Set();
    proxies.forEach(proxy => {
      if (proxy.proxy_type) {
        types.add(proxy.proxy_type);
      }
    });
    return Array.from(types);
  }

  function getCountries() {
    const countries = new Set();
    proxies.forEach(proxy => {
      if (proxy.country && proxy.country !== 'Unknown') {
        countries.add(proxy.country);
      }
    });
    return Array.from(countries);
  }

  // Function to save proxies to cache
  const saveProxiesToCache = async () => {
    try {
      const response = await invoke('save_proxies_cache');
      logger.info('Saved proxies to cache', 'ProxyCache');
      setCacheStatus('Working proxies saved to cache');
      
      // Clear cache status after 3 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 3000);
    } catch (error) {
      logger.error('Failed to save proxies to cache', 'ProxyCache', toErrorString(error));
      setCacheStatus('Failed to save to cache');
      
      // Clear cache status after 3 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 3000);
    }
  };
  
  // Function to load proxies from cache
  const loadProxiesFromCache = async () => {
    try {
      const response = await invoke('load_proxies_cache');
      logger.info('Loaded proxies from cache', 'ProxyCache');
      setCacheStatus('Loaded proxies from cache');

      // Clear cache status after 3 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 3000);
    } catch (error) {
      logger.error('Failed to load proxies from cache', 'ProxyCache', toErrorString(error));
      // Only show a status if there was an actual error, not just if no cache exists
      if (error.toString().indexOf("Failed to read cache file") === -1) {
        setCacheStatus('Failed to load from cache');

        // Clear cache status after 3 seconds
        setTimeout(() => {
          setCacheStatus('');
        }, 3000);
      }
    }
  };

  // Function to delete/clear proxies cache
  const deleteProxiesCache = async () => {
    try {
      const confirmation = window.confirm('Are you sure you want to delete the proxy cache?');
      if (!confirmation) return;

      const response = await invoke('clear_proxies_cache');
      logger.info('Deleted proxies cache', 'ProxyCache');
      setCacheStatus('Cache deleted successfully');

      // Clear cache status after 3 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 3000);
    } catch (error) {
      logger.error('Failed to delete proxies cache', 'ProxyCache', toErrorString(error));
      setCacheStatus('Failed to delete cache');

      // Clear cache status after 3 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 3000);
    }
  };

  // Function to completely clear account and session data
  const clearAccountCompletely = async () => {
    try {
      const confirmation = window.confirm('This will log you out completely and clear all cached data. Continue?');
      if (!confirmation) return;

      setMessage('Clearing account and all data...');
      logger.info('Starting complete account clearing', 'App');

  // 1) Clear proxy cache
      try {
        await invoke('clear_proxies_cache');
        logger.info('Cleared proxy cache', 'App');
        setMessage('Cleared proxy cache...');
      } catch (error) {
        logger.warning('Failed to clear proxy cache', 'App', toErrorString(error));
      }

  // 2) Clear frontend storage
      setMessage('Clearing browser storage...');
      if (typeof Storage !== 'undefined') {
        try {
          // Clear all local storage
          localStorage.clear();
          // Clear all session storage
          sessionStorage.clear();
          logger.info('Cleared frontend browser storage', 'App');
        } catch (storageError) {
          logger.warning('Could not clear frontend browser storage', 'App', toErrorString(storageError));
        }
      }

  // 3) Ask backend to clear session
      setMessage('Clearing Discord session data...');
      try {
        const backendResult = await invoke('clear_discord_session_data');
        logger.info('Backend session clearing result', 'App', backendResult);
        setMessage('Cleared Discord session data...');
      } catch (backendError) {
        logger.warning('Backend session clearing failed', 'App', toErrorString(backendError));
        setMessage('Backend clearing failed, continuing...');
      }

  // 4) Logout endpoint
      setMessage('Logging out of Discord...');
      setDiscordUrl('https://discord.com/api/auth/logout');

      // Wait longer for the logout to process
      await new Promise(resolve => setTimeout(resolve, 2000));

  // 5) Blank page to reset
      setMessage('Cleaning webview state...');
      setDiscordUrl('about:blank');

      // Wait for blank page to load
      await new Promise(resolve => setTimeout(resolve, 1000));

  // 6) Try extra webview clearing hooks (if available)
      try {
        // Additional manual clearing if supported
        if (window.webview && window.webview.clearData) {
          await window.webview.clearData();
          logger.info('Cleared webview data manually', 'App');
        }
      } catch (clearError) {
        logger.debug('Manual webview clearing not supported or failed', 'App', toErrorString(clearError));
      }

  // 7) Navigate to app
  setMessage('Reloading Discord...');
  const finalBase = getProxyBase() || 'http://localhost:8080';
  setDiscordUrl(`${finalBase}/app`);

  // Done
      setMessage('Account cleared completely. You should now be logged out.');
      setCacheStatus('Account cleared completely');
      logger.info('Complete account clearing finished successfully', 'App');

      // Clear status after 5 seconds (longer to ensure user sees confirmation)
      setTimeout(() => {
        setCacheStatus('');
        setMessage('');
      }, 5000);

    } catch (error) {
      const msg = toErrorString(error);
      logger.error('Failed to clear account completely', 'App', msg);
      setMessage(`Error clearing account: ${msg}`);
      setCacheStatus('Error during account clearing');

      // Clear error status after 5 seconds
      setTimeout(() => {
        setCacheStatus('');
      }, 5000);
    }
  };

  // Visual age indicator based on last_checked
  function getProxyAgeClass(lastChecked) {
    if (!lastChecked) return '';
    
    try {
      const now = new Date();
      const lastCheckedDate = new Date(lastChecked);
      const hoursDiff = (now - lastCheckedDate) / (1000 * 60 * 60);
      
      if (hoursDiff > 3) {
        return 'test-old';
      } else if (hoursDiff > 1) {
        return 'test-aging';
      }
      return '';
    } catch (e) {
      return '';
    }
  }

  return (
    <div className={`container ${currentView === 'discord' ? 'discord-mode' : ''}`}>
      {currentView === 'proxy-manager' ? (
  // Proxy Manager
        <>
          <div className="app-header">
            <div className="app-title">FreeDiscord</div>
            <div className="app-status">
              <span className={proxyStatus === 'Running' ? 'status-running' : 'status-stopped'}>
                {proxyStatus}
              </span>
            </div>
          </div>
          
          <div className="status-card">
            {/* Status label shown above */}
            {activeProxy && (
              <div className="active-proxy-info">
                <p>Active Proxy: {activeProxy.ip}:{activeProxy.port} ({activeProxy.proxy_type})</p>
                <p>Location: {activeProxy.city}, {activeProxy.country}</p>
                <p>Ping: {activeProxy.ping_ms >= 0 ? `${activeProxy.ping_ms} ms` : 'Unknown'}</p>
              </div>
            )}
            <div className="proxy-stats">
              <p>Total Proxies: <strong>{proxies.length}</strong></p>
              <p>Working Proxies: <strong>{proxies.filter(p => p.is_working).length}</strong></p>
            </div>
            {cacheStatus && (
              <div className="cache-status">
                <p>{cacheStatus}</p>
              </div>
            )}
          </div>
          
          <div className="actions" style={{ display: 'flex', gap: '8px', alignItems: 'center' }}>
            <div className="actions-left" style={{ display: 'flex', gap: '8px', alignItems: 'center' }}>
              <div className="pipeline-actions" style={{ display:'flex', gap: 6 }}>
                <button 
                  className={`blue ${stepLoading.fetch ? 'loading' : ''}`} 
                  onClick={runStepFetch} 
                  disabled={stepLoading.fetch || stepLoading.parse || stepLoading.test || testingStatus.active}
                  title={stepLoading.parse ? 'Parsing in progress…' : stepLoading.test ? 'Testing in progress…' : undefined}
                >
                  Step 1: Fetch
                </button>
                <button 
                  className={`blue ${stepLoading.parse ? 'loading' : ''}`} 
                  onClick={runStepParse} 
                  disabled={stepLoading.parse || stepLoading.fetch || stepLoading.test || testingStatus.active}
                  title={stepLoading.fetch ? 'Fetching in progress…' : stepLoading.test ? 'Testing in progress…' : undefined}
                >
                  Step 2: Parse
                </button>
                <button 
                  className={`blue ${stepLoading.test ? 'loading' : ''}`} 
                  onClick={runStepTest} 
                  disabled={stepLoading.test || stepLoading.fetch || stepLoading.parse}
                  title={stepLoading.fetch ? 'Fetching in progress…' : stepLoading.parse ? 'Parsing in progress…' : undefined}
                >
                  Step 3: Test
                </button>
              </div>
              <button className="red" onClick={stopProxy} disabled={proxyStatus === 'Stopped'}>
                Stop Proxy
              </button>
              <button className="gray" onClick={() => setShowAddCustomModal(true)}>
                Add Custom Proxy
              </button>
            </div>
            <div className="actions-right" style={{ marginLeft: 'auto' }}>
              <button 
                className="green" 
                onClick={launchDiscord} 
                disabled={proxyStatus === 'Stopped'}
                title="Launch Discord in main window"
              >
                Launch Discord
              </button>
            </div>
          </div>

          {/* Add Custom Proxy modal */}
          {showAddCustomModal && (
            <div className="modal-overlay" onClick={(e) => { if (e.target.classList.contains('modal-overlay')) setShowAddCustomModal(false); }}>
              <div className="modal">
                <div className="modal-header">
                  <h4>Add Custom Proxy</h4>
                </div>
                <div className="modal-body">
                  <div className="modal-field">
                    <label>IP</label>
                    <input
                      type="text"
                      placeholder="e.g. 1.2.3.4"
                      value={customProxyIp}
                      onChange={(e) => setCustomProxyIp(e.target.value)}
                    />
                  </div>
                  {/* Protocol before port for better defaults */}
                  <div className="modal-field">
                    <label>Type</label>
                    <select 
                      value={customProxyType} 
                      onChange={(e) => {
                        const nextType = e.target.value;
                        setCustomProxyType(nextType);
                        // Set sensible default ports by protocol
                        if (!customProxyPort || String(customProxyPort).trim() === '') {
                          if (nextType === 'http' || nextType === 'https') {
                            setCustomProxyPort('8080');
                          } else if (nextType === 'socks4' || nextType === 'socks5') {
                            setCustomProxyPort('1080');
                          }
                        } else {
                          // If user has typed, still adjust to protocol defaults to be helpful
                          if (nextType === 'http' || nextType === 'https') {
                            setCustomProxyPort('8080');
                          } else if (nextType === 'socks4' || nextType === 'socks5') {
                            setCustomProxyPort('1080');
                          }
                        }
                      }}>
                      <option value="http">http</option>
                      <option value="https">https</option>
                      <option value="socks4">socks4</option>
                      <option value="socks5">socks5</option>
                    </select>
                  </div>
                  <div className="modal-field">
                    <label>Port</label>
                    <input
                      type="number"
                      placeholder="e.g. 1080"
                      value={customProxyPort}
                      onChange={(e) => setCustomProxyPort(e.target.value)}
                    />
                  </div>
                  <div className="modal-field">
                    <label>Username (optional)</label>
                    <input
                      type="text"
                      placeholder="proxy username"
                      value={customProxyUser}
                      onChange={(e) => setCustomProxyUser(e.target.value)}
                    />
                  </div>
                  <div className="modal-field">
                    <label>Password (optional)</label>
                    <div className="password-input-wrap">
                      <input
                        type={showPassword ? 'text' : 'password'}
                        placeholder="proxy password"
                        value={customProxyPass}
                        onChange={(e) => setCustomProxyPass(e.target.value)}
                      />
                      <button
                        type="button"
                        className={`small gray password-toggle`}
                        onClick={() => setShowPassword(v => !v)}
                        title={showPassword ? 'Hide password' : 'Show password'}
                      >
                        {showPassword ? 'Hide' : 'Show'}
                      </button>
                    </div>
                  </div>
                  {/* Detailed test logs */}
                  {detailedLogs.length > 0 && (
                    <div className="modal-logs">
                      <div className="modal-logs-header">
                        <h5>Detailed Test Logs</h5>
                        <div className="modal-logs-actions">
                          <button
                            className={`small auto-scroll-toggle toggle ${autoScrollDetailedLogs ? 'on' : 'off'}`}
                            onClick={() => setAutoScrollDetailedLogs(!autoScrollDetailedLogs)}
                            aria-pressed={autoScrollDetailedLogs}
                            title="Toggle auto-scroll"
                          >
                            Auto-scroll: {autoScrollDetailedLogs ? 'On' : 'Off'}
                          </button>
                          <button
                            className="small clear-logs"
                            onClick={() => setDetailedLogs([])}
                            title="Clear detailed logs"
                          >
                            Clear
                          </button>
                        </div>
                      </div>
                      <div className="modal-logs-body" ref={detailedLogContainerRef}>
                        <ul>
                          {detailedLogs.slice(-200).map((item, index) => {
                            const { /* timestamp, */ text } = typeof item === 'string' ? { timestamp: null, text: item } : (item || {});
                            let safeText = (typeof text === 'string' ? text : (() => { try { return JSON.stringify(text, null, 2); } catch { return String(text); } })());
                            if (typeof safeText === 'string') {
                              safeText = safeText.replace(/^\[object Object\]$/, '');
                            }
                            const fallback = (typeof item === 'string' ? item : (() => { try { return JSON.stringify(item, null, 2); } catch { return String(item); } })());
                            return (
                              <li key={index} className="log-message">
                                <span className="log-text">{safeText || fallback}</span>
                              </li>
                            );
                          })}
                        </ul>
                      </div>
                    </div>
                  )}
                </div>
                <div className="modal-footer">
                  <button
                    className={`small blue ${isTestingDetailed ? 'loading' : ''}`}
                    onClick={testCustomProxyDetailed}
                    disabled={!customProxyIp || !customProxyPort || isTestingDetailed}
                  >
                    {isTestingDetailed ? '⏳ Testing…' : 'Test Detailed'}
                  </button>
                  <button className="small gray" onClick={addCustomProxy} disabled={isAddingCustomProxy}>
                    {isAddingCustomProxy ? 'Adding…' : 'Add Proxy'}
                  </button>
                  <button className="small red" onClick={() => setShowAddCustomModal(false)} disabled={isAddingCustomProxy || isTestingDetailed}>
                    Cancel
                  </button>
                </div>
              </div>
            </div>
          )}
          
          {/* Logs */}
          <div className="log-container">
            <h3>
              Logs {testingStatus.active && testingStatus.count > 0 && 
                <span className="testing-status">
                  (Testing {testingStatus.count} proxies...)
                </span>
              }
              <span className="log-actions-right">
                <button
                  className={`small auto-scroll-toggle toggle ${autoScrollLogs ? 'on' : 'off'}`}
                  onClick={() => setAutoScrollLogs(!autoScrollLogs)}
                  title="Toggle auto-scroll to newest logs"
                  aria-pressed={autoScrollLogs}
                >
                  Auto-scroll: {autoScrollLogs ? 'On' : 'Off'}
                </button>
                <button
                  className="small clear-logs"
                  onClick={() => setLogMessages([])}
                  title="Clear logs"
                >
                  Clear
                </button>
              </span>
            </h3>
            <div className="log-messages" ref={logContainerRef}>
              {logMessages.length > 0 ? (
                <ul>
                  {logMessages.map((item, index) => {
                    const { timestamp, text } = typeof item === 'string' ? { timestamp: null, text: item } : (item || {});
                    let safeText = (typeof text === 'string' ? text : (() => { try { return JSON.stringify(text, null, 2); } catch { return String(text); } })());
                    if (typeof safeText === 'string') {
                      safeText = safeText.replace(/^\[object Object\]$/, '');
                    }
                    const fallback = (typeof item === 'string' ? item : (() => { try { return JSON.stringify(item, null, 2); } catch { return String(item); } })());
                    return (
                      <li key={index} className="log-message">
                        {timestamp && <span className="log-time">{formatDateTime(timestamp)} — </span>}
                        <span className="log-text">{safeText || fallback}</span>
                      </li>
                    );
                  })}
                </ul>
              ) : (
                <p>No logs yet. Click "Fetch Proxies" to get started.</p>
              )}
            </div>
          </div>
          
          <div className="filters">
            <div className="filter">
              <input 
                type="text" 
                placeholder="Search IP, city, country..."
                value={searchTerm}
                onChange={(e) => setSearchTerm(e.target.value)}
              />
            </div>
            
            <div className="filter">
              <select 
                value={countryFilter} 
                onChange={(e) => setCountryFilter(e.target.value)}
              >
                <option value="">All Countries</option>
                {getCountries().map(country => (
                  <option key={country} value={country}>{country}</option>
                ))}
              </select>
            </div>
            
            <div className="filter">
              <select 
                value={proxyTypeFilter} 
                onChange={(e) => setProxyTypeFilter(e.target.value)}
              >
                <option value="">All Types</option>
                {getProxyTypes().map(type => (
                  <option key={type} value={type}>{type}</option>
                ))}
              </select>
            </div>
            
            <div className="filter checkbox">
              <label>
                <input 
                  type="checkbox" 
                  checked={onlyWorking} 
                  onChange={(e) => setOnlyWorking(e.target.checked)}
                />
                Working only
              </label>
            </div>
          </div>
          
          <div className="pagination">
            <button 
              onClick={() => getPage(0)} 
              disabled={currentPage === 0}
            >
              First
            </button>
            <button 
              onClick={() => getPage(currentPage - 1)} 
              disabled={currentPage === 0}
            >
              Previous
            </button>
            <span>Page {currentPage + 1} of {totalPages || 1}</span>
            <button 
              onClick={() => getPage(currentPage + 1)} 
              disabled={currentPage >= totalPages - 1}
            >
              Next
            </button>
            <button 
              onClick={() => getPage(totalPages - 1)} 
              disabled={currentPage >= totalPages - 1}
            >
              Last
            </button>
          </div>
          
          <div className="proxies-table">
            <table>
              <thead>
                <tr>
                  <th>IP</th>
                  <th>Port</th>
                  <th>Type</th>
                  <th>Country</th>
                  <th>City</th>
                  <th>Ping</th>
                  <th>Status</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {filteredProxies.length > 0 ? (
                  filteredProxies.map((proxy) => {
                    const key = `${proxy.ip}:${proxy.port}`;
                    const isTestingThis = testingProxies.has(key);
                    const isStartingThis = startingProxies.has(key);
                    return (
                      <tr 
                        key={key} 
                        className={`${proxy.is_working ? 'working' : 'not-working'} ${getProxyAgeClass(proxy.last_checked)}`}>
                        <td>{proxy.ip}</td>
                        <td>{proxy.port}</td>
                        <td>{proxy.proxy_type}</td>
                        <td>{proxy.country}</td>
                        <td>{proxy.city}</td>
                        <td>{proxy.ping_ms >= 0 ? `${proxy.ping_ms} ms` : 'Unknown'}</td>
                        <td>
                          <span className={`status ${proxy.status}`}>
                            {proxy.status === 'testing' ? 'Testing...' : 
                             proxy.status === 'tested' ? 'Working' : 
                             proxy.status === 'failed' ? 'Failed' : 'Pending'}
                          </span>
                          {proxy.status !== 'testing' && proxy.status !== 'pending' && getProxyAgeClass(proxy.last_checked) === 'test-old' && (
                            <span className="retest-indicator" title={`Tested ${proxy.last_checked}. Will be retested soon.`}>⟳</span>
                          )}
                        </td>
                        <td>
                          <button
                            className={`small gray ${isTestingThis ? 'loading' : ''}`}
                            onClick={() => testProxy(proxy)}
                            disabled={isTestingThis}
                            aria-busy={isTestingThis}
                          >
                            {isTestingThis ? '⏳ Testing…' : 'Test'}
                          </button>
                          {proxy.is_working && (
                            <button
                              className={`small green ${proxy.proxy_type.toLowerCase().includes('socks5') ? 'socks5-recommended' : ''} ${isStartingThis ? 'loading' : ''}`}
                              onClick={() => startProxy(proxy)}
                              disabled={isStartingThis}
                              aria-busy={isStartingThis}
                              title={proxy.proxy_type.toLowerCase().includes('socks5') ? 'SOCKS5 recommended for Discord' : ''}
                            >
                              {isStartingThis ? '⏳ Starting…' : `Use ${proxy.proxy_type.toLowerCase().includes('socks5') ? '⭐' : ''}`}
                            </button>
                          )}
                          <button
                            className="small red"
                            onClick={() => deleteProxy(proxy)}
                            title="Delete this proxy"
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    );
                  })
                ) : (
                  <tr>
                    <td colSpan="8" className="no-proxies">
                      {proxies.length > 0 
                        ? 'No proxies match your filters.' 
                        : 'No proxies found. Click "Fetch Proxies" to get started.'}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          
          <div className="actions secondary-actions">
            <button className="small gray" onClick={saveProxiesToCache} disabled={!proxies.some(p => p.is_working)}>
              Save to Cache
            </button>
            <button className="small gray" onClick={loadProxiesFromCache}>
              Load from Cache
            </button>
          </div>
        </>
      ) : (
  // Discord View
        <>
          <div className="discord-direct-view">
            {discordUrl ? (
              <div className="discord-webview-container">
                {/* Iframe-based view (works in dev; swappable with WebView) */}
                <iframe 
                  src={discordUrl}
                  className="discord-iframe-fullscreen"
                  allow="microphone; camera; notifications; fullscreen"
                  title="Discord Web"
                  onLoad={() => {
                    logger.info('Discord loaded in main window', 'App');
                    setMessage('Discord loaded successfully');
                    
                    // Minimal CSP/error monitoring inside iframe
                    try {
                      // Avoid installing duplicate handlers on reloads
                      if (!window.__dcProxyHandlersInstalled) {
                        window.__dcProxyHandlersInstalled = true;
                      document.addEventListener('securitypolicyviolation', function(e) {
                        if (e.blockedURI && (e.blockedURI.includes('discord.com') || e.blockedURI.includes('discord.gg'))) {
                          logger.warning(`CSP violation: ${e.violatedDirective}`, 'Discord', {
                            blockedURI: e.blockedURI,
                            violatedDirective: e.violatedDirective,
                            effectiveDirective: e.effectiveDirective
                          });
                          
                          // Show a user-friendly message based on the type of CSP violation
                          if (e.blockedURI.startsWith('http:') && e.blockedURI.includes('discord')) {
                            setMessage(`Error: Need HTTPS connection to ${e.blockedURI}`);
                          } else if (e.blockedURI.startsWith('wss:')) {
                            setMessage(`WebSocket connection to ${e.blockedURI} blocked`);
                          } else {
                            setMessage(`CSP warning: Blocked connection to ${e.blockedURI}`);
                          }
                        }
                      });
                      
                      // General error events
                      window.addEventListener('error', function(e) {
                        if (e.message && e.message.includes('WebSocket')) {
                          logger.error(`WebSocket error: ${e.message}`, 'Discord');
                          setMessage(`WebSocket error: ${e.message}`);
                        }
                      });
                      
                      logger.info('Set up CSP and error monitoring', 'App');
                      }
                    } catch (error) {
                      logger.warning('Could not set up error monitoring', 'App', error?.message || String(error));
                    }
                  }}
                  onError={(e) => {
                    logger.error('Discord failed to load in main window', 'App', e?.message || String(e));
                    setMessage('Error loading Discord. Check if proxy is running correctly.');
                    setDiscordLoadFailed(true);
                  }}
                ></iframe>
                
                {discordLoadFailed && (
                  <div className="discord-error-overlay">
                    <div className="discord-error">
                      <h4>Failed to load Discord</h4>
                      <p>Please check if the proxy server is running and try again.</p>
                      <button 
                        onClick={() => {
                          setDiscordLoadFailed(false);
                          // Force refresh
                          setDiscordUrl('');
                          setTimeout(() => {
                            const base = getProxyBase() || 'http://localhost:8080';
                            const fallbackUrl = discordUrl || `${base}/app`;
                            setDiscordUrl(fallbackUrl);
                          }, 100);
                        }}
                      >
                        Retry
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ) : (
              <div className="loading-placeholder">Loading Discord...</div>
            )}
          </div>
          
          {/* Floating Controller */}
          <div className={`floating-controller ${controllerExpanded ? 'expanded' : 'collapsed'}`}>
            <div className="controller-toggle" onClick={toggleController}>
              {controllerExpanded ? '◀' : '▶'} 
            </div>
            
            <div className="controller-content">
              <div className="controller-header">
                <h3>Proxy Controller</h3>
              </div>
              
              <div className="controller-body">
                {activeProxy ? (
                  <div className="controller-proxy-info">
                    <p><strong>Active Proxy:</strong> {activeProxy.ip}:{activeProxy.port}</p>
                    <p><strong>Location:</strong> {activeProxy.city}, {activeProxy.country}</p>
                    <p><strong>Ping:</strong> {activeProxy.ping_ms >= 0 ? `${activeProxy.ping_ms} ms` : 'Unknown'}</p>
                    <p><strong>Status:</strong> <span className={proxyStatus === 'Running' ? 'status-running' : 'status-stopped'}>{proxyStatus}</span></p>
                  </div>
                ) : (
                  <div className="controller-proxy-info">
                    <p><strong>No Active Proxy</strong></p>
                    <p><strong>Status:</strong> <span className="status-stopped">Stopped</span></p>
                  </div>
                )}
                
                <div className="controller-actions">
                  <button className="small gray" onClick={returnToProxyManager}>
                    Back to Proxy Manager
                  </button>
                  <button className="small gray" onClick={reloadDiscord}>
                    Reload Discord
                  </button>
                  <button className="small gray" onClick={clearDiscordSession}>
                    Clear Session
                  </button>
                  <button className="small gray" onClick={deleteProxiesCache}>
                    Delete Cache
                  </button>
                  <button className="small gray" onClick={clearAccountCompletely}>
                    Clear Account
                  </button>
                  <button className="small red" onClick={stopProxy} disabled={proxyStatus === 'Stopped'}>
                    Stop Proxy
                  </button>
                </div>
                
                {message && (
                  <div className="controller-message">
                    {message}
                  </div>
                )}

                {cacheStatus && (
                  <div className="controller-cache-status">
                    {cacheStatus}
                  </div>
                )}
                
                {logMessages.length > 0 && (
                  <div className="controller-logs">
                    <p><strong>Recent Logs:</strong></p>
                    <div className="controller-log-entries">
                      {logMessages.slice(-3).map((item, idx) => {
                        const { timestamp, text } = typeof item === 'string' ? { timestamp: null, text: item } : (item || {});
                        const safeText = (typeof text === 'string' ? text : (() => { try { return JSON.stringify(text, null, 2); } catch { return String(text); } })()).replace(/^\[object Object\]$/, '');
                        return (
                          <div key={idx} className="controller-log-entry">
                            {timestamp && <span className="log-time">{formatDateTime(timestamp)}</span>} {safeText || (typeof item === 'string' ? item : (() => { try { return JSON.stringify(item, null, 2); } catch { return String(item); } })())}
                          </div>
                        );
                      })}
                    </div>
                  </div>
                )}
                
                {/* Debug info */}
                <div className="controller-debug">
                  <details>
                    <summary>Debug Info</summary>
                    <p><strong>Active URL:</strong> {discordUrl}</p>
                    <p><strong>Proxy Status:</strong> {proxyStatus}</p>
                    <p><strong>Testing Status:</strong> {testingStatus.active ? `Testing ${testingStatus.count} proxies` : 'Idle'}</p>
                    <button className="small gray" onClick={async () => {
                      try {
                        const base = (await fetchProxyBase()) || getProxyBase() || 'http://localhost:8080';
                        await invoke('open_in_system_browser', { url: base });
                        logger.info(`Requested system browser open: ${base}`, 'App');
                      } catch (e) {
                        const msg = toErrorString(e);
                        logger.error('Failed to open in system browser', 'App', msg);
                        setMessage(`Failed to open in system browser: ${msg}`);
                      }
                    }}>
                      Open in Browser
                    </button>
                  </details>
                </div>
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

export default App;