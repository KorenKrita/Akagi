// Reserved names of the built-in, pure-Rust bots (see the backend's
// `bot::native` module — `is_native` gates on exactly these). Always
// available: their weights are embedded in the binary, so no install or
// Python environment is ever needed. Shared here so the Bots table, the
// status bar, the Setup wizard and the MJOT auto-select logic can't drift
// apart on the spelling.

export const NATIVE_4P = 'akagi-native'
export const NATIVE_3P = 'akagi-native3p'

export function isNativeBot(name: string): boolean {
  return name === NATIVE_4P || name === NATIVE_3P
}
