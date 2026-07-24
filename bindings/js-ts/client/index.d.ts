export interface OperationOptions {
  multi?: boolean;
  atomic?: boolean;
  createIfMissing?: boolean;
  returnShape?: string;
  hydrate?: boolean;
  exclude_hydration?: string[];
  excludeHydration?: string[];
  projection?: string[];
  select?: string[];
  fields?: string[];
  limit?: number;
  offset?: number;
  orderBy?: string;
  orderDirection?: string;
  cursor?: string;
  page?: number;
  pageSize?: number;
  includeDiagnostics?: boolean;
}

export interface PaginationMetadata {
  next_cursor: string | null;
  has_more: boolean;
  page: number | null;
  page_size: number;
}

export interface PaginatedReadResult {
  records: any[];
  pagination: PaginationMetadata;
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

export interface RelationshipRequest {
  SchemaRule: {
    drawer_name: string;
    action: 'add';
    kind: 'relationship';
    field_name: string;
    payload: {
      type: 'M:1';
      target_drawer: string;
    };
  };
}

export declare const PACKAGE_NAME: '@wardrobe/client';
export declare const PACKAGE_VERSION: '0.26.724';
export declare function relationshipRequest(
  drawerName: string,
  fieldName: string,
  targetDrawer: string
): RelationshipRequest;

export class WardrobeClient {
  static open(connectionString: string): Promise<WardrobeClient>;
  close(): Promise<void>;
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
