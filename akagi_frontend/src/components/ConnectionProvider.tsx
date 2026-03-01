import { type ReactNode, useEffect, useMemo, useState } from 'react';

import { type ConnectionConfig, ConnectionContext } from '@/contexts/ConnectionContext';

export function ConnectionProvider({ children }: { children: ReactNode }) {
  const [protocol] = useState(() => {
    const saved = localStorage.getItem('protocol');
    if (saved) return saved;
    // 开发环境（端口 5173）默认使用 http，否则使用当前协议
    if (window.location.port === '5173') return 'http';
    if (window.location.protocol === 'file:') return 'http';
    return window.location.protocol.replace(':', '');
  });

  const [backendAddress, setBackendAddress] = useState(() => {
    const saved = localStorage.getItem('backendAddress');
    if (saved) return saved;
    // 开发环境默认值
    if (window.location.port === '5173') return '127.0.0.1:8765';
    if (window.location.protocol === 'file:') return '127.0.0.1:8765';
    return window.location.host;
  });

  useEffect(() => {
    if (!window.electron) return;
    window.electron
      .invoke('get-backend-config')
      .then((cfg) => {
        if (cfg && cfg.host && cfg.port) {
          const newAddress = `${cfg.host}:${cfg.port}`;
          setBackendAddress(newAddress);
          localStorage.setItem('backendAddress', newAddress);
        }
      })
      .catch((err) => {
        console.error('[ConnectionProvider] Failed to fetch backend config from electron:', err);
      });
  }, []);

  const [clientId] = useState(() => Math.random().toString(36).slice(2) + Date.now().toString(36));

  const value = useMemo(() => {
    const apiBase = `${protocol}://${backendAddress}`;
    const backendUrl = `${protocol}://${backendAddress}/sse?clientId=${clientId}`;
    return {
      protocol,
      backendAddress,
      clientId,
      apiBase,
      backendUrl,
    } satisfies ConnectionConfig;
  }, [backendAddress, clientId, protocol]);

  return <ConnectionContext.Provider value={value}>{children}</ConnectionContext.Provider>;
}
