export type TauriCommandArgs = Record<string, unknown>;

export type TauriInvoke = <T>( command: string, args?: TauriCommandArgs) => Promise<T>;

export type WardrobeCommandStatus = 'disconnected' | 'connecting' | 'connected' | 'error';
