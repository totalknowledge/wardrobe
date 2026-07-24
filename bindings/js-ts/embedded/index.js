const path = require('path');
const fs = require('fs');

const PACKAGE_NAME = '@wardrobe/embedded';
const PACKAGE_VERSION = '0.26.725';

function relationshipRequest(drawerName, fieldName, targetDrawer) {
  return {
    SchemaRule: {
      drawer_name: drawerName,
      action: 'add',
      kind: 'relationship',
      field_name: fieldName,
      payload: {
        type: 'M:1',
        target_drawer: targetDrawer
      }
    }
  };
}

// Determine native addon library name based on platform
const libName = process.platform === 'win32' ? 'wardrobe_js_ts.dll' : process.platform === 'darwin' ? 'libwardrobe_js_ts.dylib' : 'libwardrobe_js_ts.so';

// Try standard development and relative path locations
const possiblePaths = [
  path.join(__dirname, libName),
  path.join(__dirname, '../../target/release', libName),
  path.join(__dirname, '../../target/debug', libName),
  path.join(__dirname, '../target/release', libName),
  path.join(__dirname, '../target/debug', libName),
  path.join(__dirname, '../../../target/release', libName),
  path.join(__dirname, '../../../target/debug', libName)
];

let libPath = null;
for (const p of possiblePaths) {
  if (fs.existsSync(p)) {
    libPath = p;
    break;
  }
}

if (!libPath) {
  libPath = libName;
}

// Load the compiled NAPI-RS library
const addon = { exports: {} };
process.dlopen(addon, libPath);
const { executeCommand: executeCommandNative } = addon.exports;

function executeCommand(target, command) {
  const commandJson = JSON.stringify(command);
  const resultStr = executeCommandNative(target, commandJson);
  const res = JSON.parse(resultStr);
  if (res.error) {
    throw new Error(res.error);
  }
  return res;
}

class WardrobeClient {
  constructor(target) {
    this.target = target;
  }

  static async open(connectionString) {
    let pathStr = connectionString;
    if (connectionString.startsWith('wardrobe://local/')) {
      pathStr = connectionString.slice('wardrobe://local/'.length);
    } else if (connectionString.startsWith('wardrobe+file://')) {
      pathStr = connectionString.slice('wardrobe+file://'.length);
    } else if (connectionString.startsWith('file://')) {
      pathStr = connectionString.slice('file://'.length);
    }
    return new WardrobeClient(pathStr);
  }

  async upsert(payload, filter, options) {
    const res = executeCommand(this.target, {
      upsert: {
        payload,
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.upsert;
  }

  async read(filter, options) {
    const res = executeCommand(this.target, {
      read: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return unwrapReadResult(res.read);
  }

  async delete(filter, options) {
    const res = executeCommand(this.target, {
      delete: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.delete;
  }

  async inspect(filter, options) {
    const res = executeCommand(this.target, {
      inspect: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.inspect;
  }

  async count(filter, options) {
    const res = executeCommand(this.target, {
      count: {
        filter: normalizeFilter(filter),
        options: normalizeOptions(options)
      }
    });
    return res.count;
  }

  async clean(request) {
    const res = executeCommand(this.target, {
      compact: request || null
    });
    return res.compact;
  }

  async create(request) {
    const res = executeCommand(this.target, {
      create: request
    });
    return res.create;
  }

  async alter(request) {
    const res = executeCommand(this.target, {
      alter: request
    });
    return res.alter;
  }

  async drop(request) {
    const res = executeCommand(this.target, {
      drop: request
    });
    return res.drop;
  }

  async backup(sourcePath) {
    const res = executeCommand(this.target, {
      backup: { source_path: sourcePath }
    });
    return res.backup;
  }

  async restore(destinationPath, archive) {
    const res = executeCommand(this.target, {
      restore: { destination_path: destinationPath, archive }
    });
    return res.restore;
  }

  async grant(request) {
    const res = executeCommand(this.target, {
      grant: request
    });
    return res.grant;
  }

  async revoke(request) {
    const res = executeCommand(this.target, {
      revoke: request
    });
    return res.revoke;
  }

  async status(request) {
    const res = executeCommand(this.target, {
      status: request || "Storage"
    });
    return res.status;
  }
}

function unwrapReadResult(readResult) {
  if (!readResult || typeof readResult !== 'object' || Array.isArray(readResult)) {
    return readResult;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Records')) {
    return readResult.Records;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Page')) {
    return readResult.Page;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'records')) {
    return readResult.records;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'page')) {
    return readResult.page;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Record')) {
    return readResult.Record;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'record')) {
    return readResult.record;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Pointers')) {
    return readResult.Pointers;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'pointers')) {
    return readResult.pointers;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'Exists')) {
    return readResult.Exists;
  }
  if (Object.prototype.hasOwnProperty.call(readResult, 'exists')) {
    return readResult.exists;
  }
  return readResult;
}

function normalizeFilter(filter) {
  if (!filter) return "None";
  if (Array.isArray(filter)) {
    return { Many: filter.map(normalizeFilter) };
  }
  if (typeof filter === 'string') {
    if (filter.startsWith('@')) {
      if (filter.includes(':')) {
        return { Pointer: filter };
      }
      return { Drawer: filter.slice(1) };
    }
    return { Drawer: filter };
  }
  if (filter.drawer) {
    return { Drawer: filter.drawer };
  }
  if (filter.pointer) {
    return { Pointer: filter.pointer };
  }
  if (filter.query) {
    return { Query: filter.query };
  }
  return { Query: filter };
}

function normalizeOptions(options) {
  if (!options) return {};
  return {
    multi: options.multi !== undefined ? options.multi : null,
    atomic: options.atomic !== undefined ? options.atomic : null,
    create_if_missing: options.createIfMissing !== undefined ? options.createIfMissing : null,
    return_shape: options.returnShape !== undefined ? normalizeReturnShape(options.returnShape) : null,
    hydrate: options.hydrate !== undefined ? options.hydrate : null,
    limit: options.limit !== undefined ? options.limit : null,
    offset: options.offset !== undefined ? options.offset : null,
    order_by: options.orderBy !== undefined ? options.orderBy : null,
    order_direction: options.orderDirection !== undefined ? normalizeOrderDirection(options.orderDirection) : null,
    cursor: options.cursor !== undefined ? options.cursor : null,
    page: options.page !== undefined ? options.page : null,
    page_size: options.pageSize !== undefined ? options.pageSize : null,
    include_diagnostics: options.includeDiagnostics !== undefined ? options.includeDiagnostics : null
  };
}

function normalizeReturnShape(returnShape) {
  const shapes = {
    default: 'Default',
    records: 'Records',
    record: 'Record',
    pointers: 'Pointers',
    exists: 'Exists',
    diagnostics: 'Diagnostics'
  };
  return shapes[returnShape.toLowerCase()] || returnShape;
}

function normalizeOrderDirection(orderDirection) {
  const directions = {
    asc: 'Ascending',
    ascending: 'Ascending',
    desc: 'Descending',
    descending: 'Descending'
  };
  return directions[orderDirection.toLowerCase()] || orderDirection;
}

module.exports = { PACKAGE_NAME, PACKAGE_VERSION, WardrobeClient, relationshipRequest };
