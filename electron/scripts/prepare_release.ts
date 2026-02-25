import fs from 'fs';
import path from 'path';

const rootDir = path.resolve(__dirname, '../../');
const extraDir = path.join(rootDir, 'build', 'extra');

// Ensure extraDir exists
if (fs.existsSync(extraDir)) {
  fs.rmSync(extraDir, { recursive: true, force: true });
}
fs.mkdirSync(extraDir, { recursive: true });

console.log('📦 Preparing release assets...');

// 1. LICENSE.txt

const licenseSource = path.join(rootDir, 'LICENSE');
if (fs.existsSync(licenseSource)) {
  fs.copyFileSync(licenseSource, path.join(extraDir, 'LICENSE.txt'));
  console.log('   ✅ LICENSE.txt created');
}

// 2. Create target folders
['lib', 'models', 'logs', 'config'].forEach((folder) => {
  const folderPath = path.join(extraDir, folder);
  if (!fs.existsSync(folderPath)) {
    fs.mkdirSync(folderPath);
  }
});

// 3. Bundle Models
['mortal.pth', 'mortal3p.pth'].forEach((modelFile) => {
  const src = path.join(rootDir, 'models', modelFile);
  if (fs.existsSync(src)) {
    fs.copyFileSync(src, path.join(extraDir, 'models', modelFile));
    console.log(`   ✅ Bundled model: ${modelFile}`);
  }
});

// 4. Bundle and rename libriichi for current platform
import os from 'os';
const platform = os.platform();
const arch = os.arch();

const sysStr =
  platform === 'win32'
    ? 'pc-windows-msvc'
    : platform === 'darwin'
      ? 'apple-darwin'
      : 'unknown-linux-gnu';
const ext = platform === 'win32' ? 'pyd' : 'so';
const archStr = arch === 'arm64' ? 'aarch64' : 'x86_64';

['libriichi', 'libriichi3p'].forEach((prefix) => {
  const pattern = `${prefix}-3.12-${archStr}-${sysStr}.${ext}`;
  const srcFile = path.join(rootDir, 'lib', pattern);
  if (fs.existsSync(srcFile)) {
    fs.copyFileSync(srcFile, path.join(extraDir, 'lib', `${prefix}.${ext}`));
    console.log(`   ✅ Bundled lib: ${prefix}.${ext} (from ${pattern})`);
  } else {
    // try fallback
    const fallbackSrc = path.join(rootDir, 'lib', `${prefix}.${ext}`);
    if (fs.existsSync(fallbackSrc)) {
      fs.copyFileSync(fallbackSrc, path.join(extraDir, 'lib', `${prefix}.${ext}`));
      console.log(`   ✅ Bundled lib: ${prefix}.${ext} (from fallback exact match)`);
    } else {
      console.warn(`   ⚠️ Warning: Could not find lib file ${pattern}`);
    }
  }
});

// 5. Config/Logs placeholders
['logs', 'config'].forEach((folder) => {
  fs.writeFileSync(path.join(extraDir, folder, '_placeholder'), '');
});

console.log('✅ Release assets prepared in build/extra');
