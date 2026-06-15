export type TauriInvoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export type TauriRuntime = {
  core?: {
    invoke?: TauriInvoke;
  };
  tauri?: {
    invoke?: TauriInvoke;
  };
};

export type GlobalWithTauri = typeof globalThis & {
  __TAURI__?: TauriRuntime;
};
