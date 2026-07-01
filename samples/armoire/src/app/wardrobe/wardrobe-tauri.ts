import { TauriInvoke } from "./wardrobe-tauri-definitions";

const tauri = globalThis as typeof globalThis &
  { __TAURI__: { core: { invoke: TauriInvoke }, tauri: { invoke: TauriInvoke } } };

export const invoke: TauriInvoke = (command, args) => {
  return tauri.__TAURI__.core.invoke(command, args);
}