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
  | any;

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
  status(request?: any): Promise<any>;
}
