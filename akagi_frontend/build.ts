import { execSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';

try {
  // 1. 从 Python 包中读取版本
  // 通过读取 pyproject.toml 获取版本，避免依赖已安装的包

  const pyprojectPath = path.join(process.cwd(), '..', 'akagi_backend', 'pyproject.toml');
  const pyprojectContent = fs.readFileSync(pyprojectPath, 'utf-8');
  const versionMatch = pyprojectContent.match(/^\s*version\s*=\s*["']([^"']+)["']/m);

  let version = 'dev';
  if (versionMatch && versionMatch[1]) {
    version = versionMatch[1];
  } else {
    console.warn('Could not find version in pyproject.toml, falling back to "dev"');
  }

  console.log(`Detected Akagi-NG version: ${version}`);

  // 2. 设置环境变量并执行构建
  // 通过 child_process 环境变量传递版本号

  console.log('Running: tsc && vite build');
  execSync('tsc && vite build', {
    stdio: 'inherit',
    env: { ...process.env, AKAGI_VERSION: version },
  });
} catch (error) {
  console.error('Build failed:', error);
  process.exit(1);
}
