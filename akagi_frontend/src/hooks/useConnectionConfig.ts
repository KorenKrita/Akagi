import { use } from 'react';

import { ConnectionContext } from '@/contexts/ConnectionContext';

/**
 * 管理后端连接配置的 Hook
 *
 * 自动从 localStorage 读取配置，在开发模式和生产模式下使用不同的默认值
 */
export function useConnectionConfig() {
  const context = use(ConnectionContext);
  if (!context) {
    throw new Error('useConnectionConfig must be used in ConnectionProvider');
  }
  return context;
}
