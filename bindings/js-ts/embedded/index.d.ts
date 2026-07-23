export interface OperationOptions {
  multi?: boolean;
  atomic?: boolean;
  createIfMissing?: boolean;
  returnShape?: string;
  hydrate?: boolean;
  limit?: number;
  offset?: number;
  orderBy?: string;
  orderDirection?: string;
  includeDiagnostics?: boolean;
}

export type OperationFilter =
  | string
  | { drawer: string }
  | { pointer: string }
  | { query: any }
  | readonly OperationFilter[]
  | any;

export interface StorageInventory {
  name: string;
  record_count: number;
  disk_size_bytes: number;
  register_file_count: number;
}

export type StatusRequest =
  | 'Tenants'
  | 'Databases'
  | 'Storage'
  | 'DrawerNames'
  | 'CachedDrawerCount'
  | { Schemas: { database_name: string } }
  | { Drawers: { database_name: string; schema_name: string } }
  | { Wal: { database_name?: string | null } }
  | { Path: { path: string } };

export declare const PACKAGE_NAME: '@wardrobe/embedded';
export declare const PACKAGE_VERSION: '0.26.722';

export class WardrobeClient {
  static open(connectionString: string): Promise<WardrobeClient>;
  upsert(payload: any, filter?: OperationFilter, options?: OperationOptions): Promise<any>;
  read(filter?: OperationFilter, options?: OperationOptions): Promise<any>;
  delete(filter?: OperationFilter, options?: OperationOptions): Promise<any>;
  inspect(filter?: OperationFilter, options?: OperationOptions): Promise<any>;
  count(filter?: OperationFilter, options?: OperationOptions): Promise<number>;
  clean(request?: any): Promise<any>;
  create(request: any): Promise<any>;
  alter(request: any): Promise<any>;
  drop(request: any): Promise<any>;
  backup(sourcePath: string): Promise<any>;
  restore(destinationPath: string, archive: any): Promise<any>;
  grant(request: any): Promise<any>;
  revoke(request: any): Promise<any>;
  status(request: 'Tenants' | 'DrawerNames'): Promise<string[]>;
  status(request: 'Databases'): Promise<StorageInventory[]>;
  status(request: { Schemas: { database_name: string } }): Promise<string[]>;
  status(request: {
    Drawers: { database_name: string; schema_name: string };
  }): Promise<StorageInventory[]>;
  status(request: 'CachedDrawerCount'): Promise<number>;
  status(request?: StatusRequest): Promise<unknown>;
}
