export type WardrobeConnectionTarget =
  | {
      readonly kind: 'embedded';
      readonly path: string;
      readonly requiresEmbeddedEngine: true;
      readonly usesSocketTransport: false;
    }
  | {
      readonly kind: 'network';
      readonly host: string;
      readonly port: number;
      readonly requiresEmbeddedEngine: false;
      readonly usesSocketTransport: true;
    }
  | {
      readonly kind: 'unix-socket';
      readonly path: string;
      readonly requiresEmbeddedEngine: false;
      readonly usesSocketTransport: true;
    };

export type WardrobeOperation =
  | 'read'
  | 'upsert'
  | 'delete'
  | 'inspect'
  | 'count'
  | 'clean'
  | 'create'
  | 'alter'
  | 'drop'
  | 'backup'
  | 'restore'
  | 'grant'
  | 'revoke'
  | 'status';

export declare const DEFAULT_NETWORK_PORT: 24842;
export declare const PACKAGE_NAMES: readonly ['@wardrobe/client', '@wardrobe/embedded'];
export declare const PACKAGE_VERSION: '0.26.725';
export declare const SUPPORTED_OPERATIONS: readonly WardrobeOperation[];

export declare function classifyConnectionTarget(
  connectionString: string
): WardrobeConnectionTarget;
